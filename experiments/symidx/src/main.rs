use std::{
    borrow::Cow,
    collections::{BTreeSet, HashMap},
    env, fs,
    hint::cold_path,
    io::{self, Write},
    mem::DropGuard,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
    time::{Instant, UNIX_EPOCH},
};

use ignore::WalkState;
mod probe;
use memmap2::Mmap;
use rayon::prelude::*;
use tree_sitter::Language;
use tree_sitter_tags::{TagsConfiguration, TagsContext};

type Res<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

// shortcut: 1 MiB 超（生成物・minified）は索引しない。巨大な手書きソースが要るなら設定にする。
const MAX_FILE: u64 = 1 << 20;
const MAGIC: &[u8; 8] = b"SYMIDX01";
const FILE_HDR: usize = 17; // mtime u64 + size u64 + lang u8
const DEF_HDR: usize = 9; // file u32 + line u32 + kind u8

// 並列 walk の各スレッドで LANGS（query のコンパイル）を初期化させると、更新が 0.74 秒から 3.0 秒に落ちた。
// そのため walk は拡張子表だけを見る。順序は LANGS と揃える。
const EXTS: [&[&str]; 8] = [
    &["rs"],
    &["py", "pyi"],
    &["ts", "mts", "cts"],
    &["tsx"],
    &["js", "mjs", "cjs", "jsx"],
    &["go"],
    &["rb"],
    &["java"],
];

static LANGS: LazyLock<Vec<TagsConfiguration>> = LazyLock::new(|| {
    let js = tree_sitter_javascript::TAGS_QUERY;
    // TS の tags query は JS の query を継承する前提で書かれている
    let ts = format!("{js}\n{}", tree_sitter_typescript::TAGS_QUERY);
    let specs: [(Language, &str); EXTS.len()] = [
        (tree_sitter_rust::LANGUAGE.into(), tree_sitter_rust::TAGS_QUERY),
        (tree_sitter_python::LANGUAGE.into(), tree_sitter_python::TAGS_QUERY),
        (tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), &ts),
        (tree_sitter_typescript::LANGUAGE_TSX.into(), &ts),
        (tree_sitter_javascript::LANGUAGE.into(), js),
        (tree_sitter_go::LANGUAGE.into(), tree_sitter_go::TAGS_QUERY),
        (tree_sitter_ruby::LANGUAGE.into(), tree_sitter_ruby::TAGS_QUERY),
        (tree_sitter_java::LANGUAGE.into(), tree_sitter_java::TAGS_QUERY),
    ];
    specs
        .into_iter()
        .zip(EXTS)
        .map(|((lang, q), exts)| {
            TagsConfiguration::new(lang, q, "").unwrap_or_else(|e| panic!("{exts:?} の tags query を読めない: {e}"))
        })
        .collect()
});

fn lang_of(p: &Path) -> Option<usize> {
    let ext = p.extension()?.to_str()?;
    EXTS.iter().position(|exts| exts.contains(&ext))
}

struct FileRec {
    path: PathBuf,
    mtime: u64,
    size: u64,
    lang: usize,
}

struct Def<'a> {
    name: Cow<'a, str>,
    file: u32,
    line: u32,
    kind: &'a str,
}

