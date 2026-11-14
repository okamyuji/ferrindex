use std::{
    ffi::OsString,
    fmt, fs,
    fs::{File, TryLockError},
    io,
    mem::DropGuard,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
const LOCK_POLL: Duration = Duration::from_millis(10);

/// 最も近い祖先の `.git` を持つディレクトリを返す。
/// 正規化するので `/tmp/x` と `/private/tmp/x` は同じルートになり、同じ `cache_dir` に落ちる。
pub fn find_root(cwd: &Path) -> Option<PathBuf> {
    let start = fs::canonicalize(cwd).ok()?;
    start
        .ancestors()
        .find(|d| d.join(".git").exists())
        .map(Path::to_path_buf)
}

pub(crate) fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(FNV_OFFSET, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(FNV_PRIME)
    })
}

/// `xdg` は絶対パスのときだけ採る（XDG の仕様）。
fn cache_dir_in(xdg: Option<OsString>, home: Option<PathBuf>, root: &Path) -> PathBuf {
    let base = match xdg.map(PathBuf::from) {
        Some(p) if p.is_absolute() => p,
        _ => home.unwrap_or_else(std::env::temp_dir).join(".cache"),
    };
    base.join("ferrindex")
        .join(format!("{:016x}", fnv1a64(root.as_os_str().as_bytes())))
}

pub fn cache_dir(root: &Path) -> PathBuf {
    cache_dir_in(
        std::env::var_os("XDG_CACHE_HOME"),
        std::env::home_dir(),
        root,
    )
}

#[derive(Debug, PartialEq, Eq)]
pub struct LockTimeout;

impl fmt::Display for LockTimeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("timed out waiting for the index lock")
    }
}

impl std::error::Error for LockTimeout {}

