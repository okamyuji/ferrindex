use std::{
    fs::{File, Metadata},
    io,
    os::unix::ffi::OsStrExt,
    path::Path,
    sync::mpsc,
    time::UNIX_EPOCH,
};

use ignore::{WalkBuilder, WalkState};

pub const MAX_FILE_BYTES: u64 = 1 << 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scanned {
    /// `root` からの相対パス
    pub path: Vec<u8>,
    pub mtime_ns: u64,
    pub size: u64,
}

/// git の除外規則に従って `root` 以下の通常ファイルを集める。
/// 入れ子のリポジトリ（`.git` を持つ下位ディレクトリ）には入らない。
/// 親ディレクトリとグローバルの除外設定は読まない（結果を利用者の環境から独立させる）。
///
/// 読めないディレクトリなどの I/O エラーは返す。部分的な一覧を成功として返すと、
/// 呼び出し側が「一覧に無いファイル」を削除として索引に書いてしまう。
/// 走査中に消えたファイル（`NotFound`）と、I/O を伴わない規則の不備は無視する。
pub fn scan(root: &Path) -> io::Result<Vec<Scanned>> {
    let (tx, rx) = mpsc::channel();
    WalkBuilder::new(root)
        .hidden(false)
        .ignore(false)
        .parents(false)
        .git_global(false)
        .require_git(false)
        .filter_entry(|e| {
            e.depth() == 0
                || (e.file_name() != ".git"
                    && !(e.file_type().is_some_and(|t| t.is_dir())
                        && e.path().join(".git").exists()))
        })
        .build_parallel()
        .run(|| {
            let tx = tx.clone();
            Box::new(move |entry| {
                let item = match entry {
                    Ok(e) => scanned(root, &e).transpose(),
                    Err(e) => io_failure(&e).map(Err),
                };
                if let Some(item) = item {
                    let _ = tx.send(item);
                }
                WalkState::Continue
            })
        });
    drop(tx);
    let mut files = rx.into_iter().collect::<io::Result<Vec<Scanned>>>()?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

fn io_failure(e: &ignore::Error) -> Option<io::Error> {
    e.io_error()
        .filter(|io| io.kind() != io::ErrorKind::NotFound)
        .map(|io| io::Error::new(io.kind(), e.to_string()))
}

fn scanned(root: &Path, entry: &ignore::DirEntry) -> io::Result<Option<Scanned>> {
    // ignore は .gitignore の I/O エラーを捨てる（ignore-0.4.34 lib.rs の push_ignore_io）。
    // 読めないまま進むと除外規則が抜けたファイルを索引に入れてしまう。
    // ignore は symlink の .gitignore も辿って開くので、種類を問わず確かめる
    if entry.file_name() == ".gitignore"
        && let Err(e) = File::open(entry.path())
        && e.kind() != io::ErrorKind::NotFound
    {
        return Err(io::Error::new(
            e.kind(),
            format!("{}: {e}", entry.path().display()),
        ));
    }
    if !entry.file_type().is_some_and(|t| t.is_file()) {
        return Ok(None);
    }
    let meta = match entry.metadata() {
        Ok(m) => m,
        Err(e) => return io_failure(&e).map_or(Ok(None), Err),
    };
    if meta.len() > MAX_FILE_BYTES {
        return Ok(None);
    }
    let Ok(rel) = entry.path().strip_prefix(root) else {
        return Ok(None);
    };
    Ok(Some(Scanned {
        path: rel.as_os_str().as_bytes().to_vec(),
        mtime_ns: mtime_ns(&meta),
        size: meta.len(),
    }))
}

fn mtime_ns(meta: &Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| u64::try_from(d.as_nanos()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, File},
        os::unix::fs::symlink,
        time::{Duration, SystemTime},
    };

    use super::*;

    fn repo() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        fs::create_dir(t.path().join(".git")).unwrap();
        t
    }

    fn put(root: &Path, rel: &str, body: &[u8]) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    fn paths(root: &Path) -> Vec<String> {
        scan(root)
            .unwrap()
            .into_iter()
            .map(|s| String::from_utf8(s.path).unwrap())
            .collect()
    }

    #[test]
    fn gitignore_and_nested_repo_rules() {
        let t = repo();
        let r = t.path();
        put(r, ".gitignore", b"ignored/\n*.log\n");
        put(r, "src/a.rs", b"fn a() {}");
        put(r, "ignored/x.rs", b"");
        put(r, "src/deep/y.log", b"");
        put(r, ".hidden/h.rs", b"");
        put(r, ".git/config", b"");
        put(r, "sub/.git/config", b"");
        put(r, "sub/n.rs", b"");
        put(r, "wt/n.rs", b"");
        put(r, "wt/.git", b"gitdir: /elsewhere");
        put(r, "src/sub/.gitignore", b"local.rs\n");
        put(r, "src/sub/local.rs", b"");
        put(r, "src/sub/kept.rs", b"");
        symlink(r.join("src/a.rs"), r.join("link.rs")).unwrap();
        symlink(r.join("src"), r.join("dirlink")).unwrap();
        assert_eq!(
            paths(r),
            [
                ".gitignore",
                ".hidden/h.rs",
                "src/a.rs",
                "src/sub/.gitignore",
                "src/sub/kept.rs"
            ]
        );
    }

    #[test]
    fn root_named_dot_git_parent_is_not_filtered() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join(".git-work");
        fs::create_dir_all(root.join(".git")).unwrap();
        put(&root, "a.rs", b"");
        assert_eq!(paths(&root), ["a.rs"]);
    }

    #[test]
    fn size_limit_is_one_mib_inclusive() {
        let t = repo();
        let r = t.path();
        put(r, "exact", &vec![b'x'; MAX_FILE_BYTES as usize]);
        put(r, "over", &vec![b'x'; MAX_FILE_BYTES as usize + 1]);
        put(r, "empty", b"");
        assert_eq!(paths(r), ["empty", "exact"]);
    }

    #[test]
    fn scanned_carries_size_and_mtime() {
        let t = repo();
        let r = t.path();
        put(r, "a", b"12345");
        let when = UNIX_EPOCH + Duration::new(1_000, 123);
        File::options()
            .write(true)
            .open(r.join("a"))
            .unwrap()
            .set_modified(when)
            .unwrap();
        assert_eq!(
            scan(r).unwrap(),
            [Scanned {
                path: b"a".to_vec(),
                mtime_ns: 1_000_000_000_123,
                size: 5
            }]
        );
    }

    #[test]
    fn mtime_before_the_epoch_is_zero() {
        let t = repo();
        put(t.path(), "a", b"");
        File::options()
            .write(true)
            .open(t.path().join("a"))
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH - Duration::from_secs(5))
            .unwrap();
        assert_eq!(scan(t.path()).unwrap()[0].mtime_ns, 0);
    }

    #[test]
    fn unreadable_directory_is_an_error() {
        use std::{io, os::unix::fs::PermissionsExt};
        let t = repo();
        put(t.path(), "ok.rs", b"");
        put(t.path(), "locked/x.rs", b"");
        let locked = t.path().join("locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let got = scan(t.path());
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(got.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn unsearchable_directory_is_an_error() {
        use std::{io, os::unix::fs::PermissionsExt};
        let t = repo();
        put(t.path(), "d/x.rs", b"");
        let d = t.path().join("d");
        // 一覧は読めるが中の stat ができない（実行ビットなし）
        fs::set_permissions(&d, fs::Permissions::from_mode(0o444)).unwrap();
        let got = scan(t.path());
        fs::set_permissions(&d, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(got.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn unreadable_gitignore_is_an_error() {
        use std::{io, os::unix::fs::PermissionsExt};
        let t = repo();
        put(t.path(), "g/x.rs", b"");
        put(t.path(), "g/.gitignore", b"x.rs\n");
        let gi = t.path().join("g/.gitignore");
        fs::set_permissions(&gi, fs::Permissions::from_mode(0o000)).unwrap();
        let got = scan(t.path());
        fs::set_permissions(&gi, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(got.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn unreadable_symlinked_gitignore_is_an_error() {
        use std::{io, os::unix::fs::PermissionsExt};
        let t = repo();
        put(t.path(), "sub/s.rs", b"");
        put(t.path(), "real.ign", b"sub/\n");
        std::os::unix::fs::symlink("real.ign", t.path().join(".gitignore")).unwrap();
        let real = t.path().join("real.ign");
        fs::set_permissions(&real, fs::Permissions::from_mode(0o000)).unwrap();
        let got = scan(t.path());
        fs::set_permissions(&real, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(got.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn result_is_sorted_by_path() {
        let t = repo();
        for n in ["b", "a/z", "a/b", "c"] {
            put(t.path(), n, b"");
        }
        assert_eq!(paths(t.path()), ["a/b", "a/z", "b", "c"]);
    }
}
