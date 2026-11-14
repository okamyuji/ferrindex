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
    // 括弧を除いて target の 4 段上の call は見ない
    let near = targets(Lang::JavaScript, b"(target || a || b)();\n");
    assert_eq!(near[0].kind, REF_CALL);
    let far = targets(Lang::JavaScript, b"(target || a || b || c)();\n");
    assert_eq!(far[0].kind, REF_PLAIN);
}

#[test]
fn parentheses_around_the_callee_keep_it_a_call() {
    let cases = [
        (Lang::JavaScript, "(((target)))();\n"),
        (Lang::TypeScript, "(((target)))();\n"),
        (Lang::Python, "(((target)))()\n"),
        (Lang::Go, "package a\nfunc g() { (((target)))() }\n"),
        (Lang::Rust, "fn g() { (((target)))(); }\n"),
        (Lang::C, "void g() { (((target)))(); }\n"),
        (Lang::Cpp, "void g() { (((target)))(); }\n"),
        (Lang::Lua, "(((target)))()\n"),
        (Lang::Dart, "void g() { (((target)))(); }\n"),
    ];
    for (lang, src) in cases {
        let found = targets(lang, src.as_bytes());
        assert_eq!(found.len(), 1, "{lang:?}");
        assert_eq!(found[0].kind, REF_CALL, "{lang:?}");
    }
}

#[test]
fn parentheses_do_not_turn_receivers_or_arguments_into_calls() {
    let js = kinds(
        Lang::JavaScript,
        "function g() { (obj).method(); f((target)); if ((cond)) {} }\n",
    );
    for (name, kind) in [
        ("obj", REF_PLAIN),
        ("method", REF_CALL),
        ("f", REF_CALL),
        ("target", REF_PLAIN),
        ("cond", REF_PLAIN),
    ] {
        assert!(js.contains(&(name.into(), kind)), "{name} {js:?}");
    }
    let java = kinds(Lang::Java, "class A { void g() { (obj).method(); } }\n");
    assert!(java.contains(&("obj".into(), REF_PLAIN)), "{java:?}");
}

