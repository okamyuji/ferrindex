mod common;

use std::{fs, os::unix::fs::PermissionsExt};

use common::Repo;

/// 索引を作った後で読めなくしたファイル。mtime は変わらないので索引は読み直さない。
fn unreadable(files: &[(&str, &str)]) -> Repo {
    let r = Repo::new();
    for (rel, text) in files {
        r.write(rel, text);
    }
    r.age();
    assert_eq!(r.run(&["update"]).code, 0);
    for (rel, _) in files {
        fs::set_permissions(r.root.join(rel), fs::Permissions::from_mode(0o000)).unwrap();
    }
    r
}

const A_RS: &str = "fn helper() {}\nfn x() {\n    helper();\n}\n";

#[test]
fn reading_commands_fail_with_the_path_of_an_unreadable_file() {
    let r = unreadable(&[
        ("a.rs", A_RS),
        ("p.py", "class A:\n    pass\n\nclass B(A):\n    pass\n"),
    ]);
    for args in [
        &["refs", "helper"][..],
        &["callers", "helper"],
        &["find", "helper"],
        &["show", "helper"],
        &["map"],
    ] {
        let o = r.run(args);
        assert_eq!(o.code, 4, "{args:?}: {}", o.err);
        assert!(
            o.err.starts_with("ferrindex: a.rs: "),
            "{args:?}: {}",
            o.err
        );
        assert_eq!(o.out, "", "{args:?}");
    }
    let o = r.run(&["supers", "B"]);
    assert_eq!(o.code, 4, "{}", o.err);
    assert!(o.err.starts_with("ferrindex: p.py: "), "{}", o.err);
}

#[test]
fn precise_fails_when_an_answered_file_is_unreadable() {
    let r = Repo::new();
    r.write("a.rs", A_RS);
    r.write("c.rs", "fn other() {\n    helper();\n}\n");
    r.age();
    assert_eq!(r.run(&["update"]).code, 0);
    r.trust();
    fs::set_permissions(r.root.join("c.rs"), fs::Permissions::from_mode(0o000)).unwrap();
    let server = format!(
        "python3 -I {}/tests/fixtures/fake_lsp.py",
        env!("CARGO_MANIFEST_DIR")
    );
    let o = r.run_with(
        &[
            ("FERRINDEX_LSP_RUST", &server),
            ("FAKE_REFS", r#"[["a.rs", 2, 4], ["c.rs", 1, 4]]"#),
        ],
        &["refs", "helper", "--precise"],
    );
    assert_eq!(o.code, 4, "{}", o.err);
    assert!(o.err.starts_with("ferrindex: c.rs: "), "{}", o.err);
}

#[test]
fn update_names_a_file_it_cannot_read() {
    let r = Repo::new();
    r.write("a\x1bb.rs", A_RS);
    fs::set_permissions(r.root.join("a\x1bb.rs"), fs::Permissions::from_mode(0o000)).unwrap();
    let o = r.run(&["update"]);
    assert_eq!(o.code, 4, "{}", o.err);
    assert!(o.err.starts_with("ferrindex: a\\x1bb.rs: "), "{}", o.err);
}
