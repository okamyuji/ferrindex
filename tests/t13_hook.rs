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
    hook_event(xdg, "session-start", stdin)
}

fn hook_event(xdg: &Path, event: &str, stdin: &str) -> Hook {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ferrindex"))
        .args(["hook", event])
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
    assert!(h.out.is_empty(), "{}", String::from_utf8_lossy(&h.out));
}

fn guidance(h: &Hook) -> String {
    assert_eq!(h.code, 0);
    String::from_utf8(h.out.clone()).unwrap()
}

fn assert_guidance(text: &str) {
    assert!(text.lines().count() <= 4, "{text}");
    for needle in [
        "ferrindex find <名前>",
        "ferrindex show <名前>",
        "ferrindex outline <file>",
        "ferrindex files <部分文字列>",
        "各行に定義の行が付く",
        "find と Read ではなく `ferrindex show <名前>` を直接",
        "呼び出し元は `ferrindex callers <名前>`",
        "使用箇所は `ferrindex refs <名前>`",
        "その位置を囲む定義が付きます",
        "rg、Grep、Read で確かめ直さないでください",
        "終了コード 3 のときは `rg -n` を使ってください",
        "`Class.method` と書いても修飾では絞れません",
        "正確な範囲が要るときは `--precise` を付けてください",
    ] {
        assert!(text.contains(needle), "missing {needle}: {text}");
    }
    for absent in ["trust"] {
        assert!(!text.contains(absent), "has {absent}: {text}");
    }
}

#[test]
fn builds_missing_index_in_background() {
    let r = Repo::new();
    r.write("src/a.rs", SAMPLE);
    let h = hook(&r.xdg, &input(&r.root));
    let text = guidance(&h);
    assert_guidance(&text);
    assert!(text.contains("最初の検索は待つことがあります"), "{text}");
    wait_for_index(&r.xdg);
    let found = r.run(&["find", "free_fn"]);
    assert_eq!(found.code, 0);
}

#[test]
fn finds_root_from_subdirectory() {
    let r = Repo::new();
    r.write("src/a.rs", SAMPLE);
    assert_guidance(&guidance(&hook(&r.xdg, &input(&r.root.join("src")))));
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
    let text = guidance(&hook(&r.xdg, &input(&r.root)));
    assert_guidance(&text);
    assert!(!text.contains("待つ"), "{text}");
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

fn post(command: &str) -> Hook {
    let input = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
    });
    hook_event(
        Path::new("/nonexistent"),
        "post-tool-use",
        &input.to_string(),
    )
}

fn context_of(h: &Hook) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(&h.out).ok()?;
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PostToolUse");
    v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .map(str::to_string)
}

#[test]
fn post_tool_use_adds_answer_now_note_after_lookups() {
    for cmd in [
        "ferrindex find SyncPod",
        "ferrindex callers syncPod",
        "ferrindex refs Pod --limit 5",
        "ferrindex show Kubelet.SyncPod",
        "cd sub && ferrindex find a",
        "ferrindex find foo | head -3",
    ] {
        let h = post(cmd);
        assert_eq!(h.code, 0, "{cmd}");
        let note = context_of(&h).unwrap_or_else(|| panic!("no context for {cmd}"));
        assert!(note.contains("そのまま回答"), "{cmd}: {note}");
    }
}

#[test]
fn post_tool_use_is_silent_for_other_commands() {
    for cmd in [
        "rg -n foo",
        "echo ferrindex find foo",
        "ferrindex outline a.rs",
        "ferrindex files main",
        "ferrindex impls Shape",
        "cargo test",
    ] {
        let h = post(cmd);
        assert_eq!(h.code, 0, "{cmd}");
        assert!(
            h.out.is_empty(),
            "{cmd}: {}",
            String::from_utf8_lossy(&h.out)
        );
    }
}

#[test]
fn post_tool_use_is_silent_on_broken_input() {
    for input in ["", "{", "[]", r#"{"tool_input":{"command":1}}"#] {
        let h = hook_event(Path::new("/nonexistent"), "post-tool-use", input);
        assert_eq!(h.code, 0, "{input}");
        assert!(h.out.is_empty(), "{input}");
    }
}
