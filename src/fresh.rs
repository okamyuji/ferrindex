use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fmt, fs, io,
    os::unix::ffi::OsStrExt,
    path::Path,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::codec::Corrupt;
use crate::extract::extract;
use crate::index::{FileEntry, Index, LANG_BINARY, LANG_DELETED, Stamp, write_index};
use crate::lang::lang_of;
use crate::store::{LockTimeout, cache_dir, with_lock};
use crate::walk::{Scanned, scan};

const LOCK_TIMEOUT: Duration = Duration::from_secs(20);
const MAIN_FILE: &str = "main.fidx";
const DELTA_FILE: &str = "delta.fidx";
const MERGE_MAX_FILES: usize = 1_000;
/// 差分区画が本体のファイル数のこの割合（1/MERGE_DIVISOR）を超えたら統合する。
const MERGE_DIVISOR: usize = 10;
/// 保存したスタンプが実際の stat と決して一致しないようにする印。
const UNCONFIRMED: u64 = u64::MAX;
/// mtime がこれ以内に走査へ近いファイルは、同じ刻みの内の再書き込みを stat で見分けられない。
const RACY_MARGIN_NS: u64 = 2_000_000_000;
const NUL_SCAN_BYTES: usize = 8192;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Lock,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "index I/O failed: {e}"),
            Error::Lock => LockTimeout.fmt(f),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

/// `lookup` が返す 1 件。`container` は所有する索引の名前 FST から解決済み。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub path: Vec<u8>,
    pub line: u32,
    pub col: u32,
    pub kind: u8,
    pub container: Option<Vec<u8>>,
}

/// ロックの中で開いた本体と差分区画の組。同じ走査の結果だけを見せる。
pub struct View {
    main: Index,
    delta: Option<Index>,
}

pub fn refresh(root: &Path) -> Result<View, Error> {
    refresh_in(root, &cache_dir(root), LOCK_TIMEOUT)
}

/// ロックを持ったまま走査し、索引を更新し、本体と差分区画を開いて返す。
/// 検索側は索引を自分で開かないので、本体と差分区画の世代が食い違う組は作られない。
fn refresh_in(root: &Path, dir: &Path, timeout: Duration) -> Result<View, Error> {
    match with_lock(dir, timeout, || refresh_locked(root, dir))? {
        Ok(view) => Ok(view?),
        Err(LockTimeout) => Err(Error::Lock),
    }
}

