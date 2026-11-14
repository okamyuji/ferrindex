mod common;

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use common::{Repo, SAMPLE};

struct Hook {
    code: i32,
    out: Vec<u8>,
}

fn hook(xdg: &Path, stdin: &str) -> Hook {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ferrindex"))
        .args(["hook", "session-start"])
        .env("XDG_CACHE_HOME", xdg)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let o = child.wait_with_output().unwrap();
    Hook {
        code: o.status.code().unwrap(),
        out: o.stdout,
    }
}

fn input(cwd: &Path) -> String {
    format!(
        r#"{{"cwd":{:?},"hook_event_name":"SessionStart"}}"#,
        cwd.to_str().unwrap()
    )
}

fn main_files(xdg: &Path) -> Vec<PathBuf> {
    let Ok(dirs) = fs::read_dir(xdg.join("ferrindex")) else {
        return Vec::new();
    };
    dirs.flatten()
        .map(|d| d.path().join("main.fidx"))
        .filter(|p| p.exists())
        .collect()
}

fn wait_for_index(xdg: &Path) -> PathBuf {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(p) = main_files(xdg).pop() {
            return p;
        }
        assert!(Instant::now() < deadline, "index did not appear");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn assert_silent_ok(h: &Hook) {
    assert_eq!(h.code, 0);
    assert!(h.out.is_empty());
}

#[test]
fn builds_missing_index_in_background() {
    let r = Repo::new();
    r.write("src/a.rs", SAMPLE);
    let h = hook(&r.xdg, &input(&r.root));
    assert_silent_ok(&h);
    wait_for_index(&r.xdg);
    let found = r.run(&["find", "free_fn"]);
    assert_eq!(found.code, 0);
}

#[test]
fn finds_root_from_subdirectory() {
    let r = Repo::new();
    r.write("src/a.rs", SAMPLE);
    assert_silent_ok(&hook(&r.xdg, &input(&r.root.join("src"))));
    wait_for_index(&r.xdg);
}

#[test]
fn leaves_existing_index_alone() {
    let r = Repo::new();
    r.write("a.rs", SAMPLE);
    assert_eq!(r.run(&["update"]).code, 0);
    let main = wait_for_index(&r.xdg);
    let before = fs::metadata(&main).unwrap().modified().unwrap();
    r.write("b.rs", "fn added() {}\n");
    assert_silent_ok(&hook(&r.xdg, &input(&r.root)));
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(fs::metadata(&main).unwrap().modified().unwrap(), before);
}

#[test]
fn broken_json_is_ignored() {
    let r = Repo::new();
    r.write("a.rs", SAMPLE);
    assert_silent_ok(&hook(&r.xdg, "{not json"));
    assert_silent_ok(&hook(&r.xdg, ""));
    std::thread::sleep(Duration::from_millis(300));
    assert!(main_files(&r.xdg).is_empty());
}

#[test]
fn missing_or_wrong_typed_cwd_is_ignored() {
    let r = Repo::new();
    r.write("a.rs", SAMPLE);
    assert_silent_ok(&hook(&r.xdg, r#"{"hook_event_name":"SessionStart"}"#));
    assert_silent_ok(&hook(&r.xdg, r#"{"cwd":42}"#));
    std::thread::sleep(Duration::from_millis(300));
    assert!(main_files(&r.xdg).is_empty());
}

#[test]
fn cwd_outside_git_is_ignored() {
    let r = Repo::new();
    let plain = tempfile::tempdir().unwrap();
    fs::write(plain.path().join("a.rs"), SAMPLE).unwrap();
    assert_silent_ok(&hook(&r.xdg, &input(plain.path())));
    assert_silent_ok(&hook(&r.xdg, &input(&plain.path().join("missing"))));
    std::thread::sleep(Duration::from_millis(300));
    assert!(main_files(&r.xdg).is_empty());
}

#[test]
fn other_hook_names_are_usage_errors() {
    let r = Repo::new();
    assert_eq!(r.run(&["hook"]).code, 2);
    assert_eq!(r.run(&["hook", "other"]).code, 2);
}