fn walk(roots: &[PathBuf], no_ignore: bool) -> Res<Vec<FileRec>> {
    let mut b = ignore::WalkBuilder::new(&roots[0]);
    for r in &roots[1..] {
        b.add(r);
    }
    b.standard_filters(!no_ignore);
    let out = Mutex::new(Vec::new());
    b.build_parallel().run(|| {
        Box::new(|ent| {
            if let Ok(ent) = ent
                && ent.file_type().is_some_and(|t| t.is_file())
                && let Some(lang) = lang_of(ent.path())
                && let Ok(md) = ent.metadata()
                && md.len() <= MAX_FILE
            {
                let mtime = md
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_nanos() as u64);
                let rec = FileRec { path: ent.into_path(), mtime, size: md.len(), lang };
                out.lock().unwrap().push(rec);
            }
            WalkState::Continue
        })
    });
    let mut out = out.into_inner()?;
    out.par_sort_unstable_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn parse_file(ctx: &mut TagsContext, f: &FileRec, file: u32, out: &mut Vec<Def<'static>>) -> Res<()> {
    let src = fs::read(&f.path)?;
    let cfg: &'static TagsConfiguration = &LANGS[f.lang];
    let (tags, _) = ctx.generate_tags(cfg, &src, None)?;
    for t in tags {
        let t = t?;
        if !t.is_definition {
            continue;
        }
        out.push(Def {
            name: Cow::Owned(String::from_utf8_lossy(&src[t.name_range]).into_owned()),
            file,
            line: t.span.start.row as u32 + 1,
            kind: cfg.syntax_type_name(t.syntax_type_id),
        });
    }
    Ok(())
}

fn parse_all(files: &[FileRec], ids: &[u32]) -> (Vec<Def<'static>>, usize) {
    ids.par_iter()
        .map_init(TagsContext::new, |ctx, &id| {
            let mut v = Vec::new();
            match parse_file(ctx, &files[id as usize], id, &mut v) {
                Ok(()) => (v, 0),
                Err(_) => {
                    cold_path();
                    (v, 1)
                }
            }
        })
        .reduce(
            || (Vec::new(), 0),
            |(mut a, ea), (b, eb)| {
                a.extend(b);
                (a, ea + eb)
            },
        )
}

#[derive(Default)]
struct SecW {
    offs: Vec<u64>,
    data: Vec<u8>,
}

impl SecW {
    fn push(&mut self, f: impl FnOnce(&mut Vec<u8>)) {
        self.offs.push(self.data.len() as u64);
        f(&mut self.data);
    }
    fn finish(mut self, out: &mut Vec<u8>) {
        self.offs.push(self.data.len() as u64);
        out.extend(((self.offs.len() - 1) as u32).to_le_bytes());
        self.offs.iter().for_each(|o| out.extend(o.to_le_bytes()));
        out.extend(&self.data);
    }
}

fn write_index(out_path: &Path, roots: &[PathBuf], no_ignore: bool, files: &[FileRec], mut defs: Vec<Def<'_>>) -> Res<()> {
    defs.par_sort_unstable_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    let kinds: Vec<&str> = defs.iter().map(|d| d.kind).collect::<BTreeSet<_>>().into_iter().collect();
    assert!(kinds.len() <= u8::MAX as usize, "kind が 255 種を超えた");
    let kind_id: HashMap<&str, u8> = kinds.iter().enumerate().map(|(i, k)| (*k, i as u8)).collect();

    let mut buf = Vec::with_capacity(defs.len() * 40);
    buf.extend(MAGIC);
    buf.push(no_ignore as u8);
    let mut s = SecW::default();
    roots.iter().for_each(|r| s.push(|d| d.extend(r.as_os_str().as_encoded_bytes())));
    s.finish(&mut buf);
    let mut s = SecW::default();
    for f in files {
        s.push(|d| {
            d.extend(f.mtime.to_le_bytes());
            d.extend(f.size.to_le_bytes());
            d.push(f.lang as u8);
            d.extend(f.path.as_os_str().as_encoded_bytes());
        });
    }
    s.finish(&mut buf);
    let mut s = SecW::default();
    kinds.iter().for_each(|k| s.push(|d| d.extend(k.as_bytes())));
    s.finish(&mut buf);
    let mut s = SecW::default();
    for x in &defs {
        s.push(|d| {
            d.extend(x.file.to_le_bytes());
            d.extend(x.line.to_le_bytes());
            d.push(kind_id[x.kind]);
            d.extend(x.name.as_bytes());
        });
    }
    s.finish(&mut buf);

    // 途中で失敗しても、壊れた索引も一時ファイルも残さない
    let tmp = out_path.with_added_extension("tmp");
    let guard = DropGuard::new(tmp.clone(), |p| {
        let _ = fs::remove_file(p);
    });
    fs::write(&tmp, &buf)?;
    fs::rename(&tmp, out_path)?;
    DropGuard::dismiss(guard);
    Ok(())
}

