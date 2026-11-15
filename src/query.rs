use std::{
    collections::{HashMap, hash_map::Entry},
    ffi::{OsStr, OsString},
    fmt::Display,
    fs,
    hash::Hash,
    io::{self, IsTerminal, Write},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

use crate::codec::Corrupt;
use crate::describe::{description, tokens};
use crate::extract::extract;
use crate::fresh::{self, Found, View, is_binary};
use crate::index::contains_ignore_ascii_case;
use crate::lang::{Lang, kind_name, lang_of};
use crate::lsp;
use crate::refcmd::{self, RefKind};
use crate::store::find_root;
use crate::trust;
use crate::walk::MAX_FILE_BYTES;

const EXIT_NONE: u8 = 1;
const EXIT_USAGE: u8 = 2;
const EXIT_UNUSABLE: u8 = 3;
const EXIT_IO: u8 = 4;

const DEFAULT_LIMIT: usize = 20;
const SOURCE_LINE_CHARS: usize = 160;
const SHOW_LIMIT: usize = 3;
const MAP_LIMIT: usize = 200;
const SHOW_MAX_LINES: usize = 200;

const USAGE: &str = "usage: ferrindex update [--rebuild] [--stats] | trust | untrust | servers | find <name> [--limit N] | files <substr> [--limit N] | outline <file> | show <name> [--limit N] | map [<dir>] [--limit N] | refs <name> [--limit N] [--precise] | callers <name> [--limit N] [--precise] | impls <name> [--limit N] [--precise] | supers <name> [--limit N] [--precise]";

pub(crate) struct Fail {
    code: u8,
    msg: String,
}

impl Fail {
    fn usage(msg: impl Display) -> Fail {
        Fail {
            code: EXIT_USAGE,
            msg: format!("ferrindex: {msg}\n{USAGE}"),
        }
    }

    fn unusable(reason: impl Display, hint: &str) -> Fail {
        Fail {
            code: EXIT_UNUSABLE,
            msg: format!("ferrindex: {reason}; use `{hint}` instead"),
        }
    }

    fn io(msg: impl Display) -> Fail {
        Fail {
            code: EXIT_IO,
            msg: format!("ferrindex: {msg}"),
        }
    }

    fn fresh(e: fresh::Error, hint: &str) -> Fail {
        match e {
            fresh::Error::Io(e) if e.kind() != io::ErrorKind::InvalidData => Fail::io(e),
            e => Fail::unusable(e, hint),
        }
    }
}

impl From<io::Error> for Fail {
    /// `| head` が先に閉じるのは失敗ではない。
    fn from(e: io::Error) -> Fail {
        if e.kind() == io::ErrorKind::BrokenPipe {
            Fail {
                code: 0,
                msg: String::new(),
            }
        } else {
            Fail::io(e)
        }
    }
}

pub(crate) type Res<T = bool> = Result<T, Fail>;

enum Cmd {
    Update {
        rebuild: bool,
        stats: bool,
    },
    Find(String),
    Files(String),
    Outline(PathBuf),
    Show(String),
    Map(Option<PathBuf>),
    Refs(RefKind, String),
    /// `--precise` に言語サーバーの起動を許すか。
    Trust(bool),
    Servers,
}

struct Cli {
    cmd: Cmd,
    limit: Option<usize>,
    precise: bool,
}

fn term(arg: &OsStr) -> Res<String> {
    let t = arg
        .to_str()
        .ok_or_else(|| Fail::usage("search term is not UTF-8"))?;
    if t.trim().is_empty() {
        return Err(Fail::usage("search term is empty"));
    }
    Ok(t.to_string())
}

#[derive(Default)]
struct Flags {
    limit: Option<usize>,
    rebuild: bool,
    stats: bool,
    precise: bool,
}

/// フラグを取り除いた位置引数を返す。
fn split_flags(rest: &[OsString]) -> Res<(Flags, Vec<&OsString>)> {
    let mut flags = Flags::default();
    let mut pos = Vec::new();
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.to_str() {
            Some("--limit") => flags.limit = Some(limit_value(it.next())?),
            Some("--rebuild") => flags.rebuild = true,
            Some("--stats") => flags.stats = true,
            Some("--precise") => flags.precise = true,
            Some(f) if f.starts_with("--") => {
                return Err(Fail::usage(format!("unknown flag {f}")));
            }
            _ => pos.push(a),
        }
    }
    Ok((flags, pos))
}

