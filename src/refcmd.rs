//! `refs` `callers` `impls` `supers`。
//! 索引は呼び出しと継承の参照だけを持つので、`callers` と `impls` は索引から、
//! `supers` は定義のファイルから、`refs`（素の参照を含む）は実行時にファイルを読んで答える。

use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    fs,
    io::Write,
    os::unix::ffi::OsStrExt,
    path::Path,
};

use crate::extract::extract;
use crate::fresh::{Found, View, is_binary};
use crate::lang::{Lang, lang_of};
use crate::query::{Ctx, Res, corrupt, emit, escape_control, resolve, source_line};
use crate::refs::{REF_CALL, REF_INHERIT, Ref, extract_refs};

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

pub(crate) fn run(
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
    fn kind_words() {
        assert_eq!(kind_word(REF_CALL), "call");
        assert_eq!(kind_word(REF_INHERIT), "inherit");
        assert_eq!(kind_word(0), "ref");
    }
}