struct Sec<'a> {
    offs: &'a [u8],
    data: &'a [u8],
}

impl<'a> Sec<'a> {
    fn len(&self) -> usize {
        self.offs.len() / 8 - 1
    }
    fn off(&self, i: usize) -> usize {
        u64::from_le_bytes(self.offs[i * 8..i * 8 + 8].try_into().unwrap()) as usize
    }
    // shortcut: オフセットの単調性は検証しない。壊れた索引は panic する。外部から索引を受け取るなら検証を足す。
    fn get(&self, i: usize) -> &'a [u8] {
        &self.data[self.off(i)..self.off(i + 1)]
    }
}

fn take<'a>(b: &mut &'a [u8], n: usize) -> io::Result<&'a [u8]> {
    let (h, t) = b
        .split_at_checked(n)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "索引が途中で切れている"))?;
    *b = t;
    Ok(h)
}

fn read_sec<'a>(b: &mut &'a [u8]) -> io::Result<Sec<'a>> {
    let n = u32::from_le_bytes(take(b, 4)?.try_into().unwrap()) as usize;
    let offs = take(b, 8 * (n + 1))?;
    let total = u64::from_le_bytes(offs[n * 8..].try_into().unwrap()) as usize;
    Ok(Sec { offs, data: take(b, total)? })
}

struct View<'a> {
    no_ignore: bool,
    roots: Sec<'a>,
    files: Sec<'a>,
    kinds: Sec<'a>,
    defs: Sec<'a>,
}

struct Hit<'a> {
    path: &'a Path,
    line: u32,
    kind: &'a str,
    name: &'a str,
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn as_str(b: &[u8]) -> &str {
    std::str::from_utf8(b).unwrap_or("<非UTF-8>")
}

impl<'a> View<'a> {
    fn parse(mut b: &'a [u8]) -> io::Result<Self> {
        if take(&mut b, 8)? != MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "symidx の索引ではない"));
        }
        let no_ignore = take(&mut b, 1)?[0] == 1;
        Ok(View { no_ignore, roots: read_sec(&mut b)?, files: read_sec(&mut b)?, kinds: read_sec(&mut b)?, defs: read_sec(&mut b)? })
    }

    fn file_path(&self, id: usize) -> &'a Path {
        Path::new(as_str(&self.files.get(id)[FILE_HDR..]))
    }

    fn def(&self, i: usize) -> (u32, Hit<'a>) {
        let r = self.defs.get(i);
        let file = u32_at(r, 0);
        let hit = Hit {
            path: self.file_path(file as usize),
            line: u32_at(r, 4),
            kind: as_str(self.kinds.get(r[8] as usize)),
            name: as_str(&r[DEF_HDR..]),
        };
        (file, hit)
    }

    /// 名前の完全一致（prefix なら前方一致）を最大 limit 件 sink に渡し、渡した件数を返す。
    fn lookup<E>(&self, q: &str, prefix: bool, limit: usize, mut sink: impl FnMut(Hit<'a>) -> Result<(), E>) -> Result<usize, E> {
        let q = q.as_bytes();
        let name = |i: usize| &self.defs.get(i)[DEF_HDR..];
        let (mut lo, mut hi) = (0, self.defs.len());
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if name(mid) < q { lo = mid + 1 } else { hi = mid }
        }
        let mut n = 0;
        for i in lo..self.defs.len() {
            let hit = if prefix { name(i).starts_with(q) } else { name(i) == q };
            if !hit || n == limit {
                break;
            }
            sink(self.def(i).1)?;
            n += 1;
        }
        Ok(n)
    }
}