fn refresh_locked(root: &Path, dir: &Path) -> io::Result<View> {
    let start = now_ns();
    let scanned = scan(root);
    match incremental(root, dir, &scanned, start)? {
        Some(view) => Ok(view),
        None => rebuild(root, dir, &scanned, start),
    }
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

fn open_after_write(path: &Path) -> io::Result<Index> {
    Index::open(path).map_err(|e| io::Error::other(e.to_string()))
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// 破損・欠落・抽出器の不一致のときは全件を作り直す。古い結果は返さない。
fn rebuild(root: &Path, dir: &Path, scanned: &[Scanned], start: u64) -> io::Result<View> {
    let refs: Vec<&Scanned> = scanned.iter().collect();
    let entries: Vec<FileEntry> = parse_all(root, &refs, start)
        .into_iter()
        .flatten()
        .collect();
    write_index(&dir.join(MAIN_FILE), &entries)?;
    remove_if_exists(&dir.join(DELTA_FILE))?;
    Ok(View {
        main: open_after_write(&dir.join(MAIN_FILE))?,
        delta: None,
    })
}

/// 索引が使えないときは `None`（呼び出し側が作り直す）。
fn incremental(
    root: &Path,
    dir: &Path,
    scanned: &[Scanned],
    start: u64,
) -> io::Result<Option<View>> {
    let (main_path, delta_path) = (dir.join(MAIN_FILE), dir.join(DELTA_FILE));
    let Ok(main) = Index::open(&main_path) else {
        return Ok(None);
    };
    let old_delta = if delta_path.exists() {
        let Ok(d) = Index::open(&delta_path) else {
            return Ok(None);
        };
        Some(d)
    } else {
        None
    };
    let old_entries = match &old_delta {
        Some(d) => match d.entries() {
            Ok(v) => v,
            Err(Corrupt) => return Ok(None),
        },
        None => Vec::new(),
    };
    let main_stamps: BTreeMap<Vec<u8>, Stamp> = main.files().collect();
    let mut delta: BTreeMap<Vec<u8>, FileEntry> = old_entries
        .iter()
        .map(|e| (e.path.clone(), e.clone()))
        .collect();

    let known = |path: &[u8]| match delta.get(path) {
        Some(e) => (e.lang != LANG_DELETED).then_some((e.mtime_ns, e.size)),
        None => main_stamps.get(path).map(|s| (s.mtime_ns, s.size)),
    };
    let changed: Vec<&Scanned> = scanned
        .iter()
        .filter(|s| known(&s.path) != Some((s.mtime_ns, s.size)))
        .collect();
    let parsed = parse_all(root, &changed, start);

    let mut present: std::collections::BTreeSet<&[u8]> =
        scanned.iter().map(|s| s.path.as_slice()).collect();
    for (s, entry) in changed.iter().zip(parsed) {
        match entry {
            Some(e) => {
                delta.insert(e.path.clone(), e);
            }
            None => {
                present.remove(s.path.as_slice());
            }
        }
    }
    let gone: Vec<Vec<u8>> = main_stamps
        .keys()
        .chain(delta.keys())
        .filter(|p| !present.contains(p.as_slice()))
        .cloned()
        .collect();
    for path in gone {
        if main_stamps.contains_key(&path) {
            delta.insert(path.clone(), deleted(path));
        } else {
            delta.remove(&path);
        }
    }
    let new_entries: Vec<FileEntry> = delta.into_values().collect();

    if should_merge(new_entries.len(), main_stamps.len()) {
        let Ok(base) = main.entries() else {
            return Ok(None);
        };
        let mut all: BTreeMap<Vec<u8>, FileEntry> =
            base.into_iter().map(|e| (e.path.clone(), e)).collect();
        for e in new_entries {
            if e.lang == LANG_DELETED {
                all.remove(&e.path);
            } else {
                all.insert(e.path.clone(), e);
            }
        }
        let all: Vec<FileEntry> = all.into_values().collect();
        write_index(&main_path, &all)?;
        remove_if_exists(&delta_path)?;
        return Ok(Some(View {
            main: open_after_write(&main_path)?,
            delta: None,
        }));
    }

    let delta = if new_entries == old_entries {
        old_delta
    } else if new_entries.is_empty() {
        remove_if_exists(&delta_path)?;
        None
    } else {
        write_index(&delta_path, &new_entries)?;
        Some(open_after_write(&delta_path)?)
    };
    Ok(Some(View { main, delta }))
}

fn deleted(path: Vec<u8>) -> FileEntry {
    FileEntry {
        path,
        mtime_ns: 0,
        size: UNCONFIRMED,
        lang: LANG_DELETED,
        defs: Vec::new(),
    }
}

fn should_merge(delta: usize, main: usize) -> bool {
    delta > MERGE_MAX_FILES || delta * MERGE_DIVISOR > main
}

/// 走査で得た stat（読む前に取った値）を保存用のスタンプにする。
/// mtime が走査の開始に近いファイルは、同じ刻みの内の書き換えを後の stat で見分けられない。
/// 一致しない大きさを入れて、次の更新で必ず読み直させる。
fn stamp_for(s: &Scanned, start: u64) -> (u64, u64) {
    if s.mtime_ns.saturating_add(RACY_MARGIN_NS) >= start {
        (s.mtime_ns, UNCONFIRMED)
    } else {
        (s.mtime_ns, s.size)
    }
}

/// 走査から読み出しまでの間に消えたファイルは `None`。
fn entry_for(root: &Path, s: &Scanned, start: u64) -> Option<FileEntry> {
    let (mtime_ns, size) = stamp_for(s, start);
    let rel = Path::new(OsStr::from_bytes(&s.path));
    let (lang, defs) = match lang_of(rel) {
        None => (LANG_BINARY, Vec::new()),
        Some(lang) => {
            let bytes = fs::read(root.join(rel)).ok()?;
            if bytes[..bytes.len().min(NUL_SCAN_BYTES)].contains(&0) {
                (LANG_BINARY, Vec::new())
            } else {
                (lang as u8, extract(lang, &bytes))
            }
        }
    };
    Some(FileEntry {
        path: s.path.clone(),
        mtime_ns,
        size,
        lang,
        defs,
    })
}

fn parse_all(root: &Path, items: &[&Scanned], start: u64) -> Vec<Option<FileEntry>> {
    let threads = thread::available_parallelism().map_or(1, usize::from);
    let chunk = items.len().div_ceil(threads).max(1);
    thread::scope(|sc| {
        let handles: Vec<_> = items
            .chunks(chunk)
            .map(|c| {
                sc.spawn(move || {
                    c.iter()
                        .map(|s| entry_for(root, s, start))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("parse thread panicked"))
            .collect()
    })
}

fn found(idx: &Index, name: &[u8], skip: impl Fn(&[u8]) -> bool) -> Result<Vec<Found>, Corrupt> {
    Ok(idx
        .lookup(name)?
        .into_iter()
        .filter(|h| !skip(&h.path))
        .map(|h| Found {
            container: idx.container_name(&h.row),
            line: h.row.line,
            col: h.row.col,
            kind: h.row.kind,
            path: h.path,
        })
        .collect())
}

impl View {
    /// 差分区画に同じ経路があれば、本体の項目は隠れる（削除の印も同じ経路で隠す）。
    pub fn lookup(&self, name: &[u8]) -> Result<Vec<Found>, Corrupt> {
        let delta = self.delta.as_ref();
        let mut out = found(&self.main, name, |p| delta.is_some_and(|d| d.has_path(p)))?;
        if let Some(d) = delta {
            out.extend(found(d, name, |_| false)?);
        }
        out.sort_by(|a, b| (&a.path, a.line, a.col).cmp(&(&b.path, b.line, b.col)));
        Ok(out)
    }

    /// 走査で見えた全ファイル（定義を持たないものも含む）を経路の昇順で返す。
    pub fn files(&self) -> Vec<(Vec<u8>, Stamp)> {
        let mut all: BTreeMap<Vec<u8>, Stamp> = self.main.files().collect();
        for (path, stamp) in self.delta.iter().flat_map(Index::files) {
            if stamp.lang == LANG_DELETED {
                all.remove(&path);
            } else {
                all.insert(path, stamp);
            }
        }
        all.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
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
        for s in scan(root) {
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
        let scanned: Vec<Vec<u8>> = scan(&e.root).into_iter().map(|s| s.path).collect();
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
        assert_eq!(entry_for(&e.root, &ghost, u64::MAX), None);
        put(&e, "real.rs", "fn real() {}\n");
        let real = &scan(&e.root)[0];
        assert!(entry_for(&e.root, real, u64::MAX).is_some());
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
            [MAIN_FILE, DELTA_FILE]
                .map(|n| fs::metadata(e.cache.join(n)).unwrap().modified().unwrap())
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
}
