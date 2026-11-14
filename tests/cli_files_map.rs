mod common;
use std::path::Path;

use common::Repo;

#[test]
fn files_matches_substring_ignoring_ascii_case_in_path_order() {
    let r = Repo::new();
    r.write("src/Main.rs", "fn m() {}\n");
    r.write("src/util.rs", "fn u() {}\n");
    r.write("README.md", "# t\n");
    r.write("data/x.bin", "x");
    let run = r.run(&["files", "MAIN"]);
    assert_eq!(run.lines(), ["src/Main.rs"]);
    let run = r.run(&["files", ".rs"]);
    assert_eq!(run.lines(), ["src/Main.rs", "src/util.rs"]);
    let run = r.run(&["files", "x.bin"]);
    assert_eq!(run.lines(), ["data/x.bin"]);
}

#[test]
fn files_limit_and_cwd_relative() {
    let r = Repo::new();
    for n in ["a", "b", "c"] {
        r.write(&format!("sub/{n}.rs"), "fn f() {}\n");
    }
    r.write("z.rs", "fn z() {}\n");
    let run = r.run_in(Path::new("sub"), &["files", ".rs", "--limit", "3"]);
    assert_eq!(run.lines(), ["a.rs", "b.rs", "c.rs"]);
    assert_eq!(run.err, "… 1 more\n");
    assert_eq!(r.run(&["files", "zzz"]).code, 1);
}

#[test]
fn files_does_not_list_deleted_files() {
    let r = Repo::new();
    for i in 0..30 {
        r.write(&format!("f{i}.rs"), "fn f() {}\n");
    }
    r.age();
    r.run(&["update"]);
    std::fs::remove_file(r.root.join("f3.rs")).unwrap();
    let run = r.run(&["files", "f3"]);
    assert_eq!(run.code, 1);
}

#[test]
fn map_prints_path_tokens_and_description() {
    let r = Repo::new();
    r.write(
        "a.rs",
        &format!("//! Entry point.\nfn main() {{}}\n{}", "x".repeat(70)),
    );
    r.write(
        "docs/guide.md",
        &format!("# The Guide\n\n{}", "y".repeat(100)),
    );
    r.write("conf.toml", &format!("# settings\n{}", "z".repeat(100)));
    r.write("plain.txt", &"p".repeat(40));
    let run = r.run(&["map"]);
    assert_eq!(run.code, 0, "{}", run.err);
    // コード 3.5 字/token、その他 4.0 字/token（小数は切り捨て）
    assert_eq!(
        run.lines(),
        [
            "a.rs  ~28  Entry point.",
            "conf.toml  ~27  settings",
            "docs/guide.md  ~28  The Guide",
            "plain.txt  ~10  "
        ]
    );
}

#[test]
fn map_counts_chars_not_bytes() {
    let r = Repo::new();
    r.write("a.rs", &"あ".repeat(70));
    r.write("b.md", &"あ".repeat(40));
    let run = r.run(&["map"]);
    assert_eq!(run.lines(), ["a.rs  ~20  ", "b.md  ~10  "]);
}

#[test]
fn map_description_is_the_first_comment_in_the_first_20_lines() {
    let r = Repo::new();
    let late = format!("{}// too late\nfn f() {{}}\n", "let a = 1;\n".repeat(20));
    r.write("late.rs", &late);
    let edge = format!("{}// on line 20\n", "let a = 1;\n".repeat(19));
    r.write("edge.rs", &edge);
    r.write(
        "py.py",
        "#!/usr/bin/env python\n# python note\ndef f(): pass\n",
    );
    r.write("lua.lua", "-- lua note\nlocal x = 1\n");
    r.write("doc.rs", "/// outer doc\nfn f() {}\n");
    r.write("blank.rs", "//\n//   \n// after blanks\n");
    r.write("code.c", "#include <x.h>\nint f() { return 0; }\n");
    let out = r.run(&["map"]);
    let find = |p: &str| {
        out.lines()
            .into_iter()
            .find(|l| l.starts_with(p))
            .unwrap()
            .split("  ")
            .nth(2)
            .unwrap()
            .to_string()
    };
    assert_eq!(find("late.rs"), "");
    assert_eq!(find("edge.rs"), "on line 20");
    assert_eq!(find("py.py"), "python note");
    assert_eq!(find("lua.lua"), "lua note");
    assert_eq!(find("doc.rs"), "outer doc");
    assert_eq!(find("blank.rs"), "after blanks");
    assert_eq!(find("code.c"), "");
}

