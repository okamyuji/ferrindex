use std::{
    collections::BTreeMap,
    fs::{self, File},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use super::*;
use crate::lang::Lang;

static SEQ: AtomicU64 = AtomicU64::new(0);

struct Env {
    _t: tempfile::TempDir,
    root: PathBuf,
    cache: PathBuf,
}

fn env() -> Env {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("repo");
    fs::create_dir_all(root.join(".git")).unwrap();
    let cache = t.path().join("cache");
    Env { _t: t, root, cache }
}

fn set_mtime(path: &Path, secs: u64) {
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_secs(secs))
        .unwrap();
}

/// 書くたびに別の古い mtime を付ける。時刻が競合窓の外に出るので、差分の経路を使う。
fn put(e: &Env, rel: &str, body: &str) {
    let p = e.root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, body).unwrap();
    set_mtime(&p, 1_000_000 + SEQ.fetch_add(1, Ordering::Relaxed));
}

fn rm(e: &Env, rel: &str) {
    fs::remove_file(e.root.join(rel)).unwrap();
}

fn run(e: &Env) -> View {
    refresh_in(&e.root, &e.cache, Duration::from_secs(5)).unwrap()
}

fn delta_path(e: &Env) -> PathBuf {
    e.cache.join(DELTA_FILE)
}

type Row = (String, u32, u32, u8, Option<String>);

/// 走査と抽出だけで作る基準。差分の仕組みを通らない。
fn reference(root: &Path) -> BTreeMap<String, Vec<Row>> {
    let mut out: BTreeMap<String, Vec<Row>> = BTreeMap::new();
    for s in scan(root).unwrap() {
        let rel = String::from_utf8(s.path).unwrap();
        let Some(lang) = lang_of(Path::new(&rel)) else {
            continue;
        };
        let bytes = fs::read(root.join(&rel)).unwrap();
        if bytes[..bytes.len().min(NUL_SCAN_BYTES)].contains(&0) {
            continue;
        }
        for d in extract(lang, &bytes) {
            out.entry(d.name.clone()).or_default().push((
                rel.clone(),
                d.line,
                d.col,
                d.kind,
                d.container,
            ));
        }
    }
    for rows in out.values_mut() {
        rows.sort();
    }
    out
}

fn found(v: &View, name: &str) -> Vec<Row> {
    let mut rows: Vec<Row> = v
        .lookup(name.as_bytes())
        .unwrap()
        .into_iter()
        .map(|f| {
            (
                String::from_utf8(f.path).unwrap(),
                f.line,
                f.col,
                f.kind,
                f.container.map(|c| String::from_utf8(c).unwrap()),
            )
        })
        .collect();
    rows.sort();
    rows
}

/// I3: 検索結果は、その時点の作業ツリーを丸ごと解析し直した結果と等しい。
fn assert_equals_reparse(e: &Env, v: &View, absent: &[&str]) {
    let want = reference(&e.root);
    for (name, rows) in &want {
        assert_eq!(&found(v, name), rows, "name {name}");
    }
    for name in absent {
        assert!(found(v, name).is_empty(), "{name} should be gone");
    }
    let listed: Vec<Vec<u8>> = v.files().into_iter().map(|(p, _)| p).collect();
    let scanned: Vec<Vec<u8>> = scan(&e.root).unwrap().into_iter().map(|s| s.path).collect();
    assert_eq!(listed, scanned);
}

#[test]
fn new_file_is_found() {
    let e = env();
    put(&e, "a.rs", "fn one() {}\n");
    for i in 0..20 {
        put(&e, &format!("f{i}.rs"), &format!("fn f{i}() {{}}\n"));
    }
    let v = run(&e);
    assert_equals_reparse(&e, &v, &["two"]);
    put(&e, "b.rs", "struct Two;\nfn two() {}\n");
    let v = run(&e);
    assert!(delta_path(&e).exists());
    assert_equals_reparse(&e, &v, &[]);
    assert_eq!(found(&v, "two").len(), 1);
}

