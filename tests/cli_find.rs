mod common;
use std::path::Path;

use common::{Repo, SAMPLE};

#[test]
fn subdir_relative_paths() {
    let r = Repo::new();
    r.write("top.rs", "fn target() {}\n");
    r.write("sub/in.rs", "fn target() {}\n");
    r.write("other/out.rs", "fn target() {}\n");
    let from_root = r.run(&["find", "target"]);
    assert_eq!(
        from_root.lines(),
        [
            "other/out.rs:1:4 function target",
            "sub/in.rs:1:4 function target",
            "top.rs:1:4 function target"
        ]
    );
    let from_sub = r.run_in(Path::new("sub"), &["find", "target"]);
    assert_eq!(
        from_sub.lines(),
        [
            "other/out.rs:1:4 function target",
            "in.rs:1:4 function target",
            "top.rs:1:4 function target"
        ]
    );
}

#[test]
fn deterministic_order_and_limit() {
    let r = Repo::new();
    r.write("b.rs", "fn dup() {}\nfn dup() {}\n");
    r.write("a.rs", "fn dup() {}\n");
    r.write("c.rs", "fn dup() {}\n");
    let all = r.run(&["find", "dup"]);
    assert_eq!(
        all.lines(),
        [
            "a.rs:1:4 function dup",
            "b.rs:1:4 function dup",
            "b.rs:2:4 function dup",
            "c.rs:1:4 function dup"
        ]
    );
    assert_eq!(all.err, "");
    let cut = r.run(&["find", "dup", "--limit", "2"]);
    assert_eq!(
        cut.lines(),
        ["a.rs:1:4 function dup", "b.rs:1:4 function dup"]
    );
    assert_eq!(cut.err, "… 2 more\n");
    let exact = r.run(&["find", "dup", "--limit", "4"]);
    assert_eq!(exact.lines().len(), 4);
    assert_eq!(exact.err, "");
}

#[test]
fn default_limit_is_20() {
    let r = Repo::new();
    for i in 0..25 {
        r.write(&format!("f{i:02}.rs"), "fn many() {}\n");
    }
    let run = r.run(&["find", "many"]);
    assert_eq!(run.lines().len(), 20);
    assert_eq!(run.err, "… 5 more\n");
}

#[test]
fn exact_match_beats_substring() {
    let r = Repo::new();
    r.write("a.rs", "fn open() {}\nfn reopen_all() {}\n");
    assert_eq!(r.run(&["find", "open"]).lines(), ["a.rs:1:4 function open"]);
}

#[test]
fn zero_exact_falls_back_to_case_insensitive_substring() {
    let r = Repo::new();
    r.write(
        "a.rs",
        "fn reopen_all() {}\nfn OpenSesame() {}\nfn unrelated() {}\n",
    );
    let run = r.run(&["find", "OPEN"]);
    assert_eq!(
        run.lines(),
        [
            "a.rs:1:4 function reopen_all",
            "a.rs:2:4 function OpenSesame"
        ]
    );
}

#[test]
fn substring_fallback_respects_the_container() {
    let r = Repo::new();
    r.write("a.rs", SAMPLE);
    assert_eq!(
        r.run(&["find", "Foo.BA"]).lines(),
        ["a.rs:3:8 method Foo.bar"]
    );
    assert_eq!(r.run(&["find", "Nope.BA"]).code, 1);
}

#[test]
fn substring_fallback_is_capped_at_200_names() {
    let r = Repo::new();
    let text: String = (0..205).map(|i| format!("fn zz{i:03}() {{}}\n")).collect();
    r.write("a.rs", &text);
    let run = r.run(&["find", "ZZ", "--limit", "1000"]);
    assert_eq!(run.lines().len(), 200);
    assert_eq!(run.lines()[0], "a.rs:1:4 function zz000");
    assert_eq!(run.lines()[199], "a.rs:200:4 function zz199");
}

#[test]
fn search_sees_edits_made_after_the_index_exists() {
    let r = Repo::new();
    r.write("a.rs", "fn before() {}\n");
    assert_eq!(r.run(&["find", "before"]).code, 0);
    r.write("a.rs", "fn after_edit() {}\n\n\n");
    assert_eq!(r.run(&["find", "before"]).code, 1);
    assert_eq!(
        r.run(&["find", "after_edit"]).lines(),
        ["a.rs:1:4 function after_edit"]
    );
}

#[test]
fn outline_lists_one_file_without_an_index() {
    let r = Repo::new();
    r.write("a.rs", SAMPLE);
    let run = r.run(&["outline", "a.rs"]);
    assert_eq!(
        run.lines(),
        [
            "a.rs:1:8 class Foo",
            "a.rs:3:8 method Foo.bar",
            "a.rs:5:8 class Baz",
            "a.rs:7:8 method Baz.bar",
            "a.rs:9:4 function free_fn"
        ]
    );
    assert!(!r.xdg.exists(), "outline must not create an index");
}

#[test]
fn outline_works_outside_git_and_reports_bad_input() {
    let t = tempfile::tempdir().unwrap();
    std::fs::write(t.path().join("x.py"), "def f():\n    pass\n").unwrap();
    std::fs::write(t.path().join("x.bin"), "data").unwrap();
    let xdg = t.path().join("xdg");
    let ok = common::run_at(t.path(), &xdg, &["outline", "x.py"]);
    assert_eq!(ok.lines(), ["x.py:1:5 function f"]);
    let unsupported = common::run_at(t.path(), &xdg, &["outline", "x.bin"]);
    assert_eq!(unsupported.code, 1);
    assert!(
        unsupported.err.contains("unsupported"),
        "{}",
        unsupported.err
    );
    let missing = common::run_at(t.path(), &xdg, &["outline", "nope.rs"]);
    assert_eq!(missing.code, 4);
    assert!(missing.err.starts_with("ferrindex: "), "{}", missing.err);
}

#[test]
fn outline_of_a_file_with_no_definitions_is_exit_1() {
    let r = Repo::new();
    r.write("e.rs", "// nothing\n");
    assert_eq!(r.run(&["outline", "e.rs"]).code, 1);
}
