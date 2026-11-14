mod common;

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, UNIX_EPOCH},
};

use common::{Repo, Run, run_at};

const DEFAULT_OPS: usize = 150;
const LONG_OPS: usize = 1_000;
const NAME_POOL: u64 = 14;
const DIRS: [&str; 3] = ["d0", "d0/e", "d1"];
const FILES: [&str; 5] = ["f0.rs", "f1.rs", "f2.py", "f3.rs", "f4.py"];
const NESTED: &str = "d1/nested";
const PARALLEL_EVERY: usize = 25;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[usize::try_from(self.below(xs.len() as u64)).unwrap()]
    }
}

struct World {
    repo: Repo,
    reference_xdg: PathBuf,
    rng: Rng,
    /// 更新時刻が全ファイルで別々の過去の値になるようにする。同じ値や最近の値だと
    /// 全ファイルが「走査に近すぎる」扱いになり、差分区画の経路を通らない。
    clock: u64,
    delta_seen: bool,
}

impl World {
    fn new(seed: u64) -> World {
        let repo = Repo::new();
        let reference_xdg = repo.xdg.parent().unwrap().join("reference-xdg");
        World {
            repo,
            reference_xdg,
            rng: Rng(seed),
            clock: 1_000_000_000,
            delta_seen: false,
        }
    }

    fn body(&mut self, py: bool) -> String {
        let lines = self.rng.below(4);
        (0..=lines)
            .map(|_| {
                let n = self.rng.below(NAME_POOL);
                if py {
                    format!("def name_{n}():\n    pass\n")
                } else {
                    format!("fn name_{n}() {{}}\n")
                }
            })
            .collect()
    }

    fn put(&mut self, rel: &str, text: &str) {
        self.repo.write(rel, text);
        self.stamp(rel);
    }