#[test]
fn parentheses_are_skipped_only_up_to_a_bound() {
    // 16 段までは数えずに飛ばし、残りは分類の 3 段に数える
    let kind = |n: usize| {
        let src = format!("{}target{}();\n", "(".repeat(n), ")".repeat(n));
        targets(Lang::JavaScript, src.as_bytes())[0].kind
    };
    assert_eq!(kind(18), REF_CALL);
    assert_eq!(kind(19), REF_PLAIN);
    assert_eq!(kind(1000), REF_PLAIN);
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
        (
            Lang::Java,
            "class A { void g() { obj.method(); a.b.c(); } }\n",
        ),
        (Lang::Lua, "function g() obj.method() a.b.c() end\n"),
        (Lang::Lua, "function g() obj:method() a.b:c() end\n"),
        (
            Lang::CSharp,
            "class A { void g() { obj.method(); a.b.c(); } }\n",
        ),
        (
            Lang::TypeScript,
            "function g() { obj.method(); a.b.c(); }\n",
        ),
        (Lang::Swift, "func g() { obj.method(); a.b.c() }\n"),
        (Lang::Dart, "void g() { obj.method(); a.b.c(); }\n"),
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

#[test]
fn java_receivers_behind_this_are_plain() {
    let k = kinds(Lang::Java, "class A { void g() { this.x.baz(); } }\n");
    assert!(k.contains(&("x".into(), REF_PLAIN)), "{k:?}");
    assert!(k.contains(&("baz".into(), REF_CALL)), "{k:?}");
}

#[test]
fn rust_supertraits_are_inherit_references_but_generic_bounds_are_not() {
    let src = "trait Base {}\n\
               trait Child: Base + a::Other + b::c::Deep<T> {}\n\
               fn g<T: Base>(t: T) where T: Base {}\n\
               impl<T: Base> X for T {}\n\
               trait H: Fn(Req) -> Resp + a::FnMut() -> b::Out {}\n";
    let refs = refs_of(Lang::Rust, src.as_bytes());
    let inherit: Vec<(&str, u32, Option<&str>)> = refs
        .iter()
        .filter(|r| r.kind == REF_INHERIT)
        .map(|r| (r.name.as_str(), r.line, r.container.as_deref()))
        .collect();
    assert_eq!(
        inherit,
        [
            ("Base", 2, Some("Child")),
            ("Other", 2, Some("Child")),
            ("Deep", 2, Some("Child")),
            ("X", 4, Some("T")),
            ("Fn", 5, Some("H")),
            ("FnMut", 5, Some("H")),
        ]
    );
}

#[test]
fn rust_where_self_bounds_on_a_trait_are_supertraits() {
    let src = "trait C where Self: Base + a::Other + b::Deep<T> {}\n\
               trait D<T> where T: NotT, Self::Item: NotItem {}\n\
               impl X for Y where Self: NotImpl {}\n\
               trait F { fn m() where Self: NotMethod; }\n\
               trait H where Self: Fn(Req) -> Resp {}\n\
               fn g<T>() where Self: NotFn {}\n\
               trait K where T: NotFirst, Self: Last {}\n";
    let refs = refs_of(Lang::Rust, src.as_bytes());
    let inherit: Vec<(&str, u32, Option<&str>)> = refs
        .iter()
        .filter(|r| r.kind == REF_INHERIT)
        .map(|r| (r.name.as_str(), r.line, r.container.as_deref()))
        .collect();
    assert_eq!(
        inherit,
        [
            ("Base", 1, Some("C")),
            ("Other", 1, Some("C")),
            ("Deep", 1, Some("C")),
            ("X", 3, Some("Y")),
            ("Fn", 5, Some("H")),
            ("Last", 7, Some("K")),
        ]
    );
}

#[test]
fn columns_follow_multibyte_text_across_lines() {
    let src = "fn f() {\n    \"é😀\"; a(); b();\n    c(); \"😀\"; d();\n}\n";
    let refs = refs_of(Lang::Rust, src.as_bytes());
    for r in &refs {
        let line = src.lines().nth(r.line as usize - 1).unwrap();
        let byte = line.find(&format!("{}(", r.name)).unwrap();
        let want = line[..byte].encode_utf16().count() as u32 + 1;
        assert_eq!(r.col, want, "{}", r.name);
    }
    assert_eq!(refs.len(), 4);
}

/// 1 行に並ぶ参照の数を倍にしても、時間はほぼ倍に収まる（二乗なら 4 倍）。
#[test]
fn many_references_on_one_line_take_linear_time() {
    let time = |n: usize| {
        let src = format!("function x(){{}}{}", "x();".repeat(n));
        let start = std::time::Instant::now();
        let found = refs_of(Lang::JavaScript, src.as_bytes());
        assert_eq!(found.len(), n);
        start.elapsed()
    };
    time(1000);
    // 機械の負荷の揺れで偽の失敗を出さないよう、3回の最小値で比べる
    let best = |n| (0..3).map(|_| time(n)).min().unwrap();
    let small = best(4000);
    let large = best(16000);
    // 線形なら 4 倍、二乗なら 16 倍。揺れを見込んで 8 倍で切る
    assert!(large < small * 8, "{small:?} -> {large:?}");
}

#[test]
fn rust_lifetime_bounds_are_not_supertraits() {
    let src = "trait C: 'static + Base {}\ntrait D where Self: 'a {}\n";
    let refs = refs_of(Lang::Rust, src.as_bytes());
    let inherit: Vec<&str> = refs
        .iter()
        .filter(|r| r.kind == REF_INHERIT)
        .map(|r| r.name.as_str())
        .collect();
    assert_eq!(inherit, ["Base"]);
}

#[test]
fn macro_arguments_are_calls_only_before_parentheses_or_a_macro_bang() {
    let src = "fn f() { let s = stringify!(target); println!(\"{}\", make()); \
               m!(spaced (), a.b(c), x::y(z), q[i], v{w}, n != o, \
               inner!(p), sq![r], br!{t}, vec![g(y)]); }\n";
    let found = kinds(Lang::Rust, src);
    let want: Vec<(String, u8)> = [
        ("s", REF_PLAIN),
        ("stringify", REF_CALL),
        ("target", REF_PLAIN),
        ("println", REF_CALL),
        ("make", REF_CALL),
        ("m", REF_CALL),
        ("spaced", REF_CALL),
        ("a", REF_PLAIN),
        ("b", REF_CALL),
        ("c", REF_PLAIN),
        ("x", REF_PLAIN),
        ("y", REF_CALL),
        ("z", REF_PLAIN),
        ("q", REF_PLAIN),
        ("i", REF_PLAIN),
        ("v", REF_PLAIN),
        ("w", REF_PLAIN),
        ("n", REF_PLAIN),
        ("o", REF_PLAIN),
        ("inner", REF_CALL),
        ("p", REF_PLAIN),
        ("sq", REF_CALL),
        ("r", REF_PLAIN),
        ("br", REF_CALL),
        ("t", REF_PLAIN),
        ("vec", REF_CALL),
        ("g", REF_CALL),
        ("y", REF_PLAIN),
    ]
    .into_iter()
    .map(|(n, k)| (n.to_string(), k))
    .collect();
    assert_eq!(found, want);
}

#[test]
fn calls_inside_attributes_stay_plain() {
    let found = kinds(Lang::Rust, "#[cfg(any(unix))]\nfn f() {}\n");
    assert!(found.iter().all(|(_, k)| *k == REF_PLAIN), "{found:?}");
    assert_eq!(found.len(), 3, "{found:?}");
}
