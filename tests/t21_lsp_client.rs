use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ferrindex::lang::Lang;
use ferrindex::lsp::{
    Location, LspError, Method, ServerSpec, pick_java_home, query, query_with, server_for,
};
use tempfile::TempDir;

fn fake(env: &[(&'static str, &str)]) -> ServerSpec {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_lsp.py");
    ServerSpec {
        name: "fake",
        program: PathBuf::from("python3"),
        args: vec!["-I".into(), script.into()],
        env: env.iter().map(|&(k, v)| (k, OsString::from(v))).collect(),
    }
}

/// 一時ディレクトリは /var を経由するので、サーバーが返す正規化済みのパスと比べられるよう正規化する。
fn repo() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("x.rs"), "fn f() {}\n").unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    (dir, root)
}

fn soon() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

fn ask(spec: &ServerSpec, root: &Path, method: Method) -> Result<Vec<Location>, LspError> {
    query_with(
        spec,
        root,
        Lang::Rust,
        Path::new("x.rs"),
        3,
        5,
        method,
        soon(),
    )
}

fn loc(path: PathBuf, line: u32, col: u32) -> Location {
    Location { path, line, col }
}

fn alive(pid_file: &Path) -> bool {
    let pid = fs::read_to_string(pid_file).unwrap();
    Command::new("kill")
        .args(["-0", pid.trim()])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[test]
fn references_are_parsed() {
    let (_d, root) = repo();
    let got = ask(&fake(&[]), &root, Method::References).unwrap();
    assert_eq!(
        got,
        vec![
            loc(root.join("src/a b.rs"), 10, 1),
            loc(root.join("x.rs"), 3, 5)
        ]
    );
}

#[test]
fn results_are_sorted_without_duplicates() {
    let (_d, root) = repo();
    let got = ask(
        &fake(&[("FAKE_MODE", "unsorted")]),
        &root,
        Method::References,
    )
    .unwrap();
    let (a, b) = (root.join("a.rs"), root.join("b.rs"));
    let want = vec![
        loc(a.clone(), 1, 10),
        loc(a.clone(), 5, 2),
        loc(a, 5, 3),
        loc(b, 2, 1),
    ];
    assert_eq!(got, want);
}

#[test]
fn incoming_calls_are_the_call_sites() {
    let (_d, root) = repo();
    let got = ask(&fake(&[]), &root, Method::IncomingCalls).unwrap();
    let caller = root.join("caller.rs");
    assert_eq!(got, vec![loc(caller.clone(), 5, 2), loc(caller, 7, 3)]);
}

#[test]
fn implementation_reads_location_links() {
    let (_d, root) = repo();
    let got = ask(&fake(&[]), &root, Method::Implementation).unwrap();
    assert_eq!(got, vec![loc(root.join("impl.rs"), 4, 6)]);
}

#[test]
fn supertypes_are_the_items() {
    let (_d, root) = repo();
    let got = ask(&fake(&[]), &root, Method::Supertypes).unwrap();
    assert_eq!(got, vec![loc(root.join("base.rs"), 2, 8)]);
}

#[test]
fn server_requests_are_answered() {
    let (_d, root) = repo();
    let got = ask(
        &fake(&[("FAKE_MODE", "needs_answers")]),
        &root,
        Method::References,
    )
    .unwrap();
    assert_eq!(got.len(), 2);
}

#[test]
fn answers_while_loading_are_asked_again() {
    let (_d, root) = repo();
    let got = ask(
        &fake(&[("FAKE_MODE", "loading")]),
        &root,
        Method::References,
    )
    .unwrap();
    assert_eq!(got.len(), 2);
}

#[test]
fn garbage_before_headers_is_a_start_failure() {
    let (_d, root) = repo();
    let err = ask(
        &fake(&[("FAKE_MODE", "garbage")]),
        &root,
        Method::References,
    )
    .unwrap_err();
    assert_eq!(
        err,
        LspError::StartFailed("An error has occurred. See the log file".into())
    );
}

#[test]
fn initialize_error_is_a_start_failure() {
    let (_d, root) = repo();
    let err = ask(
        &fake(&[("FAKE_FAIL", "initialize")]),
        &root,
        Method::References,
    )
    .unwrap_err();
    assert_eq!(err, LspError::StartFailed("failed initialize".into()));
}

#[test]
fn request_error_is_a_protocol_error() {
    let spec = fake(&[("FAKE_FAIL", "textDocument/references")]);
    let (_d, root) = repo();
    let err = ask(&spec, &root, Method::References).unwrap_err();
    assert_eq!(
        err,
        LspError::Protocol("failed textDocument/references".into())
    );
}

#[test]
fn server_is_killed_after_timeout() {
    let (_d, root) = repo();
    let pid_file = root.join("pid");
    let pid = pid_file.to_str().unwrap();
    let spec = fake(&[("FAKE_MODE", "hang"), ("FAKE_PID_FILE", pid)]);
    let start = Instant::now();
    let deadline = start + Duration::from_secs(1);
    let got = query_with(
        &spec,
        &root,
        Lang::Rust,
        Path::new("x.rs"),
        1,
        1,
        Method::References,
        deadline,
    );
    assert_eq!(got, Err(LspError::Timeout));
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );
    assert!(!alive(&pid_file));
}