fn limit_value(v: Option<&OsString>) -> Res<usize> {
    v.and_then(|v| v.to_str()?.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .ok_or_else(|| Fail::usage("--limit needs a positive number"))
}

fn command(name: &OsStr, pos: &[&OsString], flags: &Flags) -> Res<Cmd> {
    let (rebuild, stats) = (flags.rebuild, flags.stats);
    Ok(match (name.to_str(), pos) {
        (Some("update"), []) => Cmd::Update { rebuild, stats },
        (Some("find"), [t]) => Cmd::Find(term(t)?),
        (Some("files"), [t]) => Cmd::Files(term(t)?),
        (Some("show"), [t]) => Cmd::Show(term(t)?),
        (Some("outline"), [f]) => Cmd::Outline(PathBuf::from(f)),
        (Some("refs"), [t]) => Cmd::Refs(RefKind::Refs, term(t)?),
        (Some("callers"), [t]) => Cmd::Refs(RefKind::Callers, term(t)?),
        (Some("impls"), [t]) => Cmd::Refs(RefKind::Impls, term(t)?),
        (Some("supers"), [t]) => Cmd::Refs(RefKind::Supers, term(t)?),
        (Some("map"), []) => Cmd::Map(None),
        (Some("map"), [d]) => Cmd::Map(Some(PathBuf::from(d))),
        (Some("trust"), []) => Cmd::Trust(true),
        (Some("untrust"), []) => Cmd::Trust(false),
        (Some("servers"), []) => Cmd::Servers,
        (
            Some(
                "update" | "find" | "files" | "show" | "outline" | "map" | "refs" | "callers"
                | "impls" | "supers" | "trust" | "untrust" | "servers",
            ),
            _,
        ) => {
            return Err(Fail::usage("wrong number of arguments"));
        }
        _ => return Err(Fail::usage("unknown subcommand")),
    })
}

fn parse(args: &[OsString]) -> Res<Cli> {
    let (name, rest) = args
        .split_first()
        .ok_or_else(|| Fail::usage("missing subcommand"))?;
    let (flags, pos) = split_flags(rest)?;
    let cmd = command(name, &pos, &flags)?;
    let update = matches!(cmd, Cmd::Update { .. });
    if (flags.rebuild || flags.stats) && !update {
        return Err(Fail::usage("--rebuild and --stats belong to update"));
    }
    if flags.limit.is_some()
        && (update || matches!(cmd, Cmd::Outline(_) | Cmd::Trust(_) | Cmd::Servers))
    {
        return Err(Fail::usage("--limit does not apply to this subcommand"));
    }
    if flags.precise && !matches!(cmd, Cmd::Refs(..)) {
        return Err(Fail::usage(
            "--precise belongs to refs, callers, impls and supers",
        ));
    }
    Ok(Cli {
        cmd,
        limit: flags.limit,
        precise: flags.precise,
    })
}

/// 終了コード: 0 結果あり、1 結果なし、2 使い方の誤り、3 索引が使えない、4 その他の I/O。
pub fn run(args: &[OsString], cwd: &Path, out: &mut impl Write, err: &mut impl Write) -> u8 {
    let result = parse(args)
        .and_then(|cli| execute(&cli, cwd, out, err))
        .and_then(|found| out.flush().map(|()| found).map_err(Fail::from));
    match result {
        Ok(found) => u8::from(!found),
        Err(f) => {
            if !f.msg.is_empty() {
                let _ = writeln!(err, "{}", f.msg);
            }
            f.code
        }
    }
}

/// 検索の前後で変わらない情報。表示する経路は、cwd が根の下にあれば cwd からの相対にする。
pub(crate) struct Ctx {
    pub(crate) root: PathBuf,
    cwd: PathBuf,
    prefix: Vec<u8>,
    hint: String,
}

impl Ctx {
    fn new(cwd: &Path, hint: String) -> Res<Ctx> {
        let root =
            find_root(cwd).ok_or_else(|| Fail::unusable("not inside a git repository", &hint))?;
        let cwd = fs::canonicalize(cwd).map_err(Fail::io)?;
        let prefix = dir_prefix(cwd.strip_prefix(&root).unwrap_or(Path::new("")));
        Ok(Ctx {
            root,
            cwd,
            prefix,
            hint,
        })
    }

    fn view(&self) -> Res<View> {
        fresh::refresh(&self.root).map_err(|e| Fail::fresh(e, &self.hint))
    }

    pub(crate) fn display(&self, path: &[u8]) -> Vec<u8> {
        escape_control(&relative_to(&self.prefix, path))
    }
}

/// 根相対のファイルの中身。索引の後に消えたファイルは `None`。権限などで読めなければ、
/// 黙って結果から落とさず、パスを添えた I/O エラー（終了コード 4）にする。
pub(crate) fn read_rel(root: &Path, path: &[u8]) -> Res<Option<Vec<u8>>> {
    match fs::read(root.join(OsStr::from_bytes(path))) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Fail::io(format!(
            "{}: {e}",
            String::from_utf8_lossy(&escape_control(path))
        ))),
    }
}

