use ferrindex::extract::extract;
use ferrindex::lang::Lang;
use ferrindex::refs::{REF_CALL, REF_INHERIT, REF_PLAIN, Ref, extract_refs};

struct Case {
    lang: Lang,
    path: &'static str,
    call_line: u32,
    inherit: &'static [(u32, u8)],
}

const fn case(
    lang: Lang,
    path: &'static str,
    call_line: u32,
    inherit: &'static [(u32, u8)],
) -> Case {
    Case {
        lang,
        path,
        call_line,
        inherit,
    }
}

const CASES: [Case; 14] = [
    case(Lang::Rust, "rust/main.rs", 6, &[]),
    case(Lang::Go, "go/main.go", 8, &[]),
    case(Lang::Python, "python/main.py", 9, &[(4, REF_INHERIT)]),
    case(
        Lang::JavaScript,
        "javascript/main.js",
        8,
        &[(4, REF_INHERIT)],
    ),
    case(
        Lang::TypeScript,
        "typescript/main.ts",
        8,
        &[(4, REF_INHERIT)],
    ),
    case(Lang::Tsx, "tsx/main.tsx", 8, &[(4, REF_INHERIT)]),
    case(Lang::Java, "java/Main.java", 7, &[(4, REF_INHERIT)]),
    case(Lang::CSharp, "csharp/Main.cs", 7, &[(4, REF_INHERIT)]),
    case(Lang::Ruby, "ruby/main.rb", 9, &[(4, REF_INHERIT)]),
    case(Lang::C, "c/main.c", 6, &[]),
    case(Lang::Cpp, "cpp/main.cpp", 8, &[(4, REF_INHERIT)]),
    case(Lang::Swift, "swift/main.swift", 8, &[(4, REF_INHERIT)]),
    case(Lang::Lua, "lua/main.lua", 6, &[]),
    case(
        Lang::Dart,
        "dart/main.dart",
        8,
        &[(4, REF_INHERIT), (11, REF_INHERIT)],
    ),
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
            .iter()
            .map(|&(l, _)| l)
            .chain([c.call_line])
            .collect();
        want.sort();
        assert_eq!(lines, want, "{:?}", c.lang);
        let call = found.iter().find(|r| r.line == c.call_line).unwrap();
        assert_eq!(call.kind, REF_CALL, "{:?}", c.lang);
        for &(l, kind) in c.inherit {
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
    // target の 4 段上の call は見ない
    let near = targets(Lang::JavaScript, b"((target))();\n");
    assert_eq!(near[0].kind, REF_CALL);
    let far = targets(Lang::JavaScript, b"(((target)))();\n");
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

fn kinds(lang: Lang, src: &str) -> Vec<(String, u8)> {
    refs_of(lang, src.as_bytes())
        .into_iter()
        .map(|r| (r.name, r.kind))
        .collect()
}

#[test]
fn call_arguments_are_plain_references() {
    let cases = [
        (Lang::Rust, "fn g() { f(x); o.method(x); }\n"),
        (Lang::Python, "def g():\n    f(x)\n    o.method(x)\n"),
        (Lang::Java, "class A { void g() { f(x); o.method(x); } }\n"),
    ];
    for (lang, src) in cases {
        let k = kinds(lang, src);
        let kind_of = |n: &str, nth: usize| k.iter().filter(|(m, _)| m == n).nth(nth).unwrap().1;
        assert_eq!(kind_of("f", 0), REF_CALL, "{lang:?}");
        assert_eq!(kind_of("x", 0), REF_PLAIN, "{lang:?}");
        assert_eq!(kind_of("method", 0), REF_CALL, "{lang:?}");
        assert_eq!(kind_of("x", 1), REF_PLAIN, "{lang:?}");
    }
}

#[test]
fn base_classes_are_inherit_references() {
    let py = kinds(Lang::Python, "class B(A):\n    pass\n");
    assert!(py.contains(&("A".into(), REF_INHERIT)));
    let cpp = kinds(Lang::Cpp, "class B : public A {};\n");
    assert!(cpp.contains(&("A".into(), REF_INHERIT)));
}

#[test]
fn dart_implements_and_mixins_are_inherit_references() {
    let k = kinds(
        Lang::Dart,
        "class A extends Base with Mix implements Iface {}\n",
    );
    assert!(k.contains(&("Iface".into(), REF_INHERIT)));
}

#[test]
fn receiver_is_plain_and_member_name_is_call() {
    let cases = [
        (Lang::Go, "func g() {\n\tobj.method()\n\ta.b.c()\n}\n"),
        (Lang::Python, "def g():\n    obj.method()\n    a.b.c()\n"),
        (
            Lang::JavaScript,
            "function g() { obj.method(); a.b.c(); }\n",
        ),
        (Lang::Cpp, "void g() { obj.method(); a.b.c(); p->m(); }\n"),
        (Lang::Ruby, "def g\n  obj.method()\n  a.b.c()\nend\n"),
        (Lang::Rust, "fn g() { obj.method(); a.b.c(); m::f(); }\n"),
    ];
    for (lang, src) in cases {
        let k = kinds(lang, src);
        let kind_of = |n: &str| k.iter().find(|(m, _)| m == n).unwrap().1;
        assert_eq!(kind_of("obj"), REF_PLAIN, "{lang:?} obj");
        assert_eq!(kind_of("method"), REF_CALL, "{lang:?} method");
        assert_eq!(kind_of("a"), REF_PLAIN, "{lang:?} a");
        // Ruby の `.b` はメソッド呼び出しそのもの
        let b = if lang == Lang::Ruby {
            REF_CALL
        } else {
            REF_PLAIN
        };
        assert_eq!(kind_of("b"), b, "{lang:?} b");
        assert_eq!(kind_of("c"), REF_CALL, "{lang:?} c");
    }
}

#[test]
fn qualified_call_keeps_the_function_name_as_call() {
    let k = kinds(Lang::Rust, "fn g() { m::f(); }\n");
    assert!(k.contains(&("f".into(), REF_CALL)));
    assert!(k.contains(&("m".into(), REF_PLAIN)));
}

#[test]
fn dart_local_function_is_a_definition_and_a_container() {
    let src = b"void m() { void nested() { deep(q); } }\n";
    let defs = extract(Lang::Dart, src);
    assert!(defs.iter().any(|d| d.name == "nested"));
    let refs = extract_refs(Lang::Dart, src, &defs);
    assert!(!refs.iter().any(|r| r.name == "nested"));
    let deep = refs.iter().find(|r| r.name == "deep").unwrap();
    assert_eq!(deep.container.as_deref(), Some("nested"));
}

#[test]
fn ruby_method_names_in_chains_are_calls() {
    for src in [
        "def g\n  a.b().c()\nend\n",
        "def g\n  a.b.c()\nend\n",
        "def g\n  a.b.c\nend\n",
    ] {
        let k = kinds(Lang::Ruby, src);
        let kind_of = |n: &str| k.iter().find(|(m, _)| m == n).unwrap().1;
        assert_eq!(kind_of("a"), REF_PLAIN, "{src}");
        assert_eq!(kind_of("b"), REF_CALL, "{src}");
        assert_eq!(kind_of("c"), REF_CALL, "{src}");
    }
}

#[test]
fn identifiers_inside_blocks_and_callbacks_are_not_calls() {
    let cases = [
        (
            Lang::Ruby,
            "def g(a)\n  a.each do |follow|\n    follow.save\n  end\nend\n",
            [
                ("follow", REF_PLAIN),
                ("save", REF_CALL),
                ("each", REF_CALL),
            ],
        ),
        (
            Lang::JavaScript,
            "function g(arr) { arr.map(x => x.y()); }\n",
            [("x", REF_PLAIN), ("y", REF_CALL), ("map", REF_CALL)],
        ),
        (
            Lang::Python,
            "def g(xs):\n    list(map(lambda v: v.strip(), xs))\n",
            [("v", REF_PLAIN), ("strip", REF_CALL), ("map", REF_CALL)],
        ),
    ];
    for (lang, src, want) in cases {
        let k = kinds(lang, src);
        for (name, kind) in want {
            let all: Vec<u8> = k
                .iter()
                .filter(|(m, _)| m == name)
                .map(|(_, c)| *c)
                .collect();
            assert!(
                !all.is_empty() && all.iter().all(|&c| c == kind),
                "{lang:?} {name} {all:?}"
            );
        }
    }
}