#[test]
fn map_skips_binary_files_and_takes_a_directory() {
    let r = Repo::new();
    r.write("src/a.rs", "// in src\nfn a() {}\n");
    r.write("src/deep/b.rs", "fn b() {}\n");
    r.write("other.rs", "fn o() {}\n");
    std::fs::write(r.root.join("src/blob.png"), b"\x89PNG\0\0\0data").unwrap();
    let run = r.run(&["map", "src"]);
    assert_eq!(run.lines(), ["src/a.rs  ~5  in src", "src/deep/b.rs  ~2  "]);
    let from_sub = r.run_in(Path::new("src"), &["map"]);
    assert_eq!(from_sub.lines(), ["a.rs  ~5  in src", "deep/b.rs  ~2  "]);
    let nested = r.run_in(Path::new("src"), &["map", "deep"]);
    assert_eq!(nested.lines(), ["deep/b.rs  ~2  "]);
}

#[test]
fn map_limit_and_errors() {
    let r = Repo::new();
    for n in ["a", "b", "c"] {
        r.write(&format!("{n}.rs"), "fn f() {}\n");
    }
    let run = r.run(&["map", "--limit", "2"]);
    assert_eq!(run.lines().len(), 2);
    assert_eq!(run.err, "… 1 more\n");
    let missing = r.run(&["map", "nope"]);
    assert_eq!(missing.code, 4);
    assert_eq!(missing.err.lines().count(), 1, "{}", missing.err);
    assert!(missing.err.contains("nope"), "{}", missing.err);
    r.write("empty/.keep", "");
    std::fs::write(r.root.join("file.txt"), "x").unwrap();
    assert_eq!(r.run(&["map", "file.txt"]).code, 2);
    let outside = r.run(&["map", ".."]);
    assert_eq!(outside.code, 2, "{}", outside.err);
    assert!(outside.err.contains("outside"), "{}", outside.err);
}

#[test]
fn map_default_limit_is_200() {
    let r = Repo::new();
    for i in 0..205 {
        r.write(&format!("f{i:03}.rs"), "fn f() {}\n");
    }
    let run = r.run(&["map"]);
    assert_eq!(run.lines().len(), 200);
    assert_eq!(run.err, "… 5 more\n");
}

#[test]
fn files_and_map_print_paths_outside_cwd_with_dotdot() {
    let r = Repo::new();
    r.write("sub/a.rs", "// in sub\nfn a() {}\n");
    r.write("top.rs", "// at top\nfn t() {}\n");
    r.write("other/o.rs", "fn o() {}\n");
    let files = r.run_in(Path::new("sub"), &["files", ".rs"]);
    assert_eq!(files.lines(), ["../other/o.rs", "a.rs", "../top.rs"]);
    let map = r.run_in(Path::new("sub"), &["map", ".."]);
    assert_eq!(
        map.lines(),
        [
            "../other/o.rs  ~2  ",
            "a.rs  ~5  in sub",
            "../top.rs  ~5  at top"
        ]
    );
    let sibling = r.run_in(Path::new("sub"), &["map", "../other"]);
    assert_eq!(sibling.lines(), ["../other/o.rs  ~2  "]);
}

#[test]
fn map_description_for_python_is_the_module_docstring() {
    let r = Repo::new();
    r.write("doc.py", "#!/usr/bin/env python\n# -*- coding: utf-8 -*-\n\"\"\"Tool for x.\n\nmore\n\"\"\"\n# later\n");
    r.write("nodoc.py", "# plain comment\nx = 1\n");
    let run = r.run(&["map"]);
    let lines = run.lines();
    assert!(lines[0].ends_with("  Tool for x."), "{lines:?}");
    assert!(lines[1].ends_with("  plain comment"), "{lines:?}");
}
