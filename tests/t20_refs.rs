use ferrindex::extract::extract;
use ferrindex::lang::Lang;
use ferrindex::refs::{REF_CALL, REF_INHERIT, REF_PLAIN, Ref, extract_refs};

struct Case {
    lang: Lang,
    path: &'static str,
    call_line: u32,
    inherit: Option<(u32, u8)>,
}

const fn case(lang: Lang, path: &'static str, call_line: u32, inherit: Option<(u32, u8)>) -> Case {
    Case {
        lang,
        path,
        call_line,
        inherit,
    }
}

const CASES: [Case; 14] = [
    case(Lang::Rust, "rust/main.rs", 6, None),
    case(Lang::Go, "go/main.go", 8, None),
    case(Lang::Python, "python/main.py", 9, Some((4, REF_PLAIN))),
    case(
        Lang::JavaScript,
        "javascript/main.js",
        8,
        Some((4, REF_INHERIT)),
    ),
    case(
        Lang::TypeScript,
        "typescript/main.ts",
        8,
        Some((4, REF_INHERIT)),
    ),
    case(Lang::Tsx, "tsx/main.tsx", 8, Some((4, REF_INHERIT))),
    case(Lang::Java, "java/Main.java", 7, Some((4, REF_INHERIT))),
    case(Lang::CSharp, "csharp/Main.cs", 7, Some((4, REF_INHERIT))),
    case(Lang::Ruby, "ruby/main.rb", 9, Some((4, REF_INHERIT))),
    case(Lang::C, "c/main.c", 6, None),
    case(Lang::Cpp, "cpp/main.cpp", 8, Some((4, REF_PLAIN))),
    case(Lang::Swift, "swift/main.swift", 8, Some((4, REF_INHERIT))),
    case(Lang::Lua, "lua/main.lua", 6, None),
    case(Lang::Dart, "dart/main.dart", 8, Some((4, REF_INHERIT))),
];

fn refs_of(lang: Lang, src: &[u8]) -> Vec<Ref> {
    extract_refs(lang, src, &extract(lang, src))
}

fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/refs/{path}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn targets(lang: Lang, src: &[u8]) -> Vec<Ref> {
    refs_of(lang, src)
        .into_iter()
        .filter(|r| r.name == "target")
        .collect()
}

#[test]
fn fixtures_cover_every_indexed_language() {
    let covered: Vec<Lang> = CASES.iter().map(|c| c.lang).collect();
    for lang in Lang::ALL {
        assert!(
            lang == Lang::Markdown || covered.contains(&lang),
            "{lang:?}"
        );
    }
}

#[test]
fn each_language_finds_calls_but_not_comments_or_strings() {
    for c in &CASES {
        let found = targets(c.lang, &fixture(c.path));
        let mut lines: Vec<u32> = found.iter().map(|r| r.line).collect();
        lines.sort();
        let mut want: Vec<u32> = c
            .inherit
            .map(|(l, _)| l)
            .into_iter()
            .chain([c.call_line])
            .collect();
        want.sort();
        assert_eq!(lines, want, "{:?}", c.lang);
        let call = found.iter().find(|r| r.line == c.call_line).unwrap();
        assert_eq!(call.kind, REF_CALL, "{:?}", c.lang);
        if let Some((l, kind)) = c.inherit {
            let inherit = found.iter().find(|r| r.line == l).unwrap();
            assert_eq!(inherit.kind, kind, "{:?}", c.lang);
        }
    }
}

#[test]
fn definition_name_is_not_a_reference() {
    for c in &CASES {
        let src = fixture(c.path);
        let defs = extract(c.lang, &src);
        let refs = extract_refs(c.lang, &src, &defs);
        for d in &defs {
            assert!(
                !refs.iter().any(|r| r.line == d.line && r.col == d.col),
                "{:?} {}",
                c.lang,
                d.name
            );
        }
    }
}

#[test]
fn container_is_the_enclosing_definition() {
    for c in &CASES {
        let found = targets(c.lang, &fixture(c.path));
        let call = found.iter().find(|r| r.line == c.call_line).unwrap();
        assert_eq!(call.container.as_deref(), Some("caller"), "{:?}", c.lang);
    }
}

#[test]
fn reference_outside_any_definition_has_no_container() {
    let src = b"fn f() {}\nstatic X: u8 = g();\nfn h() { f(); }\n";
    let refs = refs_of(Lang::Rust, src);
    let g = refs.iter().find(|r| r.name == "g").unwrap();
    assert_eq!((g.container.as_deref(), g.kind), (None, REF_CALL));
    let f = refs.iter().find(|r| r.name == "f" && r.line == 3).unwrap();
    assert_eq!(f.container.as_deref(), Some("h"));
}

#[test]
fn container_is_the_innermost_definition() {
    let src = b"struct S;\nimpl S {\n    fn outer() {\n        fn inner() { used(); }\n        later();\n    }\n}\n";
    let refs = refs_of(Lang::Rust, src);
    let used = refs.iter().find(|r| r.name == "used").unwrap();
    assert_eq!(used.container.as_deref(), Some("inner"));
    let later = refs.iter().find(|r| r.name == "later").unwrap();
    assert_eq!(later.container.as_deref(), Some("outer"));
}

#[test]
fn column_counts_utf16_units() {
    let src = "fn f() { let _s = \"é😀\"; target(); }\n".as_bytes();
    let found = targets(Lang::Rust, src);
    let want = "fn f() { let _s = \"é😀\"; ".encode_utf16().count() as u32 + 1;
    assert_eq!((found[0].line, found[0].col), (1, want));
}

#[test]
fn non_call_reference_is_plain() {
    let found = targets(Lang::Rust, b"fn f() { let x = target; }\n");
    assert_eq!(found[0].kind, REF_PLAIN);
}

#[test]
fn classification_looks_at_most_three_ancestors_up() {
    // x -> parenthesized -> parenthesized -> parenthesized -> call: 4 段上の call は見ない
    let near = targets(Lang::JavaScript, b"f((target));\n");
    assert_eq!(near[0].kind, REF_CALL);
    let far = targets(Lang::JavaScript, b"f((((target))));\n");
    assert_eq!(far[0].kind, REF_PLAIN);
}

#[test]
fn ruby_constants_and_swift_identifiers_are_references() {
    let rb = refs_of(Lang::Ruby, b"def f\n  Target.run\nend\n");
    assert!(rb.iter().any(|r| r.name == "Target"));
    let sw = refs_of(Lang::Swift, b"func f() { target() }\n");
    assert!(sw.iter().any(|r| r.name == "target"));
}

#[test]
fn invalid_utf8_and_markdown_give_nothing() {
    assert!(extract_refs(Lang::Rust, b"fn f() { \xff }", &[]).is_empty());
    assert!(extract_refs(Lang::Markdown, b"# target\n", &[]).is_empty());
}

#[test]
fn deep_nesting_does_not_overflow() {
    let n = 100_000;
    let src = format!("{}target{};\n", "(".repeat(n), ")".repeat(n));
    let found = std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(move || targets(Lang::JavaScript, src.as_bytes()))
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(found.len(), 1);
}