fn open(p: &Path) -> Res<Mmap> {
    // SAFETY: 索引は rename で置き換えるので、マップ中のファイル内容は書き換わらない
    Ok(unsafe { Mmap::map(&fs::File::open(p)?)? })
}

fn build(idx: &Path, roots: Vec<PathBuf>, no_ignore: bool) -> Res<()> {
    let t = Instant::now();
    let files = walk(&roots, no_ignore)?;
    let t_walk = t.elapsed();
    let ids: Vec<u32> = (0..files.len() as u32).collect();
    let (defs, errs) = parse_all(&files, &ids);
    let t_parse = t.elapsed();
    let n = defs.len();
    write_index(idx, &roots, no_ignore, &files, defs)?;
    eprintln!(
        "build files={} defs={n} parse_errors={errs} walk={:?} parse={:?} total={:?}",
        files.len(), t_walk, t_parse - t_walk, t.elapsed()
    );
    Ok(())
}

fn update(idx: &Path) -> Res<()> {
    let t = Instant::now();
    let mm = open(idx)?;
    let v = View::parse(&mm)?;
    let roots: Vec<PathBuf> = (0..v.roots.len()).map(|i| PathBuf::from(as_str(v.roots.get(i)))).collect();
    let cur = walk(&roots, v.no_ignore)?;
    let t_walk = t.elapsed();
    let old: HashMap<&Path, (usize, u64, u64)> = (0..v.files.len())
        .map(|i| {
            let r = v.files.get(i);
            let stamp = |at: usize| u64::from_le_bytes(r[at..at + 8].try_into().unwrap());
            (v.file_path(i), (i, stamp(0), stamp(8)))
        })
        .collect();
    let mut remap: Vec<Option<u32>> = vec![None; v.files.len()];
    let mut changed = Vec::new();
    for (new_id, f) in cur.iter().enumerate() {
        match old.get(f.path.as_path()) {
            Some(&(oid, m, s)) if (m, s) == (f.mtime, f.size) => remap[oid] = Some(new_id as u32),
            _ => changed.push(new_id as u32),
        }
    }
    let mut defs: Vec<Def<'_>> = (0..v.defs.len())
        .into_par_iter()
        .filter_map(|i| {
            let (file, h) = v.def(i);
            Some(Def { name: Cow::Borrowed(h.name), file: remap[file as usize]?, line: h.line, kind: h.kind })
        })
        .collect();
    let (fresh, errs) = parse_all(&cur, &changed);
    defs.extend(fresh);
    let n = defs.len();
    write_index(idx, &roots, v.no_ignore, &cur, defs)?;
    eprintln!(
        "update files={} reparsed={} defs={n} parse_errors={errs} walk+stat={:?} total={:?}",
        cur.len(), changed.len(), t_walk, t.elapsed()
    );
    Ok(())
}

fn usage() -> ! {
    eprintln!(
        "使い方:\n  symidx build [--no-ignore] <index> <root>...\n  symidx update <index>\n  symidx find [--prefix] <index> <name>\n  symidx files <index> <部分文字列>"
    );
    std::process::exit(2)
}