#[test]
fn edited_file_replaces_its_old_definitions() {
    let e = env();
    for i in 0..20 {
        put(&e, &format!("f{i}.rs"), &format!("fn f{i}() {{}}\n"));
    }
    run(&e);
    put(&e, "f3.rs", "fn renamed3() {}\n");
    let v = run(&e);
    assert_equals_reparse(&e, &v, &["f3"]);
    put(&e, "f3.rs", "fn again3() {}\n");
    let v = run(&e);
    assert_equals_reparse(&e, &v, &["f3", "renamed3"]);
    assert_eq!(found(&v, "again3").len(), 1);
}

#[test]
fn deleted_file_disappears() {
    let e = env();
    for i in 0..20 {
        put(&e, &format!("f{i}.rs"), &format!("fn f{i}() {{}}\n"));
    }
    run(&e);
    rm(&e, "f4.rs");
    let v = run(&e);
    assert!(delta_path(&e).exists());
    assert_equals_reparse(&e, &v, &["f4"]);
    let v = run(&e);
    assert_equals_reparse(&e, &v, &["f4"]);
}

#[test]
fn file_added_then_deleted_leaves_no_trace() {
    let e = env();
    for i in 0..20 {
        put(&e, &format!("f{i}.rs"), &format!("fn f{i}() {{}}\n"));
    }
    run(&e);
    put(&e, "new.rs", "fn newbie() {}\n");
    run(&e);
    rm(&e, "new.rs");
    let v = run(&e);
    assert_equals_reparse(&e, &v, &["newbie"]);
    assert!(!delta_path(&e).exists());
}

#[test]
fn deleted_file_can_come_back() {
    let e = env();
    for i in 0..20 {
        put(&e, &format!("f{i}.rs"), &format!("fn f{i}() {{}}\n"));
    }
    run(&e);
    rm(&e, "f4.rs");
    run(&e);
    put(&e, "f4.rs", "fn back4() {}\n");
    let v = run(&e);
    assert_equals_reparse(&e, &v, &["f4"]);
    assert_eq!(found(&v, "back4").len(), 1);
}

#[test]
fn renamed_file_moves() {
    let e = env();
    for i in 0..20 {
        put(&e, &format!("f{i}.rs"), &format!("fn f{i}() {{}}\n"));
    }
    run(&e);
    fs::rename(e.root.join("f5.rs"), e.root.join("moved.rs")).unwrap();
    let v = run(&e);
    assert_equals_reparse(&e, &v, &[]);
    assert_eq!(found(&v, "f5")[0].0, "moved.rs");
}

#[test]
fn gitignore_edit_applies() {
    let e = env();
    put(&e, "keep/a.rs", "fn kept() {}\n");
    put(&e, "skip/b.rs", "fn skipped() {}\n");
    for i in 0..20 {
        put(&e, &format!("f{i}.rs"), &format!("fn f{i}() {{}}\n"));
    }
    let v = run(&e);
    assert_eq!(found(&v, "skipped").len(), 1);
    put(&e, ".gitignore", "skip/\n");
    let v = run(&e);
    assert_equals_reparse(&e, &v, &["skipped"]);
    put(&e, ".gitignore", "");
    let v = run(&e);
    assert_equals_reparse(&e, &v, &[]);
    assert_eq!(found(&v, "skipped").len(), 1);
}

#[test]
fn vanished_between_scan_and_read_is_skipped() {
    let e = env();
    let ghost = Scanned {
        path: b"ghost.rs".to_vec(),
        mtime_ns: 1,
        size: 1,
    };
    assert_eq!(entry_for(&e.root, &ghost, u64::MAX).unwrap(), None);
    put(&e, "real.rs", "fn real() {}\n");
    let real = &scan(&e.root).unwrap()[0];
    assert!(entry_for(&e.root, real, u64::MAX).unwrap().is_some());
}

#[test]
fn invalid_utf8_file_is_skipped() {
    let e = env();
    put(&e, "ok.rs", "fn ok() {}\n");
    fs::write(e.root.join("bad.rs"), b"fn bad() {}\n\xff\xfe").unwrap();
    let v = run(&e);
    assert!(found(&v, "bad").is_empty());
    assert_eq!(found(&v, "ok").len(), 1);
    assert_equals_reparse(&e, &v, &["bad"]);
}