/// 失敗しうる `or_insert_with`。
pub(crate) fn cached<K: Eq + Hash, V>(
    map: &mut HashMap<K, V>,
    key: K,
    load: impl FnOnce() -> Res<V>,
) -> Res<&mut V> {
    Ok(match map.entry(key) {
        Entry::Occupied(e) => e.into_mut(),
        Entry::Vacant(e) => e.insert(load()?),
    })
}

/// 出力は 1 行 1 件で端末にも出るので、パスや名前の改行、ESC などの制御文字は文字列に置き換える。
/// C1 制御文字（CSI の U+009B など）と、表示の順を入れ替える双方向の制御文字も置き換える。
/// UTF-8 として壊れたバイトはそのまま出す。
pub(crate) fn escape_control(path: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(path.len());
    for chunk in path.utf8_chunks() {
        for c in chunk.valid().chars() {
            match c {
                '\n' => out.extend_from_slice(b"\\n"),
                '\0'..'\u{20}' | '\u{7f}' => out.extend(format!("\\x{:02x}", c as u32).bytes()),
                '\u{80}'..='\u{9f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => {
                    out.extend(format!("\\u{{{:x}}}", c as u32).bytes());
                }
                _ => out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
            }
        }
        out.extend_from_slice(chunk.invalid());
    }
    out
}

/// 根相対の `path` を、根相対のディレクトリ `dir_prefix`（`a/b/` の形、根は空）からの相対にする。
/// 外のファイルは `../` を付ける。
fn relative_to(dir_prefix: &[u8], path: &[u8]) -> Vec<u8> {
    let dirs: Vec<&[u8]> = dir_prefix
        .split(|&b| b == b'/')
        .filter(|c| !c.is_empty())
        .collect();
    let parts: Vec<&[u8]> = path.split(|&b| b == b'/').collect();
    let dir_parts = &parts[..parts.len() - 1];
    let common = dirs
        .iter()
        .zip(dir_parts)
        .take_while(|(a, b)| a == b)
        .count();
    let mut out = b"../".repeat(dirs.len() - common);
    out.extend(parts[common..].join(&b'/'));
    out
}

/// 根からの相対ディレクトリを、経路の前方一致に使う `a/b/` の形にする。根は空。
fn dir_prefix(rel: &Path) -> Vec<u8> {
    let mut p = rel.as_os_str().as_bytes().to_vec();
    if !p.is_empty() {
        p.push(b'/');
    }
    p
}

fn grep_hint(term: &str) -> String {
    format!("rg -n '{}'", term.replace('\'', "'\\''"))
}

