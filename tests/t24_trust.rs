mod common;

use std::fs;

use common::Repo;

const UNTRUSTED: &str = "ferrindex: --precise: このリポジトリは信頼されていません。`ferrindex trust` を端末で実行すると使えます。名前一致の結果です（未確定）\n";

fn repo() -> Repo {
    let r = Repo::new();
    r.write("a.rs", "fn target() {}\nfn x() {\n    target();\n}\n");
    r
}

/// 偽サーバーは起動すると `pid` に自分の pid を書く。
fn precise(r: &Repo) -> (common::Run, bool) {
    let pid = r.xdg.with_file_name("pid");
    let _ = fs::remove_file(&pid);
    let server = format!(
        "python3 -I {}/tests/fixtures/fake_lsp.py",
        env!("CARGO_MANIFEST_DIR")
    );
    let o = r.run_with(
        &[
            ("FERRINDEX_LSP_RUST", &server),
            ("FAKE_REFS", r#"[["a.rs", 2, 4]]"#),
            ("FAKE_PID_FILE", pid.to_str().unwrap()),
        ],
        &["refs", "target", "--precise"],
    );
    (o, pid.exists())
}

#[test]
fn untrusted_repository_does_not_start_a_server() {
    let r = repo();
    let (o, started) = precise(&r);
    assert!(!started);
    assert_eq!(o.code, 0, "{}", o.err);
    assert_eq!(o.err, UNTRUSTED);
    assert_eq!(o.lines(), ["a.rs:3:5 call in x\ttarget();"]);
}

#[test]
fn trust_starts_the_server_and_untrust_stops_it_again() {
    let r = repo();
    r.trust();
    let (o, started) = precise(&r);
    assert!(started);
    assert_eq!(o.err, "");
    assert_eq!(o.lines(), ["a.rs:3:5 call in x\ttarget();"]);
    // untrust は端末でなくてもよい（試験の標準入力は端末でない）。
    let u = r.run(&["untrust"]);
    assert_eq!(u.code, 0, "{}", u.err);
    let (o, started) = precise(&r);
    assert!(!started);
    assert_eq!(o.err, UNTRUSTED);
}

#[test]
fn trust_needs_a_terminal() {
    let r = repo();
    let o = r.run(&["trust"]);
    assert_eq!(o.code, 2);
    assert!(o.err.contains("端末から実行してください"), "{}", o.err);
    assert!(!precise(&r).1);
}

#[test]
fn trust_and_untrust_need_a_git_repository() {
    let r = repo();
    let outside = r.root.parent().unwrap();
    for cmd in ["trust", "untrust"] {
        let o = common::run_at(outside, &r.xdg, &[cmd]);
        assert_eq!(o.code, 3, "{cmd}: {}", o.err);
    }
}

#[test]
fn trust_takes_no_arguments_or_flags() {
    let r = repo();
    for cmd in ["trust", "untrust"] {
        let o = r.run(&[cmd, "x"]);
        assert_eq!(o.code, 2);
        assert!(o.err.contains("wrong number of arguments"), "{}", o.err);
    }
    assert_eq!(r.run(&["untrust", "--limit", "3"]).code, 2);
}

#[test]
fn trusted_list_holds_no_plain_paths() {
    let r = repo();
    r.trust();
    r.trust();
    let list = common::data_home(&r.xdg).join("ferrindex/trusted");
    let text = fs::read_to_string(&list).unwrap();
    let root = r.root.to_str().unwrap();
    assert!(!text.contains(root), "{text}");
    // 同じ根を2回信頼しても1行。
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(
        text.lines()
            .all(|l| l.len() == 16 && l.bytes().all(|b| b.is_ascii_hexdigit())),
        "{text}"
    );
}
