//! `refs` `callers` `impls` `supers`。
//! 索引は呼び出しと継承の参照だけを持つので、`callers` と `impls` は索引から、
//! `supers` は定義のファイルから、`refs`（素の参照を含む）は実行時にファイルを読んで答える。
//! `--precise` は定義の位置を言語サーバーに問い、調べられなければ名前一致に戻る。

use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    fs,
    io::Write,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::extract::{Def, extract};
use crate::fresh::{Found, View, is_binary};
use crate::lang::{Lang, lang_of};
use crate::lsp::{self, Location, Method};
use crate::query::{Ctx, Res, corrupt, emit, escape_control, resolve, resolve_exact, source_line};
use crate::refs::{REF_CALL, REF_INHERIT, REF_PLAIN, Ref, extract_refs};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefKind {
    Refs,
    Callers,
    Impls,
    Supers,
}

/// 出力する 1 件。`path` は根からの相対。
struct Hit {
    path: Vec<u8>,
    line: u32,
    col: u32,
    kind: u8,
    container: Option<Vec<u8>>,
}

impl Hit {
    fn new(path: &[u8], r: &Ref) -> Hit {
        Hit {
            path: path.to_vec(),
            line: r.line,
            col: r.col,
            kind: r.kind,
            container: r.container.as_ref().map(|c| c.clone().into_bytes()),
        }
    }
}

