mod common;
use common::{Repo, run_at};

#[test]
fn outside_git_exits_3_with_rg_hint() {
    let t = tempfile::tempdir().unwrap();
    let run = run_at(t.path(), &t.path().join("xdg"), &["find", "it's"]);
    assert_eq!(run.code, 3);
    assert_eq!(run.out, "");
    assert!(
        run.err.contains("not inside a git repository"),
        "{}",
        run.err
    );
    assert!(run.err.contains("rg -n 'it'\\''s'"), "{}", run.err);
}

#[test]
fn every_search_command_has_its_own_rg_hint_outside_git() {
    let t = tempfile::tempdir().unwrap();
    let xdg = t.path().join("xdg");
    for (args, hint) in [
        (vec!["show", "x"], "rg -n 'x'"),
        (vec!["files", "x"], "rg --files"),
        (vec!["map"], "rg --files"),
        (vec!["update"], "rg --files"),
    ] {
        let run = run_at(t.path(), &xdg, &args);
        assert_eq!(run.code, 3, "{args:?}");
        assert!(run.err.contains(hint), "{args:?}: {}", run.err);
    }
}

#[test]
fn empty_term_is_a_usage_error() {
    let r = Repo::new();
    for args in [
        vec!["find", ""],
        vec!["find", "  "],
        vec!["files", ""],
        vec!["show", ""],
    ] {
        let run = r.run(&args);
        assert_eq!(run.code, 2, "{args:?}");
        assert!(run.err.contains("usage:"), "{}", run.err);
    }
}

#[test]
fn empty_term_wins_over_missing_git() {
    let t = tempfile::tempdir().unwrap();
    let run = run_at(t.path(), &t.path().join("xdg"), &["find", ""]);
    assert_eq!(run.code, 2);
}

#[test]
fn bad_arguments_are_usage_errors() {
    let r = Repo::new();
    for args in [
        vec![],
        vec!["bogus"],
        vec!["find"],
        vec!["find", "a", "b"],
        vec!["find", "a", "--nope"],
        vec!["find", "a", "--limit"],
        vec!["find", "a", "--limit", "0"],
        vec!["find", "a", "--limit", "x"],
        vec!["find", "a", "--rebuild"],
        vec!["find", "a", "--stats"],
        vec!["update", "x"],
        vec!["update", "--limit", "3"],
        vec!["outline"],
        vec!["outline", "a", "--limit", "3"],
        vec!["map", "a", "b"],
        vec!["files"],
        vec!["show"],
    ] {
        let run = r.run(&args);
        assert_eq!(run.code, 2, "{args:?}: {}", run.err);
        assert!(run.err.contains("usage:"), "{args:?}: {}", run.err);
    }
}

#[test]
fn non_utf8_term_is_a_usage_error() {
    use std::os::unix::ffi::OsStrExt;
    let r = Repo::new();
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_ferrindex"))
        .arg("find")
        .arg(std::ffi::OsStr::from_bytes(b"\xff"))
        .current_dir(&r.root)
        .env("XDG_CACHE_HOME", &r.xdg)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn no_results_exit_1_with_empty_stdout() {
    let r = Repo::new();
    r.write("a.rs", "fn here() {}\n");
    let run = r.run(&["find", "absent"]);
    assert_eq!((run.code, run.out.as_str()), (1, ""));
    assert_eq!(r.run(&["show", "absent"]).code, 1);
    assert_eq!(r.run(&["files", "absent"]).code, 1);
}

#[test]
fn corrupt_index_is_rebuilt_and_search_succeeds() {
    let r = Repo::new();
    r.write("a.rs", "fn here() {}\n");
    assert_eq!(r.run(&["find", "here"]).code, 0);
    for f in walk(&r.xdg) {
        std::fs::write(f, b"garbage").unwrap();
    }
    let run = r.run(&["find", "here"]);
    assert_eq!(run.code, 0, "{}", run.err);
    assert_eq!(run.heads(), ["a.rs:1:4 function here"]);
}

#[test]
fn unwritable_cache_is_an_io_error_not_exit_3() {
    use std::os::unix::fs::PermissionsExt;
    let r = Repo::new();
    r.write("a.rs", "fn here() {}\n");
    std::fs::create_dir_all(&r.xdg).unwrap();
    let blocker = r.xdg.join("ferrindex");
    std::fs::write(&blocker, b"a file where the cache dir should be").unwrap();
    std::fs::set_permissions(&blocker, std::fs::Permissions::from_mode(0o644)).unwrap();
    let run = r.run(&["find", "here"]);
    assert_eq!(run.code, 4, "{}", run.err);
    assert!(run.err.starts_with("ferrindex: "), "{}", run.err);
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else if p.extension().is_some_and(|x| x == "fidx") {
            out.push(p);
        }
    }
    out
}

#[test]
fn unknown_flag_and_wrong_arity_have_their_own_messages() {
    let r = Repo::new();
    let flag = r.run(&["find", "--nope"]);
    assert_eq!(flag.code, 2);
    assert!(flag.err.contains("unknown flag --nope"), "{}", flag.err);
    let arity = r.run(&["find", "a", "b"]);
    assert!(
        arity.err.contains("wrong number of arguments"),
        "{}",
        arity.err
    );
    assert!(r.run(&["bogus"]).err.contains("unknown subcommand"));
}
