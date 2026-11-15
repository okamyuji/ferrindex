use std::ffi::OsString;
use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ferrindex::lang::Lang;
use ferrindex::lsp::{Method, ServerSpec, query_with};
use tempfile::TempDir;

const ROLE_ENV: &str = "FERRINDEX_TEST_CALLER_ROOT";

/// サーバーの下に孫（sleep）を置き、その pid を root/sleep_pid に書く。
fn server_with_grandchild(root: &Path) -> ServerSpec {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_lsp.py");
    let line = format!(
        "sleep 1000 & echo $! > '{}'; exec python3 -I '{}'",
        root.join("sleep_pid").display(),
        script.display()
    );
    ServerSpec {
        name: "fake",
        program: PathBuf::from("sh"),
        args: vec!["-c".into(), line.into()],
        env: vec![("FAKE_MODE", OsString::from("hang"))],
    }
}

fn repo() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("x.rs"), "fn f() {}\n").unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    (dir, root)
}

fn alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < until, "{what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// 呼び出し側の役。下の試験が別プロセスで起動し、問い合わせの途中でシグナルを送る。
#[test]
#[ignore = "signal_* の試験が別プロセスで起動する"]
fn caller_role() {
    let root = PathBuf::from(std::env::var_os(ROLE_ENV).unwrap());
    let deadline = Instant::now() + Duration::from_secs(60);
    let spec = server_with_grandchild(&root);
    let _ = query_with(
        &spec,
        &root,
        Lang::Rust,
        Path::new("x.rs"),
        1,
        1,
        Method::References,
        deadline,
    );
}

fn signal_leaves_no_server(signal: i32) {
    let (_d, root) = repo();
    let mut caller = Command::new(std::env::current_exe().unwrap())
        .args(["caller_role", "--exact", "--ignored", "--nocapture"])
        .env(ROLE_ENV, &root)
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let pid_file = root.join("sleep_pid");
    let mut sleep_pid = String::new();
    wait_until("the server never started", || {
        sleep_pid = fs::read_to_string(&pid_file).unwrap_or_default();
        sleep_pid.ends_with('\n')
    });
    let sleep_pid = sleep_pid.trim();
    assert!(alive(sleep_pid));
    Command::new("kill")
        .args([format!("-{signal}"), caller.id().to_string()])
        .status()
        .unwrap();
    // 合図を無視して普通に終わった呼び出し側では、孫が消えていても何も確かめていない。
    assert_eq!(caller.wait().unwrap().signal(), Some(signal));
    wait_until("the grandchild outlived the caller", || !alive(sleep_pid));
}

#[test]
fn sigterm_to_the_caller_stops_the_server_group() {
    signal_leaves_no_server(15);
}

#[test]
fn sigint_to_the_caller_stops_the_server_group() {
    signal_leaves_no_server(2);
}