fn main() -> Res<()> {
    let mut args: Vec<String> = env::args().skip(1).collect();
    let mut flag = |f: &str| args.iter().position(|a| a == f).map(|i| args.remove(i)).is_some();
    let (no_ignore, prefix) = (flag("--no-ignore"), flag("--prefix"));
    let cwd = env::current_dir()?;
    let mut out = io::stdout().lock();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["build", idx, roots @ ..] if !roots.is_empty() => build(Path::new(idx), roots.iter().map(PathBuf::from).collect(), no_ignore)?,
        ["update", idx] => update(Path::new(idx))?,
        ["fstprobe", idx] => probe::run(Path::new(idx))?,
        ["find", idx, q] => {
            let mm = open(Path::new(idx))?;
            View::parse(&mm)?.lookup(q, prefix, 50, |h| {
                writeln!(out, "{}:{} {} {}", h.path.trim_prefix(&cwd).display(), h.line, h.kind, h.name)
            })?;
        }
        ["files", idx, q] => {
            let mm = open(Path::new(idx))?;
            let v = View::parse(&mm)?;
            let q = q.trim_prefix("./");
            for p in (0..v.files.len()).map(|i| v.file_path(i).trim_prefix(&cwd)).filter(|p| p.to_str().is_some_and(|s| s.contains(q))).take(50) {
                writeln!(out, "{}", p.display())?;
            }
        }
        _ => usage(),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = env::temp_dir().join(format!("symidx-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("src")).unwrap();
        d
    }

    fn find_all(idx: &Path, q: &str, prefix: bool) -> Vec<(String, u32, String)> {
        let mm = open(idx).unwrap();
        let v = View::parse(&mm).unwrap();
        let mut got = Vec::new();
        // 収集先は失敗しないので、エラー型を ! にして結果を unwrap なしで取り出す
        let Ok(_) = v.lookup(q, prefix, 100, |h| {
            got.push((h.path.file_name().unwrap().to_str().unwrap().to_owned(), h.line, h.kind.to_owned()));
            Ok::<(), !>(())
        });
        got
    }

    #[test]
    fn finds_definitions_in_each_language_and_updates_changed_files() {
        let d = tmpdir("langs");
        let src = d.join("src");
        let cases = [
            ("a.rs", "fn alpha() {}\nstruct Beta;\n", "alpha", 1),
            ("b.py", "def gamma():\n    pass\nclass Delta:\n    pass\n", "Delta", 3),
            ("c.ts", "function eps() {}\ninterface Zeta { x: number }\n", "Zeta", 2),
            ("c2.tsx", "function Tsx() { return null }\n", "Tsx", 1),
            ("d.go", "package x\n\nfunc Eta() {}\n", "Eta", 3),
            ("e.rb", "class Iota\nend\n", "Iota", 1),
            ("f.js", "function kappa() {}\n", "kappa", 1),
            ("g.java", "class Lam {\n  void mu() {}\n}\n", "mu", 2),
        ];
        for (f, body, ..) in cases {
            fs::write(src.join(f), body).unwrap();
        }
        fs::write(src.join("skip.txt"), "fn not_indexed() {}").unwrap();
        let idx = d.join("x.idx");
        build(&idx, vec![src.clone()], true).unwrap();
        for (f, _, name, line) in cases {
            let got = find_all(&idx, name, false);
            assert_eq!(got.len(), 1, "{name}: {got:?}");
            assert_eq!((got[0].0.as_str(), got[0].1), (f, line), "{name}");
        }
        assert!(find_all(&idx, "not_indexed", false).is_empty());
        assert_eq!(find_all(&idx, "alp", true).len(), 1);
        assert!(find_all(&idx, "alp", false).is_empty());

        fs::write(src.join("a.rs"), "// 行がずれる\nfn alpha2() {}\n").unwrap();
        update(&idx).unwrap();
        assert!(find_all(&idx, "alpha", false).is_empty());
        assert_eq!(find_all(&idx, "alpha2", false)[0].1, 2);
        assert_eq!(find_all(&idx, "Zeta", false).len(), 1, "変更していないファイルの定義が残る");
        assert!(!idx.with_added_extension("tmp").exists());
    }

    #[test]
    fn failed_write_leaves_no_temp_file() {
        let d = tmpdir("guard");
        let target = d.join("is_a_dir");
        fs::create_dir_all(target.join("child")).unwrap();
        assert!(write_index(&target, &[], false, &[], Vec::new()).is_err());
        assert!(!target.with_added_extension("tmp").exists());
    }

    #[test]
    fn rejects_truncated_index() {
        let mut b = MAGIC.to_vec();
        b.push(0);
        assert!(View::parse(&b).is_err());
        assert!(View::parse(b"NOTANIDX").is_err());
    }
}