#[test]
fn nul_in_first_8k_is_skipped_but_nul_after_is_parsed() {
    let e = env();
    fs::write(e.root.join("bin.rs"), b"fn bin() {}\n\0").unwrap();
    let mut late = b"fn late() {}\n".to_vec();
    late.resize(NUL_SCAN_BYTES, b' ');
    late.push(0);
    fs::write(e.root.join("late.rs"), &late).unwrap();
    let mut edge = b"fn edge() {}\n".to_vec();
    edge.resize(NUL_SCAN_BYTES - 1, b' ');
    edge.push(0);
    fs::write(e.root.join("edge.rs"), &edge).unwrap();
    let v = run(&e);
    assert!(found(&v, "bin").is_empty());
    assert!(found(&v, "edge").is_empty());
    assert_eq!(found(&v, "late").len(), 1);
}

#[test]
fn files_without_known_extension_are_listed_but_not_parsed() {
    let e = env();
    put(&e, "notes.txt", "fn nope() {}\n");
    put(&e, "a.rs", "fn yes() {}\n");
    let v = run(&e);
    let files = v.files();
    let names: Vec<&[u8]> = files.iter().map(|(p, _)| p.as_slice()).collect();
    assert_eq!(names, [&b"a.rs"[..], b"notes.txt"]);
    assert_eq!(files[0].1.lang, Lang::Rust as u8);
    assert_eq!(files[1].1.lang, LANG_BINARY);
    assert!(found(&v, "nope").is_empty());
}

#[test]
fn mass_change_merges_into_main() {
    let e = env();
    for i in 0..2000 {
        put(&e, &format!("m{i}.rs"), &format!("fn f{i}() {{}}\n"));
    }
    run(&e);
    for i in 0..2000 {
        put(&e, &format!("m{i}.rs"), &format!("fn g{i}() {{}}\n"));
    }
    let v = run(&e);
    assert!(!delta_path(&e).exists());
    assert!(e.cache.join(MAIN_FILE).exists());
    assert_eq!(found(&v, "g1999").len(), 1);
    assert!(found(&v, "f0").is_empty());
    assert_equals_reparse(&e, &v, &["f1999"]);
}

#[test]
fn merge_threshold_is_ten_percent_of_main() {
    let e = env();
    for i in 0..40 {
        put(&e, &format!("f{i}.rs"), &format!("fn f{i}() {{}}\n"));
    }
    run(&e);
    for i in 0..4 {
        put(&e, &format!("f{i}.rs"), &format!("fn h{i}() {{}}\n"));
    }
    let v = run(&e);
    assert!(delta_path(&e).exists(), "4 of 40 stays in the delta");
    assert_equals_reparse(&e, &v, &["f0"]);
    put(&e, "f4.rs", "fn h4() {}\n");
    let v = run(&e);
    assert!(!delta_path(&e).exists(), "5 of 40 merges");
    assert_equals_reparse(&e, &v, &["f4"]);
}

#[test]
fn should_merge_boundaries() {
    assert!(!should_merge(1000, 100_000));
    assert!(should_merge(1001, 100_000));
    assert!(!should_merge(10, 100));
    assert!(should_merge(11, 100));
    assert!(!should_merge(0, 0));
    assert!(should_merge(1, 0));
}

#[test]
fn unchanged_tree_rewrites_nothing() {
    let e = env();
    for i in 0..20 {
        put(&e, &format!("f{i}.rs"), &format!("fn f{i}() {{}}\n"));
    }
    run(&e);
    put(&e, "f1.rs", "fn other() {}\n");
    run(&e);
    let mtimes = |e: &Env| {
        [MAIN_FILE, DELTA_FILE].map(|n| fs::metadata(e.cache.join(n)).unwrap().modified().unwrap())
    };
    let before = mtimes(&e);
    std::thread::sleep(Duration::from_millis(20));
    let v = run(&e);
    assert_eq!(mtimes(&e), before);
    assert_equals_reparse(&e, &v, &["f1"]);
}

