use std::path::Path;

use ferrindex::extract::{Def, extract};
use ferrindex::lang::{Lang, kind_id, lang_of};

/// (名前, 種類, 行, container, body のソース)
type Row = (String, u8, u32, Option<String>, String);

fn rows(rel: &str) -> Vec<Row> {
    let path = Path::new("tests/fixtures/lang").join(rel);
    let src = std::fs::read(&path).unwrap();
    let lang = lang_of(&path).unwrap();
    extract(lang, &src)
        .into_iter()
        .map(|d| {
            let body = String::from_utf8(src[d.body.clone()].to_vec()).unwrap();
            (d.name, d.kind, d.line, d.container, body)
        })
        .collect()
}

fn row<'a>(rows: &'a [Row], name: &str) -> &'a Row {
    rows.iter()
        .find(|r| r.0 == name)
        .unwrap_or_else(|| panic!("{name} がない: {rows:?}"))
}

fn check(r: &Row, kind: &str, line: u32, container: Option<&str>, body: &str) {
    assert_eq!(r.1, kind_id(kind), "{}", r.0);
    assert_eq!(r.2, line, "{}", r.0);
    assert_eq!(r.3.as_deref(), container, "{}", r.0);
    assert_eq!(r.4, body, "{}", r.0);
}

fn defs_of(lang: Lang, src: &str) -> Vec<Def> {
    extract(lang, src.as_bytes())
}

#[test]
fn rust_defs_and_bodies() {
    let r = rows("rust/sample.rs");
    check(
        row(&r, "Point"),
        "class",
        1,
        None,
        "struct Point {\n    x: i32,\n}",
    );
    check(
        row(&r, "norm"),
        "method",
        6,
        None,
        "fn norm(&self) -> i32 {\n        self.x\n    }",
    );
    check(
        row(&r, "top"),
        "function",
        11,
        None,
        "fn top(a: i32) -> i32 {\n    a + 1\n}",
    );
}

#[test]
fn python_defs_and_bodies() {
    let r = rows("python/sample.py");
    assert_eq!(
        row(&r, "Greeter").4.lines().last(),
        Some("        return 1")
    );
    check(
        row(&r, "hello"),
        "function",
        3,
        Some("Greeter"),
        "@staticmethod\n    def hello(name):\n        return \"hi \" + name",
    );
    check(
        row(&r, "plain"),
        "function",
        6,
        Some("Greeter"),
        "def plain(self):\n        return 1",
    );
    check(
        row(&r, "top"),
        "function",
        11,
        None,
        "@decorator\ndef top(a):\n    return a",
    );
}

#[test]
fn typescript_defs_and_bodies() {
    let r = rows("typescript/sample.ts");
    check(
        row(&r, "Shape"),
        "interface",
        1,
        None,
        "interface Shape {\n  area(): number;\n}",
    );
    check(
        row(&r, "area"),
        "method",
        2,
        Some("Shape"),
        "area(): number",
    );
    check(
        row(&r, "radius"),
        "method",
        6,
        Some("Circle"),
        "radius(): number {\n    return 1;\n  }",
    );
    check(
        row(&r, "top"),
        "function",
        11,
        None,
        "function top(a: number): number {\n  return a + 1;\n}",
    );
}

#[test]
fn tsx_defs_and_bodies() {
    let r = rows("tsx/sample.tsx");
    check(
        row(&r, "render"),
        "method",
        2,
        Some("Panel"),
        "render() {\n    return <div />;\n  }",
    );
    check(
        row(&r, "top"),
        "function",
        7,
        None,
        "function top(a: number) {\n  return <span>{a}</span>;\n}",
    );
}

#[test]
fn javascript_defs_and_bodies() {
    let r = rows("javascript/sample.js");
    check(
        row(&r, "Box"),
        "class",
        1,
        None,
        "class Box {\n  size() {\n    return 1;\n  }\n}",
    );
    check(
        row(&r, "size"),
        "method",
        2,
        Some("Box"),
        "size() {\n    return 1;\n  }",
    );
    check(
        row(&r, "top"),
        "function",
        7,
        None,
        "function top(a) {\n  return a + 1;\n}",
    );
}

#[test]
fn go_defs_and_bodies() {
    let r = rows("go/sample.go");
    check(
        row(&r, "Server"),
        "type",
        3,
        None,
        "Server struct {\n\tport int\n}",
    );
    check(
        row(&r, "Start"),
        "method",
        7,
        None,
        "func (s *Server) Start() int {\n\treturn s.port\n}",
    );
    check(
        row(&r, "Top"),
        "function",
        11,
        None,
        "func Top(a int) int {\n\treturn a + 1\n}",
    );
}

