mod common;

use std::{fs, path::Path, process::Command};

use common::{Repo, SAMPLE};

fn git(root: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

fn snapshot(dir: &Path, out: &mut Vec<String>) {
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.file_name().is_some_and(|n| n == ".git") {
            continue;
        }
        out.push(p.display().to_string());
        if p.is_dir() {
            snapshot(&p, out);
        }
    }
    out.sort();
}

#[test]
fn every_subcommand_leaves_the_work_tree_untouched() {
    let repo = Repo::new();
    fs::remove_dir_all(repo.root.join(".git")).unwrap();
    repo.write("a.rs", SAMPLE);
    repo.write("sub/b.py", "def py_fn():\n    pass\n");
    repo.write("sub/inner/c.rs", "fn inner_fn() {}\n");
    repo.write(".gitignore", "ignored/\n");
    repo.write("ignored/x.rs", "fn hidden_fn() {}\n");
    git(&repo.root, &["init", "-q"]);
    git(&repo.root, &["add", "-A"]);
    git(
        &repo.root,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
    let mut before = Vec::new();
    snapshot(&repo.root, &mut before);

    let sub = Path::new("sub");
    for args in [
        &["update", "--rebuild"][..],
        &["update", "--stats"],
        &["find", "free_fn"],
        &["find", "fn"],
        &["files", "b"],
        &["show", "bar"],
        &["outline", "a.rs"],
        &["map"],
        &["map", "sub"],
    ] {
        let r = repo.run(args);
        assert!(r.code <= 1, "{args:?}: {}", r.err);
    }
    assert!(repo.run_in(sub, &["find", "py_fn"]).code <= 1);

    assert_eq!(
        git(&repo.root, &["status", "--porcelain", "--ignored"]),
        "!! ignored/\n"
    );
    assert_eq!(git(&repo.root, &["status", "--porcelain"]), "");
    let mut after = Vec::new();
    snapshot(&repo.root, &mut after);
    assert_eq!(before, after);
    assert!(
        repo.xdg.join("ferrindex").is_dir(),
        "the index went to the cache"
    );
}
