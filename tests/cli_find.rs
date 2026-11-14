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
        from_root.heads(),
        [
            "other/out.rs:1:4 function target",
            "sub/in.rs:1:4 function target",
            "top.rs:1:4 function target"
        ]
    );
    let from_sub = r.run_in(Path::new("sub"), &["find", "target"]);
    assert_eq!(
        from_sub.heads(),
        [
            "../other/out.rs:1:4 function target",
            "in.rs:1:4 function target",
            "../top.rs:1:4 function target"
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
        all.heads(),
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
        cut.heads(),
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
    assert_eq!(r.run(&["find", "open"]).heads(), ["a.rs:1:4 function open"]);
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
        run.heads(),
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
        r.run(&["find", "Foo.BA"]).heads(),
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
    assert_eq!(run.heads()[0], "a.rs:1:4 function zz000");
    assert_eq!(run.heads()[199], "a.rs:200:4 function zz199");
}

#[test]
fn search_sees_edits_made_after_the_index_exists() {
    let r = Repo::new();
    r.write("a.rs", "fn before() {}\n");
    assert_eq!(r.run(&["find", "before"]).code, 0);
    r.write("a.rs", "fn after_edit() {}\n\n\n");
    assert_eq!(r.run(&["find", "before"]).code, 1);
    assert_eq!(
        r.run(&["find", "after_edit"]).heads(),
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

/// 塞がった入力で永遠に待たないよう、期限を過ぎたら子を殺して失敗にする。
fn outline_with_deadline(cwd: &std::path::Path, arg: &str) -> Option<(i32, String)> {
    use std::process::{Command, Stdio};
    let mut child = Command::new(env!("CARGO_BIN_EXE_ferrindex"))
        .args(["outline", arg])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let o = child.wait_with_output().unwrap();
    Some((
        o.status.code().unwrap(),
        String::from_utf8(o.stderr).unwrap(),
    ))
}

#[test]
fn outline_rejects_non_regular_files_without_blocking() {
    let t = tempfile::tempdir().unwrap();
    let fifo = t.path().join("fifo.rs");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(made.success());
    std::fs::create_dir(t.path().join("dir.rs")).unwrap();
    for name in ["fifo.rs", "dir.rs"] {
        let (code, err) = outline_with_deadline(t.path(), name).expect("outline blocked");
        assert_eq!(code, 4, "{name}: {err}");
        assert_eq!(err.lines().count(), 1, "{err}");
        assert!(err.contains("not a regular file"), "{err}");
    }
}

#[test]
fn outline_caps_input_at_the_index_size_limit() {
    let t = tempfile::tempdir().unwrap();
    let limit = 1usize << 20;
    let pad = |n: usize| format!("fn f() {{}}\n{}", "/".repeat(n - 10));
    std::fs::write(t.path().join("exact.rs"), pad(limit)).unwrap();
    std::fs::write(t.path().join("over.rs"), pad(limit + 1)).unwrap();
    let xdg = t.path().join("xdg");
    let exact = common::run_at(t.path(), &xdg, &["outline", "exact.rs"]);
    assert_eq!(exact.lines(), ["exact.rs:1:4 function f"]);
    let over = common::run_at(t.path(), &xdg, &["outline", "over.rs"]);
    assert_eq!(over.code, 4, "{}", over.err);
    assert_eq!(over.err.lines().count(), 1, "{}", over.err);
    assert!(over.err.contains("1048576"), "{}", over.err);
}

#[test]
fn control_characters_in_paths_are_escaped_in_every_command() {
    let r = Repo::new();
    r.write("d/a\nb\x01.rs", "fn zed() {}\n");
    let name = "d/a\\nb\\x01.rs";
    let rel = std::path::Path::new("d").join("a\nb\x01.rs");
    let rel = rel.to_str().unwrap();
    assert_eq!(
        r.run(&["find", "zed"]).heads(),
        [format!("{name}:1:4 function zed")]
    );
    assert_eq!(r.run(&["files", "d/"]).lines(), [name]);
    let outline = r.run(&["outline", rel]);
    assert_eq!(outline.lines(), [format!("{name}:1:4 function zed")]);
    let show = r.run(&["show", "zed"]);
    assert_eq!(show.lines()[0], format!("{name}:1-1 function zed"));
    let map = r.run(&["map"]);
    assert_eq!(map.lines().len(), 1, "{}", map.out);
    assert!(map.lines()[0].starts_with(name), "{}", map.out);
}

#[test]
fn double_colon_and_dot_qualify_names_alike() {
    let r = Repo::new();
    r.write("app/base.rb", "class ActivityPub::BaseController\nend\n");
    r.write("app/other.rb", "class Other::BaseController\nend\n");
    for term in ["ActivityPub::BaseController", "ActivityPub.BaseController"] {
        let run = r.run(&["find", term]);
        assert_eq!(
            run.lines(),
            [
                "app/base.rb:1:20 class ActivityPub.BaseController\tclass ActivityPub::BaseController"
            ],
            "{term}"
        );
    }
    r.write("a.cpp", "struct S { void f(); };\nvoid S::f() {}\n");
    assert_eq!(r.run(&["find", "S::f"]).code, 0);
}

#[test]
fn find_appends_the_trimmed_definition_line_after_a_tab() {
    let r = Repo::new();
    let long = format!("fn long_one() {{ let _s = \"{}\"; }}", "x".repeat(300));
    r.write(
        "a.rs",
        &format!("mod m {{\n    pub fn short_one(a: i32) {{}}\n}}\n{long}\n"),
    );
    let short = r.run(&["find", "short_one"]);
    assert_eq!(
        short.lines(),
        ["a.rs:2:12 method m.short_one\tpub fn short_one(a: i32) {}"]
    );
    let line = r.run(&["find", "long_one"]).out;
    let (head, src) = line.trim_end().split_once('\t').unwrap();
    assert_eq!(head, "a.rs:4:4 function long_one");
    assert_eq!(src.chars().count(), 160);
    assert!(
        src.ends_with('…') && src.starts_with("fn long_one"),
        "{src}"
    );
    // 160 字ちょうどは切らない
    r.write(
        "b.rs",
        &format!("fn exact() {{}}{}\n", "/".repeat(160 - 13)),
    );
    let exact = r.run(&["find", "exact"]).out;
    assert_eq!(
        exact.trim_end().split_once('\t').unwrap().1.chars().count(),
        160
    );
    assert!(!exact.contains('…'));
}

#[test]
fn find_escapes_control_characters_in_the_definition_line() {
    let r = Repo::new();
    r.write("a.rs", "fn ctl() { /* a\u{1}b\tc */ }\n");
    let out = r.run(&["find", "ctl"]).out;
    assert_eq!(out.lines().count(), 1);
    assert_eq!(out.trim_end().split('\t').count(), 2, "{out:?}");
    assert!(out.contains("a\\x01b\\x09c"), "{out:?}");
}

#[test]
fn find_only_reads_the_files_it_shows() {
    let r = Repo::new();
    r.write("a.rs", "fn many() {}\n");
    r.write("b.rs", "fn many() {}\n");
    r.write("c.rs", "fn many() {}\n");
    let run = r.run(&["find", "many", "--limit", "1"]);
    assert_eq!(run.lines(), ["a.rs:1:4 function many\tfn many() {}"]);
    assert_eq!(run.err, "… 2 more\n");
}

#[test]
fn methods_of_a_scope_resolution_class_carry_the_full_qualifier() {
    let r = Repo::new();
    r.write("p.rb", "class Admin::StatusPolicy\n  def show?; end\nend\n");
    for term in ["Admin::StatusPolicy.show?", "Admin.StatusPolicy.show?"] {
        assert_eq!(
            r.run(&["find", term]).heads(),
            ["p.rb:2:7 method Admin.StatusPolicy.show?"],
            "{term}"
        );
    }
}

#[test]
fn partial_dotted_heading_is_found_by_substring() {
    let r = Repo::new();
    r.write("notes.md", "# Rust 1.100 notes\n\ntext\n");
    assert_eq!(
        r.run(&["find", "Rust 1.100"]).heads(),
        ["notes.md:1:3 section Rust 1.100 notes"]
    );
}