fn execute(cli: &Cli, cwd: &Path, out: &mut impl Write, err: &mut impl Write) -> Res {
    let limit = |default| cli.limit.unwrap_or(default);
    match &cli.cmd {
        Cmd::Outline(file) => outline(file, out),
        Cmd::Trust(on) => trust_root(cwd, *on, out),
        Cmd::Servers => {
            let root = root_of(cwd)?;
            let view = fresh::refresh(&root).map_err(|e| Fail::fresh(e, "rg --files"))?;
            servers(&root, &view, out)
        }
        Cmd::Update { rebuild, stats } => {
            let ctx = Ctx::new(cwd, "rg --files".into())?;
            let view = if *rebuild {
                fresh::rebuild_all(&ctx.root).map_err(|e| Fail::fresh(e, &ctx.hint))?
            } else {
                ctx.view()?
            };
            update_report(&view, *stats, out)?;
            Ok(true)
        }
        Cmd::Find(t) => {
            let ctx = Ctx::new(cwd, grep_hint(t))?;
            let hits = resolve(&ctx.view()?, t).map_err(|c| corrupt(c, &ctx))?;
            let shown = limit(DEFAULT_LIMIT);
            let mut texts: HashMap<&[u8], Option<Vec<u8>>> = HashMap::new();
            // 表示しない行のためにファイルを読まない
            let lines: Vec<Vec<u8>> = hits
                .iter()
                .take(shown)
                .map(|f| -> Res<Vec<u8>> {
                    let mut l = def_line(
                        &ctx.display(&f.path),
                        f.line,
                        f.col,
                        f.kind,
                        f.container.as_deref(),
                        &f.name,
                    );
                    if let Some(src) = cached(&mut texts, &f.path, || read_rel(&ctx.root, &f.path))?
                        .as_deref()
                        .and_then(|t| source_line(t, f.line))
                    {
                        l.push(b'\t');
                        l.extend(src);
                    }
                    Ok(l)
                })
                .collect::<Res<_>>()?;
            emit(out, err, &lines, hits.len())
        }
        Cmd::Files(t) => {
            let ctx = Ctx::new(cwd, "rg --files".into())?;
            let mut lines: Vec<Vec<u8>> = ctx
                .view()?
                .files()
                .into_iter()
                .filter(|(p, _)| contains_ignore_ascii_case(p, t.as_bytes()))
                .map(|(p, _)| ctx.display(&p))
                .collect();
            let total = lines.len();
            lines.truncate(limit(DEFAULT_LIMIT));
            emit(out, err, &lines, total)
        }
        Cmd::Show(t) => {
            let ctx = Ctx::new(cwd, grep_hint(t))?;
            let hits = resolve(&ctx.view()?, t).map_err(|c| corrupt(c, &ctx))?;
            let max = limit(SHOW_LIMIT);
            for f in hits.iter().take(max) {
                show_one(&ctx, f, out)?;
            }
            more(err, hits.len().saturating_sub(max))?;
            Ok(!hits.is_empty())
        }
        Cmd::Refs(kind, t) => {
            let ctx = Ctx::new(cwd, grep_hint(t))?;
            refcmd::run(
                &ctx,
                &ctx.view()?,
                (*kind, t, cli.precise),
                limit(DEFAULT_LIMIT),
                out,
                err,
            )
        }
        Cmd::Map(dir) => map(
            &Ctx::new(cwd, "rg --files".into())?,
            dir.as_deref(),
            limit(MAP_LIMIT),
            out,
            err,
        ),
    }
}

pub(crate) fn corrupt(_: Corrupt, ctx: &Ctx) -> Fail {
    Fail::unusable("index is corrupt", &ctx.hint)
}

/// 完全一致、`Container.name`、名前の部分一致（ASCII の大小無視）の順に試す。
/// Markdown の見出しは `.` を含み得るので、全体の完全一致を先に引く。
/// 修飾の区切りは `::` も `.` と同じに扱う。
pub(crate) fn resolve(view: &View, term: &str) -> Result<Vec<Found>, Corrupt> {
    let hits = resolve_exact(view, term)?;
    if !hits.is_empty() {
        return Ok(hits);
    }
    let (container, name) = split_qualified(term);
    let keep = |f: &Found| {
        container
            .as_ref()
            .is_none_or(|c| f.container.as_deref() == Some(c.as_bytes()))
    };
    let mut hits = view.lookup_substring(name.as_bytes())?;
    hits.retain(keep);
    if hits.is_empty() && container.is_some() {
        // `Rust 1.100` のように `.` が名前の一部のとき
        hits = view.lookup_substring(term.as_bytes())?;
    }
    Ok(hits)
}