#[test]
fn ruby_defs_and_bodies() {
    let r = rows("ruby/sample.rb");
    check(
        row(&r, "Worker"),
        "class",
        2,
        Some("Util"),
        "class Worker\n    def run(x)\n      x + 1\n    end\n  end",
    );
    check(
        row(&r, "run"),
        "method",
        3,
        Some("Worker"),
        "def run(x)\n      x + 1\n    end",
    );
    let util = &row(&r, "Util").4;
    assert!(
        util.starts_with("module Util\n") && util.ends_with("\nend") && !util.contains("def top")
    );
    assert_eq!(row(&r, "Util").1, kind_id("module"));
    check(row(&r, "top"), "method", 9, None, "def top(a)\n  a\nend");
}

#[test]
fn java_defs_and_bodies() {
    let r = rows("java/Sample.java");
    check(
        row(&r, "run"),
        "method",
        2,
        Some("Sample"),
        "int run(int x) {\n        return x + 1;\n    }",
    );
    check(
        row(&r, "Runner"),
        "interface",
        7,
        None,
        "interface Runner {\n    void go();\n}",
    );
    check(row(&r, "go"), "method", 8, Some("Runner"), "void go();");
}

#[test]
fn c_defs_and_bodies() {
    let r = rows("c/sample.c");
    check(row(&r, "proto"), "function", 5, None, "int proto(int a);");
    check(
        row(&r, "top"),
        "function",
        7,
        None,
        "int top(int a)\n{\n    int inner(int b);\n    return a + 1;\n}",
    );
    // 関数本体の中のプロトタイプは外側の関数に飲まれず自分の1行になる
    check(row(&r, "inner"), "function", 9, None, "int inner(int b);");
}

#[test]
fn cpp_defs_and_bodies() {
    let r = rows("cpp/sample.cpp");
    check(
        row(&r, "size"),
        "function",
        3,
        Some("Widget"),
        "int size() const {\n        return 1;\n    }",
    );
    // クラス内の宣言は外側のクラスへ広がらない
    check(
        row(&r, "decl"),
        "function",
        6,
        Some("Widget"),
        "int decl(int a);",
    );
    check(
        row(&r, "top"),
        "function",
        10,
        None,
        "template <typename T>\nT top(T a) {\n    return a;\n}",
    );
}

#[test]
fn csharp_defs_and_bodies() {
    let r = rows("csharp/Sample.cs");
    check(
        row(&r, "Worker"),
        "class",
        3,
        Some("Demo"),
        "class Worker\n    {\n        public int Run(int x)\n        {\n            return x + 1;\n        }\n    }",
    );
    check(
        row(&r, "Run"),
        "method",
        5,
        Some("Worker"),
        "public int Run(int x)\n        {\n            return x + 1;\n        }",
    );
    check(row(&r, "Go"), "method", 13, Some("IRunner"), "void Go();");
}

#[test]
fn swift_defs_and_bodies() {
    let r = rows("swift/sample.swift");
    // メソッドとプロパティはクラス全体ではなく自分の宣言だけを返す
    check(
        row(&r, "bump"),
        "method",
        4,
        Some("Counter"),
        "func bump() {\n        value += 1\n    }",
    );
    check(
        row(&r, "init"),
        "method",
        8,
        Some("Counter"),
        "init() {\n        value = 0\n    }",
    );
    check(
        row(&r, "value"),
        "property",
        2,
        Some("Counter"),
        "var value = 0",
    );
    check(row(&r, "go"), "method", 14, Some("Runner"), "func go()");
    check(
        row(&r, "top"),
        "function",
        17,
        None,
        "func top(a: Int) -> Int {\n    return a + 1\n}",
    );
    assert!(
        row(&r, "Counter").4.starts_with("class Counter {") && row(&r, "Counter").4.ends_with("}")
    );
}

#[test]
fn dart_defs_and_bodies() {
    let r = rows("dart/sample.dart");
    check(
        row(&r, "bump"),
        "method",
        4,
        Some("Counter"),
        "void bump() {\n    value += 1;\n  }",
    );
    check(
        row(&r, "top"),
        "function",
        9,
        None,
        "int top(int a) {\n  return a + 1;\n}",
    );
}

#[test]
fn lua_defs_and_bodies() {
    let r = rows("lua/sample.lua");
    check(
        row(&r, "top"),
        "function",
        3,
        None,
        "function M.top(a)\n  return a + 1\nend",
    );
    // table 全体ではなく `run = function ... end` のフィールドだけを返す
    check(
        row(&r, "run"),
        "function",
        8,
        None,
        "run = function(x)\n    return x\n  end",
    );
}

#[test]
fn markdown_defs_and_bodies() {
    let r = rows("markdown/sample.md");
    let names: Vec<_> = r.iter().map(|x| x.0.as_str()).collect();
    assert_eq!(names, ["Title", "Sub", "Next"]);
    assert_eq!(row(&r, "Title").4.lines().next(), Some("# Title"));
    assert!(row(&r, "Title").4.contains("## Sub"));
    assert!(row(&r, "Sub").4.contains("#tag"));
    assert_eq!(row(&r, "Next").4, "# Next\n");
    assert_eq!(row(&r, "Sub").1, kind_id("section"));
}

