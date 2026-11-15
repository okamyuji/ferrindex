//! `--precise` が言語サーバーを起動してよいリポジトリの一覧。
//! サーバーはリポジトリのコード（build script など）を実行しうるので、利用者が端末で信頼したものだけに使う。
//! 一覧には根の正規化パスのハッシュだけを 1 行 1 件で書く。キャッシュではないのでデータの置き場に置く。

use std::{
    env,
    ffi::OsString,
    fs,
    io::{self, Write},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    time::Duration,
};

use sha2::{Digest, Sha256};

use crate::query::escape_control;
use crate::store::{with_lock, write_atomic};

const LIST: &str = "trusted";
const LOCK_TIMEOUT: Duration = Duration::from_secs(20);

/// `xdg` は絶対パスのときだけ採る（XDG の仕様）。
fn data_dir_in(xdg: Option<OsString>, home: Option<PathBuf>) -> PathBuf {
    let base = match xdg.map(PathBuf::from) {
        Some(p) if p.is_absolute() => p,
        _ => home.unwrap_or_else(env::temp_dir).join(".local/share"),
    };
    base.join("ferrindex")
}

fn data_dir() -> PathBuf {
    data_dir_in(env::var_os("XDG_DATA_HOME"), env::home_dir())
}

/// 衝突を作れるハッシュだと、信頼した根と同じ値になるディレクトリ名へ信頼が移るので SHA-256 にする。
fn key(root: &Path) -> String {
    Sha256::digest(root.as_os_str().as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn keys(dir: &Path) -> io::Result<Vec<String>> {
    match fs::read_to_string(dir.join(LIST)) {
        Ok(text) => Ok(text.lines().map(str::to_string).collect()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// 種類は `Fail` が終了コードを選ぶのに使うので保ち、どのファイルかを添える。
fn naming(dir: &Path, e: io::Error) -> io::Error {
    let path = escape_control(dir.join(LIST).as_os_str().as_bytes());
    io::Error::new(e.kind(), format!("{}: {e}", String::from_utf8_lossy(&path)))
}

/// `root` は `find_root` が返した正規化済みの根。
pub fn is_trusted(root: &Path) -> io::Result<bool> {
    is_trusted_in(&data_dir(), root)
}

fn is_trusted_in(dir: &Path, root: &Path) -> io::Result<bool> {
    Ok(keys(dir).map_err(|e| naming(dir, e))?.contains(&key(root)))
}

pub fn set(root: &Path, trusted: bool) -> io::Result<()> {
    set_in(&data_dir(), root, trusted)
}

fn set_in(dir: &Path, root: &Path, trusted: bool) -> io::Result<()> {
    let update = || {
        let k = key(root);
        let mut list = keys(dir)?;
        list.retain(|x| *x != k);
        if trusted {
            list.push(k);
        }
        write_atomic(&dir.join(LIST), |f| {
            list.iter().try_for_each(|k| writeln!(f, "{k}"))
        })
    };
    let done = with_lock(dir, LOCK_TIMEOUT, update).and_then(|r| r.map_err(io::Error::other));
    done.and_then(|r| r).map_err(|e| naming(dir, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_takes_only_an_absolute_xdg() {
        let home = Some(PathBuf::from("/h"));
        assert_eq!(
            data_dir_in(Some("/x".into()), home.clone()),
            Path::new("/x/ferrindex")
        );
        assert_eq!(
            data_dir_in(Some("rel".into()), home.clone()),
            Path::new("/h/.local/share/ferrindex")
        );
        assert_eq!(
            data_dir_in(None, home),
            Path::new("/h/.local/share/ferrindex")
        );
    }

    #[test]
    fn key_is_the_sha256_of_the_root_in_hex() {
        assert_eq!(
            key(Path::new("/r")),
            "6588117f7b5162325520a43670f6b2f80d50e9aa09f820f57274904a22ddfaf2"
        );
    }

    #[test]
    fn set_adds_once_and_removes_only_its_root() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("ferrindex");
        let (a, b) = (Path::new("/r/a"), Path::new("/r/b"));
        assert!(!is_trusted_in(&dir, a).unwrap());
        set_in(&dir, a, true).unwrap();
        set_in(&dir, a, true).unwrap();
        set_in(&dir, b, true).unwrap();
        assert!(is_trusted_in(&dir, a).unwrap() && is_trusted_in(&dir, b).unwrap());
        assert_eq!(keys(&dir).unwrap().len(), 2);
        set_in(&dir, a, false).unwrap();
        assert!(!is_trusted_in(&dir, a).unwrap());
        assert!(is_trusted_in(&dir, b).unwrap());
    }

    #[test]
    fn unreadable_list_is_an_error_naming_the_list() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("a\x1bb");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(LIST), b"\xff\n").unwrap();
        let want = format!("{}/a\\x1bb/trusted: ", d.path().display());
        for e in [
            is_trusted_in(&dir, Path::new("/r")).unwrap_err(),
            set_in(&dir, Path::new("/r"), true).unwrap_err(),
        ] {
            assert_eq!(e.kind(), io::ErrorKind::InvalidData);
            assert!(e.to_string().starts_with(&want), "{e}");
        }
    }
}