/// `<dir>/lock` の排他ロックを持ったまま `f` を実行する。ロックは `f` の後（panic 時も）閉じて外れる。
/// 外側の `Err` はロックファイルの作成や取得の I/O 失敗、内側の `Err` は時間切れ。
pub fn with_lock<T>(
    dir: &Path,
    timeout: Duration,
    f: impl FnOnce() -> T,
) -> io::Result<Result<T, LockTimeout>> {
    fs::create_dir_all(dir)?;
    let lock = File::create(dir.join("lock"))?;
    let deadline = Instant::now() + timeout;
    loop {
        match lock.try_lock() {
            Ok(()) => return Ok(Ok(f())),
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(e)) => return Err(e),
        }
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            return Ok(Err(LockTimeout));
        };
        thread::sleep(left.min(LOCK_POLL));
    }
}

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// 同じディレクトリの一時ファイルへ書き、fsync して rename する。
/// 読み手は旧版か新版のどちらかだけを見る。失敗や panic では一時ファイルを消す。
pub fn write_atomic(path: &Path, f: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name"))?;
    let mut tmp_name = OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let tmp = path.with_file_name(tmp_name);
    let mut file = File::create_new(&tmp)?;
    let guard = DropGuard::new(tmp, |t| {
        let _ = fs::remove_file(t);
    });
    f(&mut file)?;
    file.sync_all()?;
    fs::rename(&*guard, path)?;
    DropGuard::dismiss(guard);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{io::Write, sync::mpsc};

    use super::*;

    fn listing(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn root_from_subdir() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("a/b")).unwrap();
        fs::create_dir(d.path().join("a/.git")).unwrap();
        let want = fs::canonicalize(d.path().join("a")).unwrap();
        assert_eq!(find_root(&d.path().join("a/b")), Some(want.clone()));
        assert_eq!(find_root(&d.path().join("a")), Some(want));
    }

    #[test]
    fn root_accepts_git_file_and_picks_nearest() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join(".git")).unwrap();
        fs::create_dir_all(d.path().join("sub/deep")).unwrap();
        fs::write(d.path().join("sub/.git"), "gitdir: x").unwrap();
        let want = fs::canonicalize(d.path().join("sub")).unwrap();
        assert_eq!(find_root(&d.path().join("sub/deep")), Some(want));
    }

    #[test]
    fn no_root_outside_git() {
        let d = tempfile::tempdir().unwrap();
        // 一時ディレクトリの祖先に .git が無いことを前提にする。
        assert_eq!(find_root(d.path()), None);
        assert_eq!(find_root(&d.path().join("missing")), None);
    }

    #[test]
    fn cache_dir_respects_xdg() {
        let root = Path::new("/r/x");
        let hex = format!("{:016x}", fnv1a64(b"/r/x"));
        let xdg = cache_dir_in(Some("/xdg".into()), Some("/home/u".into()), root);
        assert_eq!(xdg, Path::new("/xdg/ferrindex").join(&hex));
        let rel = cache_dir_in(Some("rel".into()), Some("/home/u".into()), root);
        assert_eq!(rel, Path::new("/home/u/.cache/ferrindex").join(&hex));
        let none = cache_dir_in(None, Some("/home/u".into()), root);
        assert_eq!(none, rel);
        let no_home = cache_dir_in(None, None, root);
        assert_eq!(
            no_home,
            std::env::temp_dir().join(".cache/ferrindex").join(&hex)
        );
        assert!(cache_dir(root).ends_with(Path::new("ferrindex").join(&hex)));
    }

    #[test]
    fn fnv1a64_known_vectors() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn lock_runs_closure_and_creates_dir() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("n/e/w");
        let r = with_lock(&dir, Duration::from_secs(1), || 7).unwrap();
        assert_eq!(r, Ok(7));
        assert!(dir.join("lock").is_file());
        // 解放済みなので再取得できる
        assert_eq!(with_lock(&dir, Duration::ZERO, || 8).unwrap(), Ok(8));
    }

    #[test]
    fn lock_timeout() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().to_path_buf();
        let (held_tx, held_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let holder = {
            let dir = dir.clone();
            thread::spawn(move || {
                with_lock(&dir, Duration::from_secs(1), || {
                    held_tx.send(()).unwrap();
                    done_rx.recv().unwrap();
                })
                .unwrap()
                .unwrap();
            })
        };
        held_rx.recv().unwrap();
        let t = Instant::now();
        let r = with_lock(&dir, Duration::from_millis(100), || ()).unwrap();
        assert_eq!(r, Err(LockTimeout));
        assert!(t.elapsed() >= Duration::from_millis(100));
        assert!(t.elapsed() < Duration::from_secs(5));
        done_tx.send(()).unwrap();
        holder.join().unwrap();
        assert_eq!(with_lock(&dir, Duration::ZERO, || ()).unwrap(), Ok(()));
    }

    #[test]
    fn lock_timeout_message() {
        assert_eq!(
            LockTimeout.to_string(),
            "timed out waiting for the index lock"
        );
    }

    #[test]
    fn lock_waits_for_release() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().to_path_buf();
        let (held_tx, held_rx) = mpsc::channel();
        let holder = {
            let dir = dir.clone();
            thread::spawn(move || {
                with_lock(&dir, Duration::from_secs(1), || {
                    held_tx.send(()).unwrap();
                    thread::sleep(Duration::from_millis(150));
                })
                .unwrap()
                .unwrap();
            })
        };
        held_rx.recv().unwrap();
        let t = Instant::now();
        assert_eq!(
            with_lock(&dir, Duration::from_secs(10), || 1).unwrap(),
            Ok(1)
        );
        assert!(t.elapsed() >= Duration::from_millis(50));
        holder.join().unwrap();
    }

    #[test]
    fn lock_io_error_is_reported() {
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("f");
        fs::write(&file, "").unwrap();
        assert!(with_lock(&file, Duration::from_secs(1), || ()).is_err());
    }

    #[test]
    fn write_atomic_creates_and_replaces() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("out.bin");
        write_atomic(&p, |f| f.write_all(b"one")).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"one");
        write_atomic(&p, |f| f.write_all(b"two")).unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        assert_eq!(listing(d.path()), ["out.bin"]);
    }

    #[test]
    fn failed_write_leaves_no_temp() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("out.bin");
        fs::write(&p, "old").unwrap();
        let e = write_atomic(&p, |f| {
            f.write_all(b"partial")?;
            Err(io::Error::other("boom"))
        })
        .unwrap_err();
        assert_eq!(e.to_string(), "boom");
        assert_eq!(fs::read(&p).unwrap(), b"old");
        assert_eq!(listing(d.path()), ["out.bin"]);
    }

    #[test]
    fn panicking_write_leaves_no_temp() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("out.bin");
        let r = std::panic::catch_unwind(|| write_atomic(&p, |_| panic!("boom")));
        assert!(r.is_err());
        assert!(listing(d.path()).is_empty());
    }

    #[test]
    fn failed_rename_leaves_no_temp() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("target");
        fs::create_dir(&p).unwrap();
        fs::write(p.join("keep"), "").unwrap();
        // 非空ディレクトリへの rename は失敗する
        assert!(write_atomic(&p, |f| f.write_all(b"x")).is_err());
        assert_eq!(listing(d.path()), ["target"]);
    }

    #[test]
    fn write_atomic_rejects_pathless_target() {
        let e = write_atomic(Path::new("/"), |_| Ok(())).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn temp_names_are_unique_within_a_process() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("out.bin");
        let mut seen = Vec::new();
        write_atomic(&p, |_| {
            write_atomic(&p, |_| {
                seen = listing(d.path());
                Ok(())
            })
        })
        .unwrap();
        assert_eq!(seen.len(), 2);
        assert_ne!(seen[0], seen[1]);
    }
}