/// 名前の完全一致と `Container.name` だけを引く。部分一致には広げない。
pub(crate) fn resolve_exact(view: &View, term: &str) -> Result<Vec<Found>, Corrupt> {
    let hits = view.lookup(term.as_bytes())?;
    if !hits.is_empty() {
        return Ok(hits);
    }
    let (container, name) = split_qualified(term);
    let Some(container) = container else {
        return Ok(hits);
    };
    let mut hits = view.lookup(name.as_bytes())?;
    hits.retain(|f| f.container.as_deref() == Some(container.as_bytes()));
    Ok(hits)
}

/// `A::B.c` は（`A.B`、`c`）。区切りが無ければ（None、全体）。
fn split_qualified(term: &str) -> (Option<String>, String) {
    let dotted = term.replace("::", ".");
    match dotted.rsplit_once('.') {
        Some((c, n)) if !c.is_empty() && !n.is_empty() => (Some(c.to_string()), n.to_string()),
        _ => (None, dotted),
    }
}

/// `path:line:col kind Container.name`
pub(crate) fn def_line(
    path: &[u8],
    line: u32,
    col: u32,
    kind: u8,
    container: Option<&[u8]>,
    name: &[u8],
) -> Vec<u8> {
    let mut l = path.to_vec();
    l.extend(format!(":{line}:{col} {} ", kind_name(kind)).bytes());
    if let Some(c) = container {
        l.extend(escape_control(c));
        l.push(b'.');
    }
    l.extend(escape_control(name));
    l
}

/// `line`（1 始まり）行目を、前後の空白を除き 160 字までにして制御文字を逃がしたもの。
/// 行が無いとき（索引の後でファイルが縮んだ）は None。
pub(crate) fn source_line(text: &[u8], line: u32) -> Option<Vec<u8>> {
    let raw = text
        .split(|&b| b == b'\n')
        .nth(line.checked_sub(1)? as usize)?;
    let s = String::from_utf8_lossy(raw);
    let s = s.trim();
    let cut: String = if s.chars().count() > SOURCE_LINE_CHARS {
        s.chars().take(SOURCE_LINE_CHARS - 1).chain(['…']).collect()
    } else {
        s.to_string()
    };
    Some(escape_control(cut.as_bytes()))
}

/// 表示する行を標準出力へ。全 `total` 件のうち出さなかった分は標準エラーに件数だけ出す。
pub(crate) fn emit(
    out: &mut impl Write,
    err: &mut impl Write,
    lines: &[Vec<u8>],
    total: usize,
) -> Res {
    for l in lines {
        out.write_all(l)?;
        out.write_all(b"\n")?;
    }
    more(err, total - lines.len())?;
    Ok(total > 0)
}

pub(crate) fn more(err: &mut impl Write, rest: usize) -> io::Result<()> {
    if rest > 0 {
        writeln!(err, "… {rest} more")?;
    }
    Ok(())
}

/// FIFO などを開くと書き手が現れるまで止まるので、読む前に通常ファイルだと確かめる。
/// 大きさの上限は索引と同じにする。
fn read_regular(file: &Path) -> Res<Vec<u8>> {
    let io = |e: &dyn Display| Fail::io(format!("{}: {e}", file.display()));
    let meta = fs::metadata(file).map_err(|e| io(&e))?;
    if !meta.is_file() {
        return Err(io(&"not a regular file"));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(io(&format!("larger than {MAX_FILE_BYTES} bytes")));
    }
    fs::read(file).map_err(|e| io(&e))
}