#[test]
fn stamp_rule_marks_recent_files_unconfirmed() {
    let s = |mtime_ns| Scanned {
        path: b"a".to_vec(),
        mtime_ns,
        size: 7,
    };
    let start = 10 * RACY_MARGIN_NS;
    assert_eq!(
        stamp_for(&s(start - RACY_MARGIN_NS - 1), start),
        (start - RACY_MARGIN_NS - 1, 7)
    );
    assert_eq!(
        stamp_for(&s(start - RACY_MARGIN_NS), start),
        (start - RACY_MARGIN_NS, UNCONFIRMED)
    );
    assert_eq!(stamp_for(&s(start), start), (start, UNCONFIRMED));
    assert_eq!(stamp_for(&s(start + 5), start), (start + 5, UNCONFIRMED));
    assert_eq!(stamp_for(&s(u64::MAX), start), (u64::MAX, UNCONFIRMED));
}

#[test]
fn rewrite_with_same_size_and_mtime_is_seen_while_the_file_is_recent() {
    let e = env();
    fs::write(e.root.join("a.rs"), "fn aaaa() {}\n").unwrap();
    let v = run(&e);
    assert_eq!(found(&v, "aaaa").len(), 1);
    let mtime = fs::metadata(e.root.join("a.rs"))
        .unwrap()
        .modified()
        .unwrap();
    fs::write(e.root.join("a.rs"), "fn bbbb() {}\n").unwrap();
    File::options()
        .write(true)
        .open(e.root.join("a.rs"))
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    let v = run(&e);
    assert!(found(&v, "aaaa").is_empty());
    assert_eq!(found(&v, "bbbb").len(), 1);
}

#[test]
fn confirmed_stamp_is_trusted() {
    let e = env();
    put(&e, "a.rs", "fn aaaa() {}\n");
    for i in 0..20 {
        put(&e, &format!("f{i}.rs"), &format!("fn f{i}() {{}}\n"));
    }
    run(&e);
    let mtime = fs::metadata(e.root.join("a.rs"))
        .unwrap()
        .modified()
        .unwrap();
    fs::write(e.root.join("a.rs"), "fn bbbb() {}\n").unwrap();
    File::options()
        .write(true)
        .open(e.root.join("a.rs"))
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    let v = run(&e);
    assert_eq!(found(&v, "aaaa").len(), 1, "stat unchanged, so not re-read");
}

#[test]
fn corrupt_main_is_rebuilt() {
    let e = env();
    for i in 0..20 {
        put(&e, &format!("f{i}.rs"), &format!("fn f{i}() {{}}\n"));
    }
    run(&e);
    let main = e.cache.join(MAIN_FILE);
    let mut b = fs::read(&main).unwrap();
    let n = b.len();
    b[n - 1] ^= 0xff;
    fs::write(&main, b).unwrap();
    let v = run(&e);
    assert_equals_reparse(&e, &v, &[]);
}

#[test]
fn corrupt_delta_is_rebuilt() {
    let e = env();
    for i in 0..20 {
        put(&e, &format!("f{i}.rs"), &format!("fn f{i}() {{}}\n"));
    }
    run(&e);
    put(&e, "f1.rs", "fn other() {}\n");
    run(&e);
    fs::write(delta_path(&e), b"garbage").unwrap();
    let v = run(&e);
    assert_equals_reparse(&e, &v, &["f1"]);
    assert!(!delta_path(&e).exists());
}

#[test]
fn unreadable_delta_is_rebuilt_not_ignored() {
    use std::os::unix::fs::PermissionsExt;
    let e = env();
    for i in 0..20 {
        put(&e, &format!("f{i}.rs"), &format!("fn f{i}() {{}}\n"));
    }
    run(&e);
    put(&e, "f1.rs", "fn other() {}\n");
    run(&e);
    fs::set_permissions(delta_path(&e), fs::Permissions::from_mode(0o000)).unwrap();
    let v = run(&e);
    assert_equals_reparse(&e, &v, &["f1"]);
    assert_eq!(found(&v, "other").len(), 1);
}

#[test]
fn remove_if_exists_ignores_only_missing_files() {
    let e = env();
    fs::create_dir_all(&e.cache).unwrap();
    assert!(remove_if_exists(&e.cache.join("nope")).is_ok());
    let f = e.cache.join("f");
    fs::write(&f, b"x").unwrap();
    assert!(remove_if_exists(&f).is_ok());
    assert!(!f.exists());
    let d = e.cache.join("d");
    fs::create_dir(&d).unwrap();
    assert!(remove_if_exists(&d).is_err());
}