/// 3つ組は（何を調べるか、名前、言語サーバーに問うか）。
pub(crate) fn run(
    ctx: &Ctx,
    view: &View,
    (kind, term, precise): (RefKind, &str, bool),
    limit: usize,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Res {
    if !precise {
        return name_match(ctx, view, kind, term, limit, out, err);
    }
    let why = match precise_lines(ctx, view, kind, term)? {
        Ok(mut lines) if !lines.is_empty() => {
            let total = lines.len();
            lines.truncate(limit);
            return emit(out, err, &lines, total);
        }
        // 読み込み途中のサーバーは0件と答えうるので、0件は確定とみなさない。
        Ok(_) => None,
        Err(why) => Some(why),
    };
    let (mut found_out, mut found_err) = (Vec::new(), Vec::new());
    let found = name_match(ctx, view, kind, term, limit, &mut found_out, &mut found_err)?;
    // 0件どうしなら答えは食い違っていないので、戻ったとは告げない。
    let why = match why {
        Some(why) => Some(why),
        None => found.then(|| "言語サーバーの答えが0件でした".to_string()),
    };
    if let Some(why) = why {
        writeln!(err, "ferrindex: {why}。名前一致の結果です（未確定）")?;
    }
    err.write_all(&found_err)?;
    out.write_all(&found_out)?;
    Ok(found)
}

fn name_match(
    ctx: &Ctx,
    view: &View,
    kind: RefKind,
    term: &str,
    limit: usize,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Res {
    let name = last_segment(term);
    match kind {
        RefKind::Callers => indexed(ctx, view, &name, REF_CALL, limit, out, err),
        RefKind::Refs => refs(ctx, view, &name, limit, out, err),
        RefKind::Impls | RefKind::Supers => {
            let defs = resolve(view, term).map_err(|c| corrupt(c, ctx))?;
            if only_go(&defs) {
                writeln!(err, "{GO_NEEDS_PRECISE}")?;
                Ok(false)
            } else if kind == RefKind::Supers {
                supers(ctx, defs, limit, out, err)
            } else {
                indexed(ctx, view, &name, REF_INHERIT, limit, out, err)
            }
        }
    }
}

const GO_NEEDS_PRECISE: &str =
    "ferrindex: Go のインターフェースの実装は名前一致では分かりません。--precise を使ってください";

/// Go は型がインターフェースを暗黙に満たすので、名前では継承の関係が分からない。
fn only_go(defs: &[Found]) -> bool {
    !defs.is_empty()
        && defs
            .iter()
            .all(|d| lang_of(Path::new(OsStr::from_bytes(&d.path))) == Some(Lang::Go))
}

/// `Foo.bar` と `Foo::bar` は `bar` を探す。`resolve` と同じ分け方にする。
fn last_segment(term: &str) -> String {
    let dotted = term.replace("::", ".");
    match dotted.rsplit_once('.') {
        Some((c, n)) if !c.is_empty() && !n.is_empty() => n.to_string(),
        _ => dotted,
    }
}

/// 同名の定義ごとにサーバーを起動するので、問い合わせる定義の数を抑える。
const PRECISE_MAX_DEFS: usize = 5;
/// 定義が複数でも、すべての問い合わせを合わせた待ちの上限。
const PRECISE_WAIT: Duration = Duration::from_secs(30);

/// 言語サーバーの答えを出力の行にする。調べられなければ理由を返す（呼び手は名前一致に戻る）。
fn precise_lines(
    ctx: &Ctx,
    view: &View,
    kind: RefKind,
    term: &str,
) -> Res<Result<Vec<Vec<u8>>, String>> {
    let defs = resolve_exact(view, term).map_err(|c| corrupt(c, ctx))?;
    if defs.is_empty() {
        return Ok(Err(format!("{term}の定義が見つかりません")));
    }
    if defs.len() > PRECISE_MAX_DEFS {
        return Ok(Err(format!(
            "{term}の定義が{}件あり、問い合わせる上限の{PRECISE_MAX_DEFS}件を超えます",
            defs.len()
        )));
    }
    let method = match kind {
        RefKind::Refs => Method::References,
        RefKind::Callers => Method::IncomingCalls,
        RefKind::Impls => Method::Implementation,
        RefKind::Supers => Method::Supertypes,
    };
    let deadline = Instant::now() + PRECISE_WAIT;
    let mut found = Vec::new();
    for d in &defs {
        let file = Path::new(OsStr::from_bytes(&d.path));
        // 索引の定義はどれも言語の決まるファイルにある。Markdown はサーバーが無いと答える。
        let lang = lang_of(file).unwrap_or(Lang::Markdown);
        match lsp::query(&ctx.root, lang, file, d.line, d.col, method, deadline) {
            Ok(locs) => found.extend(locs),
            Err(e) => return Ok(Err(format!("{}: {e}", lsp::server_name(lang)))),
        }
    }
    found.sort();
    found.dedup();
    let root = canonical_root(ctx);
    // 修飾の無い名前の呼び出しは、どれも索引にあるどれかの定義を指すはず。修飾した名前は
    // 同名の別の定義を指す呼び出しと見分けられないので確かめない。
    let checks_calls =
        matches!(kind, RefKind::Refs | RefKind::Callers) && last_segment(term) == term;
    if checks_calls
        && !found.is_empty()
        && misses_a_call(view, term, &root, &found).map_err(|c| corrupt(c, ctx))?
    {
        return Ok(Err(
            "言語サーバーが取り込みや別名経由の呼び出しを返していません".to_string(),
        ));
    }
    let plain = match kind {
        RefKind::Refs => REF_PLAIN,
        RefKind::Callers => REF_CALL,
        RefKind::Impls | RefKind::Supers => REF_INHERIT,
    };
    Ok(Ok(located(ctx, &root, &found, plain)))
}

/// サーバーは正規化したパスで答える。
fn canonical_root(ctx: &Ctx) -> PathBuf {
    fs::canonicalize(&ctx.root).unwrap_or_else(|_| ctx.root.clone())
}

/// 名前一致の呼び出しに、全定義の答えを合わせたものに無い位置があるか。あれば、サーバーは
/// `require` の分割代入、別名、型を推論できない受け手を経た呼び出しを追えていない。
fn misses_a_call(
    view: &View,
    name: &str,
    root: &Path,
    found: &[Location],
) -> Result<bool, crate::codec::Corrupt> {
    let answered: HashSet<(&[u8], u32, u32)> = found
        .iter()
        .filter_map(|l| {
            let rel = l.path.strip_prefix(root).ok()?;
            Some((rel.as_os_str().as_bytes(), l.line, l.col))
        })
        .collect();
    Ok(view
        .lookup_refs(name.as_bytes())?
        .iter()
        .any(|f| f.kind == REF_CALL && !answered.contains(&(&f.path[..], f.line, f.col))))
}

/// 位置ごとにファイルを抽出し直し、その位置の参照の種類と、囲む最も内側の定義を添える。
/// 作業ツリーの外の位置（依存ライブラリなど）は出さない。
fn located(ctx: &Ctx, root: &Path, found: &[Location], plain: u8) -> Vec<Vec<u8>> {
    let mut parsed: HashMap<&Path, Option<Parsed>> = HashMap::new();
    let mut lines = Vec::new();
    for loc in found {
        let Ok(rel) = loc.path.strip_prefix(root) else {
            continue;
        };
        let Some((src, defs, refs)) = parsed.entry(rel).or_insert_with(|| parse(ctx, rel)) else {
            continue;
        };
        let at = (loc.line, loc.col);
        let kind = refs
            .iter()
            .find(|r| (r.line, r.col) == at)
            .map_or(plain, |r| r.kind);
        let offset = byte_offset(src, loc.line, loc.col);
        // 位置が定義の名前そのもの（impls や supers の答え）なら、その定義は囲みに数えない。
        let container = defs
            .iter()
            .filter(|d| d.body.contains(&offset) && (d.line, d.col) != at)
            .min_by_key(|d| d.body.len())
            .map(|d| d.name.clone().into_bytes());
        let hit = Hit {
            path: rel.as_os_str().as_bytes().to_vec(),
            line: loc.line,
            col: loc.col,
            kind,
            container,
        };
        lines.push(render(ctx, &hit, Some(src)));
    }
    lines
}

/// ファイルの内容と、抽出し直した定義と参照。
type Parsed = (Vec<u8>, Vec<Def>, Vec<Ref>);

/// 言語の決まらないファイル（サーバーが返した設定ファイルなど）は、定義と参照を空にして行だけ出す。
fn parse(ctx: &Ctx, rel: &Path) -> Option<Parsed> {
    let src = read_text(ctx, rel.as_os_str().as_bytes())?;
    let Some(lang) = lang_of(rel) else {
        return Some((src, Vec::new(), Vec::new()));
    };
    let defs = extract(lang, &src);
    let refs = extract_refs(lang, &src, &defs);
    Some((src, defs, refs))
}

/// 1 始まりの行と、1 始まりで UTF-16 単位の列を、バイト位置にする。
fn byte_offset(src: &[u8], line: u32, col: u32) -> usize {
    let start: usize = src
        .split_inclusive(|&b| b == b'\n')
        .take(line.saturating_sub(1) as usize)
        .map(<[u8]>::len)
        .sum();
    let rest = String::from_utf8_lossy(&src[start..]);
    let mut units = 1;
    for (i, c) in rest.char_indices() {
        if units >= col as usize || c == '\n' {
            return start + i;
        }
        units += c.len_utf16();
    }
    src.len()
}

fn kind_word(kind: u8) -> &'static str {
    match kind {
        REF_CALL => "call",
        REF_INHERIT => "inherit",
        _ => "ref",
    }
}

/// `path:line:col kind in container\tソース行`。コンテナと行が無ければその部分を省く。
fn render(ctx: &Ctx, h: &Hit, src: Option<&[u8]>) -> Vec<u8> {
    let mut l = ctx.display(&h.path);
    l.extend(format!(":{}:{} {}", h.line, h.col, kind_word(h.kind)).bytes());
    if let Some(c) = &h.container {
        l.extend(b" in ");
        l.extend(escape_control(c));
    }
    if let Some(text) = src.and_then(|t| source_line(t, h.line)) {
        l.push(b'\t');
        l.extend(text);
    }
    l
}

fn read_text(ctx: &Ctx, path: &[u8]) -> Option<Vec<u8>> {
    fs::read(ctx.root.join(OsStr::from_bytes(path))).ok()
}

fn indexed(
    ctx: &Ctx,
    view: &View,
    name: &str,
    kind: u8,
    limit: usize,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Res {
    let mut found = view
        .lookup_refs(name.as_bytes())
        .map_err(|c| corrupt(c, ctx))?;
    found.retain(|f| f.kind == kind);
    let mut texts: HashMap<&[u8], Option<Vec<u8>>> = HashMap::new();
    let lines: Vec<Vec<u8>> = found
        .iter()
        .take(limit)
        .map(|f| {
            let src = texts
                .entry(&f.path)
                .or_insert_with(|| read_text(ctx, &f.path));
            let hit = Hit {
                path: f.path.clone(),
                line: f.line,
                col: f.col,
                kind: f.kind,
                container: f.container.clone(),
            };
            render(ctx, &hit, src.as_deref())
        })
        .collect();
    emit(out, err, &lines, found.len())
}

/// 定義のファイルを今の内容で取り直し、定義の名前を囲みに持つ継承参照を出す。
fn supers(
    ctx: &Ctx,
    defs: Vec<Found>,
    limit: usize,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Res {
    let mut seen = HashSet::new();
    let mut lines = Vec::new();
    for d in defs {
        if !seen.insert((d.path.clone(), d.name.clone())) {
            continue;
        }
        let Some(lang) = lang_of(Path::new(OsStr::from_bytes(&d.path))) else {
            continue;
        };
        let Some(src) = read_text(ctx, &d.path) else {
            continue;
        };
        let file_defs = extract(lang, &src);
        for r in extract_refs(lang, &src, &file_defs) {
            if r.kind == REF_INHERIT && r.container.as_deref().map(str::as_bytes) == Some(&d.name) {
                lines.push(render(ctx, &Hit::new(&d.path, &r), Some(&src)));
            }
        }
    }
    let total = lines.len();
    lines.truncate(limit);
    emit(out, err, &lines, total)
}

/// 名前のバイト列を含むテキストファイル。大きすぎるファイルは `view.files()` が既に除いている。
fn candidate(ctx: &Ctx, path: &[u8], needle: &[u8]) -> Option<(Lang, Vec<u8>)> {
    let rel = Path::new(OsStr::from_bytes(path));
    let lang = lang_of(rel)?;
    let src = read_text(ctx, path)?;
    (!is_binary(&src) && contains(&src, needle)).then_some((lang, src))
}

/// 1 スレッドあたりに一度に渡すファイル数。`limit` に届いた後に無駄に読む分の上限になる。
const FILES_PER_THREAD: usize = 16;

/// 経路順の塊ごとに、候補の読み込みと解析をスレッドで分ける。`limit` 行に届いた塊の終わりで止め、
/// 残りは読まない。
fn refs(
    ctx: &Ctx,
    view: &View,
    name: &str,
    limit: usize,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Res {
    let files: Vec<Vec<u8>> = view.files().into_iter().map(|(p, _)| p).collect();
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut lines = Vec::new();
    let mut next = 0;
    while next < files.len() && lines.len() < limit {
        let end = files.len().min(next + threads * FILES_PER_THREAD);
        lines.extend(scan_chunk(ctx, &files[next..end], name, threads));
        next = end;
    }
    // 読んだ塊の中の超過行は数えない。残りのファイルがあるとき、件数は下限にしかならない。
    let unread = next < files.len();
    let total = if unread {
        limit.min(lines.len())
    } else {
        lines.len()
    };
    lines.truncate(limit);
    let found = emit(out, err, &lines, total)?;
    if unread {
        writeln!(err, "… more files may match")?;
    }
    Ok(found)
}

/// `files` の参照行を、経路順のまま返す。
fn scan_chunk(ctx: &Ctx, files: &[Vec<u8>], name: &str, threads: usize) -> Vec<Vec<u8>> {
    let per = files.len().div_ceil(threads).max(1);
    std::thread::scope(|s| {
        let handles: Vec<_> = files
            .chunks(per)
            .map(|part| {
                s.spawn(move || {
                    part.iter()
                        .flat_map(|p| scan_file(ctx, p, name))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("scan thread panicked"))
            .collect()
    })
}

fn scan_file(ctx: &Ctx, path: &[u8], name: &str) -> Vec<Vec<u8>> {
    let Some((lang, src)) = candidate(ctx, path, name.as_bytes()) else {
        return Vec::new();
    };
    let defs = extract(lang, &src);
    extract_refs(lang, &src, &defs)
        .iter()
        .filter(|r| r.name == name)
        .map(|r| render(ctx, &Hit::new(path, r), Some(&src)))
        .collect()
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    let Some((&first, rest)) = needle.split_first() else {
        return true;
    };
    let mut from = 0;
    while let Some(p) = hay[from..].iter().position(|&b| b == first) {
        from += p + 1;
        if hay[from..].starts_with(rest) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_finds_needles_at_every_position() {
        assert!(contains(b"abc", b"abc"));
        assert!(contains(b"xabc", b"abc"));
        assert!(contains(b"aabc", b"abc"));
        assert!(contains(b"abcx", b"c"));
        assert!(contains(b"abc", b""));
        assert!(!contains(b"abx", b"abc"));
        assert!(!contains(b"ab", b"abc"));
        assert!(!contains(b"", b"a"));
    }

    #[test]
    fn last_segment_splits_on_dot_and_double_colon() {
        assert_eq!(last_segment("bar"), "bar");
        assert_eq!(last_segment("Foo.bar"), "bar");
        assert_eq!(last_segment("a::b::c"), "c");
        assert_eq!(last_segment("Foo."), "Foo.");
        assert_eq!(last_segment(".bar"), ".bar");
    }

    #[test]
    fn byte_offset_counts_utf16_columns() {
        let src = "ab\n\u{3042}\u{1F600}x\n".as_bytes();
        assert_eq!(byte_offset(src, 1, 1), 0);
        assert_eq!(byte_offset(src, 1, 2), 1);
        assert_eq!(byte_offset(src, 2, 1), 3);
        assert_eq!(byte_offset(src, 2, 2), 6);
        // 絵文字は UTF-16 で 2 単位、UTF-8 で 4 バイト。
        assert_eq!(byte_offset(src, 2, 4), 10);
        // 行末より右の列は行末に寄せる。
        assert_eq!(byte_offset(src, 1, 9), 2);
        assert_eq!(byte_offset(src, 9, 1), src.len());
    }

    #[test]
    fn kind_words() {
        assert_eq!(kind_word(REF_CALL), "call");
        assert_eq!(kind_word(REF_INHERIT), "inherit");
        assert_eq!(kind_word(0), "ref");
    }
}