fn outline(file: &Path, out: &mut impl Write) -> Res {
    let lang = lang_of(file).ok_or_else(|| Fail {
        code: EXIT_NONE,
        msg: format!("ferrindex: unsupported file type: {}", file.display()),
    })?;
    let src = read_regular(file)?;
    let mut defs = extract(lang, &src);
    defs.sort_by_key(|d| (d.line, d.col));
    let path = escape_control(file.as_os_str().as_bytes());
    for d in &defs {
        let l = def_line(
            &path,
            d.line,
            d.col,
            d.kind,
            d.container.as_deref().map(str::as_bytes),
            d.name.as_bytes(),
        );
        out.write_all(&l)?;
        out.write_all(b"\n")?;
    }
    Ok(!defs.is_empty())
}

/// 見出し、続けて今のファイルから取り直した本体。取り直した定義が見つからなければ何も出さない
/// （索引を作った後でファイルが書き換わった）。
fn show_one(ctx: &Ctx, f: &Found, out: &mut impl Write) -> Res<()> {
    let rel = Path::new(OsStr::from_bytes(&f.path));
    let Some(lang) = lang_of(rel) else {
        return Ok(());
    };
    let Some(src) = read_rel(&ctx.root, &f.path)? else {
        return Ok(());
    };
    let Some(def) = extract(lang, &src)
        .into_iter()
        .find(|d| (d.line, d.col, d.name.as_bytes()) == (f.line, f.col, f.name.as_slice()))
    else {
        return Ok(());
    };
    let Ok(text) = std::str::from_utf8(&src) else {
        return Ok(());
    };
    // 本体はインデントの途中から始まるので、行頭まで戻して表示する
    let start = text[..def.body.start].rfind('\n').map_or(0, |i| i + 1);
    let Some(body) = text.get(start..def.body.end) else {
        return Ok(());
    };
    let lines: Vec<&str> = body.lines().collect();
    let first = text[..start].matches('\n').count() + 1;
    let last = first + lines.len().saturating_sub(1);
    let mut head = ctx.display(&f.path);
    head.extend(format!(":{first}-{last} {} ", kind_name(f.kind)).bytes());
    if let Some(c) = &f.container {
        head.extend(escape_control(c));
        head.push(b'.');
    }
    head.extend(escape_control(f.name.as_slice()));
    out.write_all(&head)?;
    out.write_all(b"\n")?;
    for l in lines.iter().take(SHOW_MAX_LINES) {
        writeln!(out, "{l}")?;
    }
    if lines.len() > SHOW_MAX_LINES {
        writeln!(out, "… ({} more lines)", lines.len() - SHOW_MAX_LINES)?;
    }
    Ok(())
}

fn update_report(view: &View, stats: bool, out: &mut impl Write) -> io::Result<()> {
    if !stats {
        return writeln!(out, "indexed {} files", view.files().len());
    }
    let parts = view.parts();
    let total: usize = parts.iter().map(|(_, i)| i.stats().bytes).sum();
    writeln!(out, "total {total} bytes")?;
    for (label, idx) in parts {
        let s = idx.stats();
        writeln!(
            out,
            "{label}: {} files, {} names, {} bytes",
            s.files, s.names, s.bytes
        )?;
        let sections = idx.section_bytes();
        let header = s.bytes - sections.iter().map(|x| x.1).sum::<usize>();
        writeln!(out, "  header {header} bytes")?;
        for (name, bytes) in sections {
            writeln!(out, "  {name} {bytes} bytes")?;
        }
    }
    Ok(())
}

fn map_dir(ctx: &Ctx, dir: Option<&Path>) -> Res<Vec<u8>> {
    let Some(dir) = dir else {
        return Ok(ctx.prefix.clone());
    };
    let abs = fs::canonicalize(ctx.cwd.join(dir))
        .map_err(|e| Fail::io(format!("{}: {e}", dir.display())))?;
    let rel = abs
        .strip_prefix(&ctx.root)
        .map_err(|_| Fail::usage(format!("{}: outside the repository", dir.display())))?;
    if !abs.is_dir() {
        return Err(Fail::usage(format!("{}: not a directory", dir.display())));
    }
    Ok(dir_prefix(rel))
}

