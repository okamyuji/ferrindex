use std::ops::Range;

use tree_sitter::{Node, Parser};
use tree_sitter_tags::TagsContext;

use crate::lang::{Lang, kind_id, tags_config};
use crate::markdown;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Def {
    pub name: String,
    /// 1 始まり
    pub line: u32,
    /// 1 始まり、UTF-16 単位
    pub col: u32,
    pub kind: u8,
    pub container: Option<String>,
    /// `show` が表示する範囲（src へのバイト範囲）
    pub body: Range<usize>,
}

/// UTF-8 でない入力は空を返す。
pub fn extract(lang: Lang, src: &[u8]) -> Vec<Def> {
    let Ok(text) = std::str::from_utf8(src) else {
        return Vec::new();
    };
    if lang == Lang::Markdown {
        return markdown::headings(text);
    }
    tag_defs(lang, src).unwrap_or_default()
}

fn tag_defs(lang: Lang, src: &[u8]) -> Option<Vec<Def>> {
    let cfg = tags_config(lang)?;
    let mut parser = Parser::new();
    parser.set_language(&cfg.language).ok()?;
    let tree = parser.parse(src, None)?;
    let root = tree.root_node();
    let mut ctx = TagsContext::new();
    let (tags, _) = ctx.generate_tags(cfg, src, None).ok()?;

    let mut defs = Vec::new();
    let mut names: Vec<Range<usize>> = Vec::new();
    for tag in tags {
        let Ok(tag) = tag else { continue };
        if !tag.is_definition {
            continue;
        }
        let kind = cfg.syntax_type_name(tag.syntax_type_id);
        let (Some(node), Some(name_node)) = (
            root.descendant_for_byte_range(tag.range.start, tag.range.end),
            root.descendant_for_byte_range(tag.name_range.start, tag.name_range.end),
        ) else {
            continue;
        };
        let body = adjust(lang, node, name_node);
        defs.push(Def {
            name: String::from_utf8_lossy(&src[tag.name_range.clone()]).into_owned(),
            line: tag.span.start.row as u32 + 1,
            col: tag.utf16_column_range.start as u32 + 1,
            kind: kind_id(kind),
            container: None,
            body: body.start_byte()..body.end_byte(),
        });
        names.push(tag.name_range);
    }
    let containers = containers(&defs, &names);
    for (d, c) in defs.iter_mut().zip(containers) {
        d.container = c;
    }
    Some(defs)
}

/// 各定義の名前の範囲を含む最も内側の class / interface / module / type（自分自身は除く）。
/// 定義を名前の位置順に走査し、holder の範囲を入れ子のスタックで持つので O(n log n)。
fn containers(defs: &[Def], names: &[Range<usize>]) -> Vec<Option<String>> {
    let holder_kinds = ["class", "interface", "module", "type"].map(kind_id);
    let mut holders: Vec<usize> = (0..defs.len())
        .filter(|&i| holder_kinds.contains(&defs[i].kind))
        .collect();
    holders.sort_by_key(|&i| (defs[i].body.start, std::cmp::Reverse(defs[i].body.end)));
    let mut order: Vec<usize> = (0..defs.len()).collect();
    order.sort_by_key(|&i| names[i].start);

    let mut out = vec![None; defs.len()];
    let mut next = 0;
    let mut stack: Vec<usize> = Vec::new();
    for i in order {
        while next < holders.len() && defs[holders[next]].body.start <= names[i].start {
            stack.push(holders[next]);
            next += 1;
        }
        while stack
            .last()
            .is_some_and(|&h| defs[h].body.end < names[i].end)
        {
            stack.pop();
        }
        // 自分自身の下にあるエントリは名前を含むとは限らないので範囲を確かめる
        let inner = stack
            .iter()
            .rev()
            .find(|&&h| h != i && names[i].end <= defs[h].body.end);
        out[i] = inner.map(|&h| defs[h].name.clone());
    }
    out
}

/// 範囲が同じ入れ子のノードの一番外側（Dart は method_signature と function_signature が同じ範囲になる）。
fn outermost(mut n: Node<'_>) -> Node<'_> {
    while let Some(p) = n.parent().filter(|p| p.byte_range() == n.byte_range()) {
        n = p;
    }
    n
}

fn ancestor<'a>(mut n: Node<'a>, pred: impl Fn(Node<'a>) -> bool) -> Option<Node<'a>> {
    while let Some(p) = n.parent() {
        if pred(p) {
            return Some(p);
        }
        n = p;
    }
    None
}

/// tags.scm が定義の一部しか(または広すぎる範囲を)捕捉する言語で、`show` に出す範囲を直す。
fn adjust<'a>(lang: Lang, node: Node<'a>, name: Node<'a>) -> Node<'a> {
    let fixed = match lang {
        Lang::C | Lang::Cpp if node.kind() == "function_declarator" => {
            widen_c(node, lang == Lang::Cpp)
        }
        Lang::Dart => outermost(node)
            .parent()
            .filter(|p| p.kind() == "method_declaration"),
        Lang::Python => node.parent().filter(|p| p.kind() == "decorated_definition"),
        // メソッドとプロパティはクラス全体が捕捉されるので、名前から最も近い宣言まで狭める。
        // 入れ子のクラスでは外側の宣言が node より前から始まるので弾かれ、node のままになる
        Lang::Swift => ancestor(name, |p| {
            matches!(
                p.kind(),
                "function_declaration"
                    | "init_declaration"
                    | "deinit_declaration"
                    | "subscript_declaration"
                    | "property_declaration"
                    | "protocol_function_declaration"
            )
        })
        .filter(|n| n.start_byte() >= node.start_byte()),
        Lang::Lua if node.kind() == "table_constructor" => ancestor(name, |p| p.kind() == "field"),
        _ => None,
    };
    fixed.unwrap_or(node)
}

/// 関数本体か宣言の全体まで広げる。関数本体 `{}` やクラス本体の外へは出ない。
fn widen_c<'a>(decl: Node<'a>, cpp: bool) -> Option<Node<'a>> {
    let full = ancestor(decl, |p| {
        matches!(
            p.kind(),
            "function_definition"
                | "declaration"
                | "field_declaration"
                | "compound_statement"
                | "field_declaration_list"
                | "translation_unit"
        )
    })
    .filter(|p| {
        matches!(
            p.kind(),
            "function_definition" | "declaration" | "field_declaration"
        )
    })?;
    Some(match full.parent() {
        Some(t) if cpp && t.kind() == "template_declaration" => t,
        _ => full,
    })
}