#[test]
fn missing_main_with_stale_delta_is_rebuilt() {
    let e = env();
    for i in 0..20 {
        put(&e, &format!("f{i}.rs"), &format!("fn f{i}() {{}}\n"));
    }
    run(&e);
    put(&e, "f1.rs", "fn other() {}\n");
    run(&e);
    fs::remove_file(e.cache.join(MAIN_FILE)).unwrap();
    let v = run(&e);
    assert_equals_reparse(&e, &v, &["f1"]);
    assert!(!delta_path(&e).exists());
}

#[test]
fn lookup_orders_hits_by_path_then_position() {
    let e = env();
    for i in 0..20 {
        put(&e, &format!("f{i:02}.rs"), "fn dup() {}\nfn dup2() {}\n");
    }
    run(&e);
    put(&e, "f05.rs", "\n\nfn dup() {}\n");
    let v = run(&e);
    let hits = found(&v, "dup");
    assert_eq!(hits.len(), 20);
    let order: Vec<(Vec<u8>, u32)> = v
        .lookup(b"dup")
        .unwrap()
        .into_iter()
        .map(|f| (f.path, f.line))
        .collect();
    let mut sorted = order.clone();
    sorted.sort();
    assert_eq!(order, sorted);
}

#[test]
fn container_is_resolved_from_the_owning_index() {
    let e = env();
    for i in 0..20 {
        put(
            &e,
            &format!("f{i}.rs"),
            &format!("struct S{i};\nimpl S{i} {{ fn m{i}(&self) {{}} }}\n"),
        );
    }
    run(&e);
    put(&e, "f1.rs", "struct Zed;\nimpl Zed { fn zm(&self) {} }\n");
    let v = run(&e);
    assert_eq!(found(&v, "zm")[0].4.as_deref(), Some("Zed"));
    assert_eq!(found(&v, "m2")[0].4.as_deref(), Some("S2"));
    assert_equals_reparse(&e, &v, &["m1"]);
}

#[test]
fn concurrent_lock_holder_times_out() {
    let e = env();
    put(&e, "a.rs", "fn a() {}\n");
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
    let cache = e.cache.clone();
    let h = std::thread::spawn(move || {
        with_lock(&cache, Duration::from_secs(1), || {
            held_tx.send(()).unwrap();
            rx.recv().unwrap();
        })
        .unwrap()
        .unwrap();
    });
    held_rx.recv().unwrap();
    let r = refresh_in(&e.root, &e.cache, Duration::from_millis(50));
    assert!(matches!(r, Err(Error::Lock)));
    tx.send(()).unwrap();
    h.join().unwrap();
    assert!(Error::Lock.to_string().contains("lock"));
    assert!(
        Error::from(io::Error::other("boom"))
            .to_string()
            .contains("boom")
    );
    assert!(refresh_in(&e.root, &e.cache, Duration::from_secs(1)).is_ok());
}

#[test]
fn unreadable_directory_fails_the_refresh_and_keeps_the_index() {
    use std::os::unix::fs::PermissionsExt;
    let e = env();
    put(&e, "a.rs", "fn top() {}\n");
    put(&e, "sub/b.rs", "fn hidden() {}\n");
    assert_eq!(found(&run(&e), "hidden").len(), 1);
    let sub = e.root.join("sub");
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o000)).unwrap();
    let err = refresh_in(&e.root, &e.cache, Duration::from_secs(5));
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(err, Err(Error::Io(_))));
    assert_eq!(found(&run(&e), "hidden").len(), 1);
}

#[test]
fn unreadable_file_fails_the_refresh_and_keeps_the_index() {
    use std::os::unix::fs::PermissionsExt;
    let e = env();
    put(&e, "a.rs", "fn top() {}\n");
    put(&e, "b.rs", "fn secret() {}\n");
    assert_eq!(found(&run(&e), "secret").len(), 1);
    put(&e, "b.rs", "fn secret() {}\nfn more() {}\n");
    let b = e.root.join("b.rs");
    fs::set_permissions(&b, fs::Permissions::from_mode(0o000)).unwrap();
    let err = refresh_in(&e.root, &e.cache, Duration::from_secs(5));
    fs::set_permissions(&b, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(err, Err(Error::Io(_))));
    assert_eq!(found(&run(&e), "secret").len(), 1);
}