fn root_of(cwd: &Path) -> Res<PathBuf> {
    find_root(cwd).ok_or_else(|| Fail {
        code: EXIT_UNUSABLE,
        msg: "ferrindex: not inside a git repository".into(),
    })
}

/// 信頼は端末の利用者だけが与えられる。端末を持たない Claude Code の Bash などは自分で信頼できない。
fn trust_root(cwd: &Path, on: bool, out: &mut impl Write) -> Res {
    let root = root_of(cwd)?;
    if on && !io::stdin().is_terminal() {
        return Err(Fail {
            code: EXIT_USAGE,
            msg: "ferrindex: trust は端末から実行してください".into(),
        });
    }
    trust::set(&root, on)?;
    out.write_all(if on { b"trusted " } else { b"untrusted " })?;
    out.write_all(&escape_control(root.as_os_str().as_bytes()))?;
    out.write_all(b"\n")?;
    Ok(true)
}

/// 索引にある言語ごとに、ファイル数と言語サーバーの導入状況をタブ区切りで出す。
fn servers(root: &Path, view: &View, out: &mut impl Write) -> Res {
    let files = view.files();
    writeln!(
        out,
        "language\tfiles\tserver\tinstalled\tprerequisite\tinstall"
    )?;
    let mut any = false;
    for lang in Lang::ALL.into_iter().filter(|&l| l != Lang::Markdown) {
        let n = files.iter().filter(|(_, s)| s.lang == lang as u8).count();
        if n == 0 {
            continue;
        }
        let st = lsp::setup(lang, root);
        writeln!(
            out,
            "{}\t{n}\t{}\t{}\t{}\t{}",
            format!("{lang:?}").to_lowercase(),
            lsp::server_name(lang),
            if st.installed { "yes" } else { "no" },
            st.missing.as_deref().unwrap_or("ok"),
            lsp::install_command(lang),
        )?;
        any = true;
    }
    Ok(any)
}