    fn stamp(&mut self, rel: &str) {
        self.clock += 1 + self.rng.below(5_000);
        let t = UNIX_EPOCH + Duration::from_secs(self.clock);
        fs::File::options()
            .write(true)
            .open(self.repo.root.join(rel))
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    fn random_path(&mut self) -> String {
        let (d, f) = (*self.rng.pick(&DIRS), *self.rng.pick(&FILES));
        format!("{d}/{f}")
    }

    fn existing(&mut self) -> Option<String> {
        let mut all = Vec::new();
        for d in DIRS {
            for f in FILES {
                let rel = format!("{d}/{f}");
                if self.repo.root.join(&rel).is_file() {
                    all.push(rel);
                }
            }
        }
        (!all.is_empty()).then(|| self.rng.pick(&all).clone())
    }

    fn step(&mut self) {
        match self.rng.below(10) {
            0..=2 => self.edit_existing(),
            3..=4 => self.create(),
            5 => self.remove(),
            6 => self.rename(),
            7 => self.edit_gitignore(),
            8 => self.toggle_nested_repo(),
            // 内容は同じで時刻だけ進める。stat の変化だけで再解析に回る経路を通す。
            _ => self.touch(),
        }
    }

    fn edit_existing(&mut self) {
        if let Some(rel) = self.existing() {
            let text = self.body(rel.ends_with(".py"));
            self.put(&rel, &text);
        }
    }

    fn create(&mut self) {
        let rel = self.random_path();
        let text = self.body(rel.ends_with(".py"));
        self.put(&rel, &text);
    }

    fn remove(&mut self) {
        if let Some(rel) = self.existing() {
            fs::remove_file(self.repo.root.join(rel)).unwrap();
        }
    }

    fn rename(&mut self) {
        let to = self.random_path();
        if let Some(from) = self.existing().filter(|f| *f != to) {
            fs::rename(self.repo.root.join(&from), self.repo.root.join(&to)).unwrap();
        }
    }

    fn edit_gitignore(&mut self) {
        let rules = [
            "",
            "d1/\n",
            "*.py\n",
            "d0/e/\n",
            "f1.rs\n",
            "!f2.py\n*.py\n",
        ];
        let text = *self.rng.pick(&rules);
        self.put(".gitignore", text);
    }

    fn touch(&mut self) {
        if let Some(rel) = self.existing() {
            self.stamp(&rel);
        }
    }

    fn toggle_nested_repo(&mut self) {
        let nested = self.repo.root.join(NESTED);
        if nested.exists() {
            fs::remove_dir_all(nested).unwrap();
        } else {
            fs::create_dir_all(nested.join(".git")).unwrap();
            let text = self.body(false);
            self.put(&format!("{NESTED}/n.rs"), &text);
        }
    }

    fn queries(&mut self) -> Vec<Vec<String>> {
        let n = self.rng.below(NAME_POOL);
        let m = self.rng.below(NAME_POOL);
        let s = |v: &[&str]| v.iter().map(ToString::to_string).collect::<Vec<_>>();
        vec![
            s(&["find", "name_", "--limit", "1000"]),
            s(&["find", &format!("name_{n}")]),
            s(&["find", &format!("name_{m}"), "--limit", "1000"]),
            s(&["show", &format!("name_{n}")]),
            s(&["files", ".", "--limit", "1000"]),
            s(&["files", "d0/", "--limit", "1000"]),
            s(&["files", ".py"]),
            s(&["map", "--limit", "1000"]),
            s(&["map", "d0"]),
        ]
    }

    fn run_main(&self, q: &[String]) -> Run {
        let a: Vec<&str> = q.iter().map(String::as_str).collect();
        run_at(&self.repo.root, &self.repo.xdg, &a)
    }

    fn run_reference(&self, q: &[String]) -> Run {
        let a: Vec<&str> = q.iter().map(String::as_str).collect();
        run_at(&self.repo.root, &self.reference_xdg, &a)
    }

    fn compare(&mut self, op: usize) {
        let rebuilt = run_at(
            &self.repo.root,
            &self.reference_xdg,
            &["update", "--rebuild"],
        );
        assert_eq!(rebuilt.code, 0, "op {op}: {}", rebuilt.err);
        for q in self.queries() {
            let (got, want) = (self.run_main(&q), self.run_reference(&q));
            assert_eq!(
                (got.code, &got.out),
                (want.code, &want.out),
                "op {op}: {q:?} differs from a rebuilt index\nstderr: {}",
                got.err
            );
        }
        self.delta_seen |= has_delta(&self.repo.xdg);
        if op.is_multiple_of(PARALLEL_EVERY) {
            self.parallel_finds(op);
        }
    }

    fn parallel_finds(&mut self, op: usize) {
        let q = ["find", "name_", "--limit", "1000"];
        let spawn = || {
            Command::new(env!("CARGO_BIN_EXE_ferrindex"))
                .args(q)
                .current_dir(&self.repo.root)
                .env("XDG_CACHE_HOME", &self.repo.xdg)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        };
        let (a, b) = (spawn(), spawn());
        let want = self.run_reference(&q.map(String::from));
        for child in [a, b] {
            let o = child.wait_with_output().unwrap();
            assert_eq!(
                (
                    o.status.code().unwrap(),
                    String::from_utf8(o.stdout).unwrap()
                ),
                (want.code, want.out.clone()),
                "op {op}: parallel find differs"
            );
        }
    }
}

fn has_delta(dir: &Path) -> bool {
    fs::read_dir(dir).is_ok_and(|rd| {
        rd.flatten().any(|e| {
            let p = e.path();
            if p.is_dir() {
                has_delta(&p)
            } else {
                p.file_name().is_some_and(|n| n == "delta.fidx")
            }
        })
    })
}

fn differential(ops: usize, seed: u64) {
    let mut w = World::new(seed);
    // 初期状態は 12 ファイル以上にして、1 ファイルの変更が差分区画に収まる（本体の 1/10 以下）ようにする。
    for i in 0..30 {
        let rel = format!("seed/s{i}.rs");
        let text = w.body(false);
        w.put(&rel, &text);
    }
    w.compare(0);
    for op in 1..=ops {
        w.step();
        w.compare(op);
    }
    assert!(w.delta_seen, "the delta path was never exercised");
}

#[test]
fn matches_rebuilt_index_after_random_edits() {
    differential(DEFAULT_OPS, 0x9e37_79b9_7f4a_7c15);
}

#[test]
#[ignore = "long: run with --ignored"]
fn matches_rebuilt_index_after_1000_random_edits() {
    differential(LONG_OPS, 0x1234_5678_9abc_def1);
}
