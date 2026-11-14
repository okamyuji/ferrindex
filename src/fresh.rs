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
pub(crate) const MAIN_FILE: &str = "main.fidx";
const DELTA_FILE: &str = "delta.fidx";
const MERGE_MAX_FILES: usize = 1_000;
/// 差分区画が本体のファイル数のこの割合（1/MERGE_DIVISOR）を超えたら統合する。
const MERGE_DIVISOR: usize = 10;
/// 保存したスタンプが実際の stat と決して一致しないようにする印。
const UNCONFIRMED: u64 = u64::MAX;
/// mtime がこれ以内に走査へ近いファイルは、同じ刻みの内の再書き込みを stat で見分けられない。
const RACY_MARGIN_NS: u64 = 2_000_000_000;
const NUL_SCAN_BYTES: usize = 8192;
/// 部分一致の検索で引く名前の上限。大きな索引で出力と時間が膨らむのを止める。
pub const MAX_SUBSTRING_NAMES: usize = 200;

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
    pub name: Vec<u8>,
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

/// 索引を捨てて全件を作り直す。
pub fn rebuild_all(root: &Path) -> Result<View, Error> {
    let dir = cache_dir(root);
    locked(&dir, LOCK_TIMEOUT, || {
        let start = now_ns();
        rebuild(root, &dir, &scan(root), start)
    })
}

/// ロックを持ったまま走査し、索引を更新し、本体と差分区画を開いて返す。
/// 検索側は索引を自分で開かないので、本体と差分区画の世代が食い違う組は作られない。
fn refresh_in(root: &Path, dir: &Path, timeout: Duration) -> Result<View, Error> {
    locked(dir, timeout, || refresh_locked(root, dir))
}

fn locked(
    dir: &Path,
    timeout: Duration,
    f: impl FnOnce() -> io::Result<View>,
) -> Result<View, Error> {
    match with_lock(dir, timeout, f)? {
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

/// 先頭に NUL があればテキストでないとみなす。
pub(crate) fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(NUL_SCAN_BYTES)].contains(&0)
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

/// 書いた直後の索引が開けないときは `InvalidData`。作り直しでも直らない破損として呼び出し側が見分ける。
fn open_after_write(path: &Path) -> io::Result<Index> {
    Index::open(path).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
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
            if is_binary(&bytes) {
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
            name: name.to_vec(),
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

    /// 名前に `needle` を含む定義（ASCII の大小は無視）。名前は `MAX_SUBSTRING_NAMES` 種類まで。
    pub fn lookup_substring(&self, needle: &[u8]) -> Result<Vec<Found>, Corrupt> {
        let names: std::collections::BTreeSet<Vec<u8>> = self
            .parts()
            .into_iter()
            .flat_map(|(_, idx)| idx.names_containing(needle, MAX_SUBSTRING_NAMES))
            .collect();
        let mut out = Vec::new();
        for name in names.into_iter().take(MAX_SUBSTRING_NAMES) {
            out.extend(self.lookup(&name)?);
        }
        out.sort_by(|a, b| (&a.path, a.line, a.col).cmp(&(&b.path, b.line, b.col)));
        Ok(out)
    }

    /// 本体と、あれば差分区画。
    pub fn parts(&self) -> Vec<(&'static str, &Index)> {
        let mut v = vec![("main", &self.main)];
        v.extend(self.delta.as_ref().map(|d| ("delta", d)));
        v
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
mod tests;