/// `limit` は表示した件数で数える（バイナリと消えたファイルは数えない）。
fn map(
    ctx: &Ctx,
    dir: Option<&Path>,
    limit: usize,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Res {
    let prefix = map_dir(ctx, dir)?;
    let files: Vec<Vec<u8>> = ctx
        .view()?
        .files()
        .into_iter()
        .map(|(p, _)| p)
        .filter(|p| p.starts_with(&prefix))
        .collect();
    let (mut printed, mut seen) = (0, 0);
    for path in &files {
        if printed == limit {
            break;
        }
        seen += 1;
        let rel = Path::new(OsStr::from_bytes(path));
        let Some(bytes) = read_rel(&ctx.root, path)? else {
            continue;
        };
        if is_binary(&bytes) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let lang = lang_of(rel);
        out.write_all(&ctx.display(path))?;
        write!(out, "  ~{}  ", tokens(text.chars().count(), lang))?;
        out.write_all(&escape_control(description(&text, lang).as_bytes()))?;
        out.write_all(b"\n")?;
        printed += 1;
    }
    more(err, files.len() - seen)?;
    Ok(!files.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_rel_returns_none_only_for_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "x").unwrap();
        assert_eq!(
            read_rel(dir.path(), b"a.rs").ok().unwrap(),
            Some(b"x".to_vec())
        );
        assert_eq!(read_rel(dir.path(), b"gone.rs").ok().unwrap(), None);
        // ディレクトリは読めない（NotFound でない）ので、パスを添えた終了コード 4。
        let e = read_rel(dir.path(), b"").err().unwrap();
        assert_eq!(e.code, EXIT_IO);
        assert!(e.msg.starts_with("ferrindex: : "), "{}", e.msg);
    }

    #[test]
    fn cached_loads_once_and_passes_failures_through() {
        let mut map = HashMap::new();
        assert_eq!(*cached(&mut map, 1, || Ok(10)).ok().unwrap(), 10);
        assert_eq!(*cached(&mut map, 1, || Ok(20)).ok().unwrap(), 10);
        assert!(cached(&mut map, 2, || Err(Fail::io("x"))).is_err());
        assert!(!map.contains_key(&2));
    }

    #[test]
    fn broken_pipe_is_quiet_success_and_other_io_is_exit_4() {
        let quiet = Fail::from(io::Error::from(io::ErrorKind::BrokenPipe));
        assert_eq!((quiet.code, quiet.msg.as_str()), (0, ""));
        let other = Fail::from(io::Error::other("disk"));
        assert_eq!(other.code, EXIT_IO);
        assert_eq!(other.msg, "ferrindex: disk");
    }

    #[test]
    fn unrepairable_index_is_exit_3_and_plain_io_is_exit_4() {
        let hint = "rg -n 'x'";
        let bad = Fail::fresh(
            fresh::Error::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "index is corrupt",
            )),
            hint,
        );
        assert_eq!(bad.code, EXIT_UNUSABLE);
        assert!(bad.msg.contains("use `rg -n 'x'` instead"), "{}", bad.msg);
        assert_eq!(Fail::fresh(fresh::Error::Lock, hint).code, EXIT_UNUSABLE);
        let io = Fail::fresh(fresh::Error::Io(io::Error::other("disk")), hint);
        assert_eq!(io.code, EXIT_IO);
    }

    #[test]
    fn escape_control_covers_c0_and_del_but_not_space_or_utf8() {
        assert_eq!(escape_control(b"a\nb"), b"a\\nb");
        assert_eq!(
            escape_control(b"\x00\x1f\x7f\r\t"),
            b"\\x00\\x1f\\x7f\\x0d\\x09"
        );
        assert_eq!(escape_control(b" ~\x80\xe3"), b" ~\x80\xe3");
        assert_eq!(escape_control("日本.rs".as_bytes()), "日本.rs".as_bytes());
    }

    #[test]
    fn escape_control_covers_c1_and_bidi_controls() {
        let esc = |s: &str| String::from_utf8(escape_control(s.as_bytes())).unwrap();
        assert_eq!(esc("a\u{80}\u{9b}\u{9f}b"), "a\\u{80}\\u{9b}\\u{9f}b");
        assert_eq!(
            esc("\u{202a}\u{202e}\u{2066}\u{2069}"),
            "\\u{202a}\\u{202e}\\u{2066}\\u{2069}"
        );
        // 範囲のすぐ外の文字はそのまま。
        assert_eq!(
            esc("\u{7e}\u{a0}\u{2029}\u{202f}\u{2065}\u{206a}"),
            "\u{7e}\u{a0}\u{2029}\u{202f}\u{2065}\u{206a}"
        );
        // 壊れた UTF-8 の後ろでも見つける。
        assert_eq!(escape_control(b"\xff\xc2\x9b"), b"\xff\\u{9b}");
    }

    #[test]
    fn dir_prefix_adds_a_slash_only_below_the_root() {
        assert_eq!(dir_prefix(Path::new("")), b"");
        assert_eq!(dir_prefix(Path::new("a/b")), b"a/b/");
    }

    #[test]
    fn relative_paths_climb_out_of_the_current_directory() {
        let rel = |dir: &str, path: &str| {
            String::from_utf8(relative_to(dir.as_bytes(), path.as_bytes())).unwrap()
        };
        assert_eq!(rel("", "a/b.rs"), "a/b.rs");
        assert_eq!(rel("sub/", "sub/a.rs"), "a.rs");
        assert_eq!(rel("sub/", "top.rs"), "../top.rs");
        assert_eq!(rel("sub/", "other/x.rs"), "../other/x.rs");
        assert_eq!(rel("a/b/", "a/c/x.rs"), "../c/x.rs");
        assert_eq!(rel("a/b/", "a/x.rs"), "../x.rs");
        assert_eq!(rel("a/b/", "a/b/c/x.rs"), "c/x.rs");
        assert_eq!(rel("src/", "src2/x.rs"), "../src2/x.rs");
        assert_eq!(rel("src/", "src"), "../src");
    }

    #[test]
    fn grep_hint_quotes_single_quotes() {
        assert_eq!(grep_hint("it's"), "rg -n 'it'\\''s'");
    }
}