#[test]
fn server_exits_cleanly_after_the_answer() {
    let (_d, root) = repo();
    let (pid_file, exit_file) = (root.join("pid"), root.join("exited"));
    let spec = fake(&[
        ("FAKE_PID_FILE", pid_file.to_str().unwrap()),
        ("FAKE_EXIT_FILE", exit_file.to_str().unwrap()),
    ]);
    ask(&spec, &root, Method::References).unwrap();
    assert!(
        exit_file.exists(),
        "server got no grace to exit after shutdown and exit"
    );
    assert!(!alive(&pid_file));
}

#[test]
fn missing_program_is_not_installed() {
    let (_d, root) = repo();
    let mut spec = fake(&[]);
    spec.program = root.join("no-such-server");
    let err = ask(&spec, &root, Method::References).unwrap_err();
    assert_eq!(err, LspError::NotInstalled("fake".into()));
}

#[test]
fn missing_file_is_a_start_failure() {
    let (_d, root) = repo();
    let got = query_with(
        &fake(&[]),
        &root,
        Lang::Rust,
        Path::new("nope.rs"),
        1,
        1,
        Method::References,
        soon(),
    );
    assert!(matches!(got, Err(LspError::StartFailed(m)) if m.contains("nope.rs")));
}

#[test]
fn jdk_below_25_is_skipped() {
    let dir = TempDir::new().unwrap();
    let jdk = |name: &str, version: &str| {
        let home = dir.path().join(name);
        fs::create_dir(&home).unwrap();
        fs::write(
            home.join("release"),
            format!("IMPLEMENTOR=\"x\"\nJAVA_VERSION=\"{version}\"\n"),
        )
        .unwrap();
        home
    };
    let (old, new) = (jdk("jdk21", "21.0.5"), jdk("jdk25", "25"));
    let missing = dir.path().join("missing");
    assert_eq!(
        pick_java_home([missing, old.clone(), new.clone()]),
        Some(new)
    );
    assert_eq!(pick_java_home([old]), None);
}

#[test]
fn markdown_is_unsupported() {
    assert!(matches!(
        server_for(Lang::Markdown),
        Err(LspError::Unsupported(_))
    ));
    let (_d, root) = repo();
    let got = query(
        &root,
        Lang::Markdown,
        Path::new("x.rs"),
        1,
        1,
        Method::References,
        soon(),
    );
    assert!(matches!(got, Err(LspError::Unsupported(_))));
}

#[test]
fn shutdown_grace_ends_at_the_deadline() {
    let (_d, root) = repo();
    let spec = ServerSpec {
        name: "s",
        program: PathBuf::from("sh"),
        args: vec!["-c".into(), "echo bad; exec sleep 30".into()],
        env: vec![],
    };
    let start = Instant::now();
    let deadline = start + Duration::from_secs(1);
    let got = query_with(
        &spec,
        &root,
        Lang::Rust,
        Path::new("x.rs"),
        1,
        1,
        Method::References,
        deadline,
    );
    assert_eq!(got, Err(LspError::StartFailed("bad".into())));
    let took = start.elapsed();
    assert!(took < Duration::from_millis(1300), "{took:?}");
}

#[test]
fn answers_are_asked_again_until_two_agree() {
    let (_d, root) = repo();
    let got = ask(
        &fake(&[("FAKE_MODE", "growing")]),
        &root,
        Method::References,
    )
    .unwrap();
    assert_eq!(
        got,
        [loc(root.join("x.rs"), 1, 1), loc(root.join("x.rs"), 2, 1)]
    );
}

#[test]
fn answers_that_keep_changing_are_unsettled() {
    let (_d, root) = repo();
    let start = Instant::now();
    let deadline = start + Duration::from_secs(3);
    let got = query_with(
        &fake(&[("FAKE_MODE", "changing")]),
        &root,
        Lang::Rust,
        Path::new("x.rs"),
        1,
        1,
        Method::References,
        deadline,
    );
    assert_eq!(got, Err(LspError::Unsettled));
    let took = start.elapsed();
    assert!(took < Duration::from_millis(3500), "{took:?}");
}