#[test]
fn markdown_atx_rules() {
    let src = "---\n# in frontmatter\n---\n   # Three\n    # Four\n#\n## closed ##\n~~~\n# in tilde\n~~~\n####### seven\n###### six\n#\ttab\n# 日本 x\n";
    let d = defs_of(Lang::Markdown, src);
    let got: Vec<_> = d.iter().map(|d| (d.name.as_str(), d.line, d.col)).collect();
    assert_eq!(
        got,
        [
            ("Three", 4, 6),
            ("closed", 7, 4),
            ("six", 12, 8),
            ("tab", 13, 3),
            ("日本 x", 14, 3)
        ]
    );
    // 空見出し `#` も節の境界になる
    assert_eq!(&src[d[0].body.clone()], "   # Three\n    # Four\n");
    // 同じか上のレベルが出るまで続く
    assert!(src[d[1].body.clone()].ends_with("###### six\n"));
}

#[test]
fn markdown_unclosed_frontmatter_and_fence() {
    let d = defs_of(Lang::Markdown, "---\n# a\n");
    assert_eq!(d.len(), 1);
    // 閉じないフェンスは終端まで続く
    assert!(defs_of(Lang::Markdown, "```\n# a\n").is_empty());
    // 閉じのフェンスは開きより短くできない
    assert!(defs_of(Lang::Markdown, "````\n```\n# a\n````\n").is_empty());
    assert_eq!(defs_of(Lang::Markdown, "```\n# a\n```\n# b\n").len(), 1);
    // バッククォートのフェンスの info string に ` があればフェンスではない
    assert_eq!(defs_of(Lang::Markdown, "``` a`b\n# a\n").len(), 1);
}

#[test]
fn column_is_utf16_and_one_based() {
    let d = defs_of(Lang::Rust, "/* 😀 */ fn f() {}\n");
    assert_eq!((d[0].line, d[0].col), (1, 13));
    let d = defs_of(Lang::Rust, "fn f() {}\n");
    assert_eq!(d[0].col, 4);
}

#[test]
fn invalid_utf8_gives_nothing() {
    assert!(extract(Lang::Rust, b"fn f() {}\n\xff").is_empty());
    assert!(extract(Lang::Markdown, b"# a\n\xff").is_empty());
}

#[test]
fn nested_container_is_innermost() {
    let d = defs_of(
        Lang::Ruby,
        "module A\n  class B\n    def m\n    end\n  end\nend\n",
    );
    let c = |n: &str| d.iter().find(|d| d.name == n).unwrap().container.clone();
    assert_eq!(
        (c("A"), c("B"), c("m")),
        (None, Some("A".into()), Some("B".into()))
    );
}

#[test]
fn every_language_extracts_something() {
    for dir in std::fs::read_dir("tests/fixtures/lang").unwrap() {
        for f in std::fs::read_dir(dir.unwrap().path()).unwrap() {
            let p = f.unwrap().path();
            let src = std::fs::read(&p).unwrap();
            assert!(
                !extract(lang_of(&p).unwrap(), &src).is_empty(),
                "{}",
                p.display()
            );
        }
    }
}

#[test]
fn c_struct_in_declaration_keeps_its_own_range() {
    let src = "struct q { int a; } v;\n";
    let d = defs_of(Lang::C, src);
    assert_eq!(&src[d[0].body.clone()], "struct q { int a; }");
}

#[test]
fn lua_function_inside_table_field_keeps_its_own_range() {
    let src = "local T = { run = function()\n  function inner() end\nend }\n";
    let d = defs_of(Lang::Lua, src);
    let inner = d.iter().find(|d| d.name == "inner").unwrap();
    assert_eq!(&src[inner.body.clone()], "function inner() end");
}

#[test]
fn swift_nested_class_keeps_its_own_range() {
    let src = "func f() {\n    class Inner {}\n}\n";
    let d = defs_of(Lang::Swift, src);
    let inner = d.iter().find(|d| d.name == "Inner").unwrap();
    assert_eq!(&src[inner.body.clone()], "class Inner {}");
}

#[test]
fn many_nested_and_flat_holders_get_the_right_container() {
    let depth = 300;
    let mut src = String::new();
    for i in 0..depth {
        src.push_str(&format!("mod m{i} {{\n"));
    }
    src.push_str("fn leaf() {}\n");
    src.push_str(&"}\n".repeat(depth));
    for i in 0..20_000 {
        src.push_str(&format!("struct S{i};\nfn f{i}() {{}}\n"));
    }
    let d = defs_of(Lang::Rust, &src);
    let c = |n: &str| d.iter().find(|d| d.name == n).unwrap().container.clone();
    assert_eq!(c("leaf"), Some(format!("m{}", depth - 1)));
    assert_eq!(c("m0"), None);
    assert_eq!(c("m17"), Some("m16".into()));
    assert_eq!(c("f19999"), None);
    assert_eq!(d.len(), depth + 1 + 40_000);
}
