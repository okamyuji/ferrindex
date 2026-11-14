#![allow(dead_code)]

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

pub struct Run {
    pub code: i32,
    pub out: String,
    pub err: String,
}

impl Run {
    pub fn lines(&self) -> Vec<&str> {
        self.out.lines().collect()
    }

    /// find の各行から、タブ以降のソース行を除いたもの。
    pub fn heads(&self) -> Vec<&str> {
        self.out
            .lines()
            .map(|l| l.split('\t').next().unwrap())
            .collect()
    }
}

pub struct Repo {
    _tmp: tempfile::TempDir,
    pub root: PathBuf,
    pub xdg: PathBuf,
}

impl Repo {
    pub fn new() -> Repo {
        let tmp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(tmp.path()).unwrap();
        let root = base.join("repo");
        fs::create_dir_all(root.join(".git")).unwrap();
        let xdg = base.join("xdg");
        Repo {
            _tmp: tmp,
            root,
            xdg,
        }
    }

    pub fn write(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    /// 全ファイルの更新時刻を過去へ戻す。直近に書いたファイルは再解析の対象に残るので、
    /// 差分区画が作られる場面を試すときに使う。
    pub fn age(&self) {
        let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        for p in files_under(&self.root) {
            fs::File::options()
                .write(true)
                .open(p)
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
    }

    pub fn run(&self, args: &[&str]) -> Run {
        self.run_in(Path::new(""), args)
    }

    pub fn run_in(&self, sub: &Path, args: &[&str]) -> Run {
        run_at(&self.root.join(sub), &self.xdg, args)
    }
}

pub fn run_at(cwd: &Path, xdg: &Path, args: &[&str]) -> Run {
    let o = Command::new(env!("CARGO_BIN_EXE_ferrindex"))
        .args(args)
        .current_dir(cwd)
        .env("XDG_CACHE_HOME", xdg)
        .output()
        .unwrap();
    Run {
        code: o.status.code().unwrap(),
        out: String::from_utf8(o.stdout).unwrap(),
        err: String::from_utf8(o.stderr).unwrap(),
    }
}

pub const SAMPLE: &str = "struct Foo;\nimpl Foo {\n    fn bar(&self) {}\n}\nstruct Baz;\nimpl Baz {\n    fn bar(&self) {}\n}\nfn free_fn() {}\n";

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.file_name().is_some_and(|n| n == ".git") {
            continue;
        }
        if p.is_dir() {
            out.extend(files_under(&p));
        } else {
            out.push(p);
        }
    }
    out
}
