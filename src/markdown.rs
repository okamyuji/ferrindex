use crate::extract::Def;
use crate::lang::kind_id;

struct Heading<'a> {
    level: usize,
    start: usize,
    name: &'a str,
    line: u32,
    col: u32,
}

/// CommonMark の ATX 見出し。フェンスコードと先頭の frontmatter は読み飛ばす。
pub fn headings(text: &str) -> Vec<Def> {
    let found = scan(text);
    let section = kind_id("section");
    found
        .iter()
        .enumerate()
        .filter(|(_, h)| !h.name.is_empty())
        .map(|(i, h)| {
            let end = found[i + 1..]
                .iter()
                .find(|n| n.level <= h.level)
                .map_or(text.len(), |n| n.start);
            Def {
                name: h.name.to_string(),
                line: h.line,
                col: h.col,
                kind: section,
                container: None,
                body: h.start..end,
            }
        })
        .collect()
}

fn scan(text: &str) -> Vec<Heading<'_>> {
    let skip = frontmatter_end(text);
    let mut out = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    let mut pos = 0;
    for (i, raw) in text.split_inclusive('\n').enumerate() {
        let start = pos;
        pos += raw.len();
        if start < skip {
            continue;
        }
        let line = raw.trim_end_matches(['\n', '\r']);
        let indent = line.len() - line.trim_start_matches(' ').len();
        if indent > 3 {
            continue;
        }
        let rest = &line[indent..];
        if let Some((ch, n)) = fence {
            let run = rest.chars().take_while(|&c| c == ch).count();
            if run >= n && rest[run..].trim().is_empty() {
                fence = None;
            }
            continue;
        }
        if let Some(open) = fence_open(rest) {
            fence = Some(open);
            continue;
        }
        if let Some((level, name)) = atx(rest) {
            let off = name.as_ptr() as usize - line.as_ptr() as usize;
            out.push(Heading {
                level,
                start,
                name,
                line: i as u32 + 1,
                col: line[..off].encode_utf16().count() as u32 + 1,
            });
        }
    }
    out
}

fn fence_open(rest: &str) -> Option<(char, usize)> {
    let ch = rest.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let n = rest.chars().take_while(|&c| c == ch).count();
    // バッククォートのフェンスは info string に ` を含められない
    (n >= 3 && !(ch == '`' && rest[n..].contains('`'))).then_some((ch, n))
}

fn atx(rest: &str) -> Option<(usize, &str)> {
    let level = rest.chars().take_while(|&c| c == '#').count();
    let after = &rest[level..];
    if !(1..=6).contains(&level) || !(after.is_empty() || after.starts_with([' ', '\t'])) {
        return None;
    }
    let content = after.trim();
    let closed = content.trim_end_matches('#');
    let name = if closed.is_empty() || closed.ends_with([' ', '\t']) {
        closed.trim_end()
    } else {
        content
    };
    Some((level, name))
}

/// 先頭が `---` で、後ろに `---` か `...` の行があるときだけ frontmatter とみなす。
fn frontmatter_end(text: &str) -> usize {
    let mut lines = text.split_inclusive('\n');
    let Some(first) = lines.next() else { return 0 };
    if first.trim_end() != "---" {
        return 0;
    }
    let mut pos = first.len();
    for l in lines {
        pos += l.len();
        if matches!(l.trim_end(), "---" | "...") {
            return pos;
        }
    }
    0
}
