use std::{
    ffi::{OsStr, OsString},
    fmt::Display,
    fs,
    io::{self, Write},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

use crate::codec::Corrupt;
use crate::describe::{description, tokens};
use crate::extract::extract;
use crate::fresh::{self, Found, View, is_binary};
use crate::index::contains_ignore_ascii_case;
use crate::lang::{kind_name, lang_of};
use crate::store::find_root;

const EXIT_NONE: u8 = 1;
const EXIT_USAGE: u8 = 2;
const EXIT_UNUSABLE: u8 = 3;
const EXIT_IO: u8 = 4;

const DEFAULT_LIMIT: usize = 20;
const SHOW_LIMIT: usize = 3;
const MAP_LIMIT: usize = 200;
const SHOW_MAX_LINES: usize = 200;

const USAGE: &str = "usage: ferrindex update [--rebuild] [--stats] | find <name> [--limit N] | files <substr> [--limit N] | outline <file> | show <name> [--limit N] | map [<dir>] [--limit N]";

struct Fail {
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

type Res<T = bool> = Result<T, Fail>;

enum Cmd {
    Update { rebuild: bool, stats: bool },
    Find(String),
    Files(String),
    Outline(PathBuf),
    Show(String),
    Map(Option<PathBuf>),
}

struct Cli {
    cmd: Cmd,
    limit: Option<usize>,
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
        (Some("map"), []) => Cmd::Map(None),
        (Some("map"), [d]) => Cmd::Map(Some(PathBuf::from(d))),
        (Some("update" | "find" | "files" | "show" | "outline" | "map"), _) => {
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
    if flags.limit.is_some() && (update || matches!(cmd, Cmd::Outline(_))) {
        return Err(Fail::usage("--limit does not apply to this subcommand"));
    }
    Ok(Cli {
        cmd,
        limit: flags.limit,
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
struct Ctx {
    root: PathBuf,
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

    fn display<'a>(&self, path: &'a [u8]) -> &'a [u8] {
        path.strip_prefix(self.prefix.as_slice()).unwrap_or(path)
    }
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
            let lines: Vec<Vec<u8>> = hits
                .iter()
                .map(|f| {
                    def_line(
                        ctx.display(&f.path),
                        f.line,
                        f.col,
                        f.kind,
                        f.container.as_deref(),
                        &f.name,
                    )
                })
                .collect();
            emit(out, err, &lines, limit(DEFAULT_LIMIT))
        }
        Cmd::Files(t) => {
            let ctx = Ctx::new(cwd, "rg --files".into())?;
            let lines: Vec<Vec<u8>> = ctx
                .view()?
                .files()
                .into_iter()
                .filter(|(p, _)| contains_ignore_ascii_case(p, t.as_bytes()))
                .map(|(p, _)| ctx.display(&p).to_vec())
                .collect();
            emit(out, err, &lines, limit(DEFAULT_LIMIT))
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
        Cmd::Map(dir) => map(
            &Ctx::new(cwd, "rg --files".into())?,
            dir.as_deref(),
            limit(MAP_LIMIT),
            out,
            err,
        ),
    }
}

fn corrupt(_: Corrupt, ctx: &Ctx) -> Fail {
    Fail::unusable("index is corrupt", &ctx.hint)
}

/// 完全一致、`Container.name`、名前の部分一致（ASCII の大小無視）の順に試す。
/// Markdown の見出しは `.` を含み得るので、全体の完全一致を先に引く。
fn resolve(view: &View, term: &str) -> Result<Vec<Found>, Corrupt> {
    let hits = view.lookup(term.as_bytes())?;
    if !hits.is_empty() {
        return Ok(hits);
    }
    let (container, name) = match term.rsplit_once('.') {
        Some((c, n)) if !c.is_empty() && !n.is_empty() => (Some(c), n),
        _ => (None, term),
    };
    let keep = |f: &Found| container.is_none_or(|c| f.container.as_deref() == Some(c.as_bytes()));
    let mut hits = view.lookup(name.as_bytes())?;
    hits.retain(keep);
    if hits.is_empty() {
        hits = view.lookup_substring(name.as_bytes())?;
        hits.retain(keep);
    }
    Ok(hits)
}

/// `path:line:col kind Container.name`
fn def_line(
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
        l.extend_from_slice(c);
        l.push(b'.');
    }
    l.extend_from_slice(name);
    l
}

/// 並び替え済みの行を `limit` 件まで標準出力へ。超えた分は標準エラーに件数だけ出す。
fn emit(out: &mut impl Write, err: &mut impl Write, lines: &[Vec<u8>], limit: usize) -> Res {
    for l in lines.iter().take(limit) {
        out.write_all(l)?;
        out.write_all(b"\n")?;
    }
    more(err, lines.len().saturating_sub(limit))?;
    Ok(!lines.is_empty())
}

fn more(err: &mut impl Write, rest: usize) -> io::Result<()> {
    if rest > 0 {
        writeln!(err, "… {rest} more")?;
    }
    Ok(())
}

fn outline(file: &Path, out: &mut impl Write) -> Res {
    let lang = lang_of(file).ok_or_else(|| Fail {
        code: EXIT_NONE,
        msg: format!("ferrindex: unsupported file type: {}", file.display()),
    })?;
    let src = fs::read(file).map_err(|e| Fail::io(format!("{}: {e}", file.display())))?;
    let mut defs = extract(lang, &src);
    defs.sort_by_key(|d| (d.line, d.col));
    let path = file.as_os_str().as_bytes();
    for d in &defs {
        let l = def_line(
            path,
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
    let Ok(src) = fs::read(ctx.root.join(rel)) else {
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
    let mut head = ctx.display(&f.path).to_vec();
    head.extend(format!(":{first}-{last} {} ", kind_name(f.kind)).bytes());
    if let Some(c) = &f.container {
        head.extend_from_slice(c);
        head.push(b'.');
    }
    head.extend_from_slice(&f.name);
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
        .map_err(|e| Fail::usage(format!("{}: {e}", dir.display())))?;
    let rel = abs
        .strip_prefix(&ctx.root)
        .map_err(|_| Fail::usage(format!("{}: outside the repository", dir.display())))?;
    if !abs.is_dir() {
        return Err(Fail::usage(format!("{}: not a directory", dir.display())));
    }
    Ok(dir_prefix(rel))
}

/// 読んで数えるのは表示する `limit` 件だけ。
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
    for path in files.iter().take(limit) {
        let rel = Path::new(OsStr::from_bytes(path));
        let Ok(bytes) = fs::read(ctx.root.join(rel)) else {
            continue;
        };
        if is_binary(&bytes) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let lang = lang_of(rel);
        out.write_all(ctx.display(path))?;
        writeln!(
            out,
            "  ~{}  {}",
            tokens(text.chars().count(), lang),
            description(&text, lang)
        )?;
    }
    more(err, files.len().saturating_sub(limit))?;
    Ok(!files.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn dir_prefix_adds_a_slash_only_below_the_root() {
        assert_eq!(dir_prefix(Path::new("")), b"");
        assert_eq!(dir_prefix(Path::new("a/b")), b"a/b/");
    }

    #[test]
    fn grep_hint_quotes_single_quotes() {
        assert_eq!(grep_hint("it's"), "rg -n 'it'\\''s'");
    }
}
