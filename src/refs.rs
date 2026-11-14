use std::cmp::Reverse;
use std::collections::HashSet;

use tree_sitter::{Node, Parser, TreeCursor};

use crate::extract::Def;
use crate::lang::{Lang, tags_config};

pub const REF_PLAIN: u8 = 0;
pub const REF_CALL: u8 = 1;
pub const REF_INHERIT: u8 = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ref {
    pub name: String,
    /// 1 始まり
    pub line: u32,
    /// 1 始まり、UTF-16 単位
    pub col: u32,
    pub kind: u8,
    /// 参照を囲む最も内側の定義の名前
    pub container: Option<String>,
}

/// 分類のために見る祖先の段数。入れ子が深い入力でも一つの参照の分類を定数時間にする。
const CLASSIFY_DEPTH: usize = 3;
const IMPL_TRAIT_DEPTH: usize = 3;
/// ブロックや無名関数の引数と中身は、外側の call の一部ではない。
const SCOPE_WORDS: [&str; 5] = ["block", "lambda", "closure", "parameter", "arrow_function"];
const CALL_WORDS: [&str; 3] = ["call", "invocation", "send"];
const INHERIT_WORDS: [&str; 10] = [
    "superclass",
    "extends",
    "implements",
    "base_list",
    "heritage",
    "inheritance",
    "super_interfaces",
    "delegation_specifier",
    "base_class",
    "interfaces",
];

/// UTF-8 でない入力と Markdown は空を返す。
pub fn extract_refs(lang: Lang, src: &[u8], defs: &[Def]) -> Vec<Ref> {
    if std::str::from_utf8(src).is_err() {
        return Vec::new();
    }
    scan(lang, src, defs).unwrap_or_default()
}

fn scan(lang: Lang, src: &[u8], defs: &[Def]) -> Option<Vec<Ref>> {
    let mut parser = Parser::new();
    parser.set_language(&tags_config(lang)?.language).ok()?;
    let tree = parser.parse(src, None)?;

    let def_names: HashSet<(u32, u32)> = defs.iter().map(|d| (d.line, d.col)).collect();
    let mut order: Vec<usize> = (0..defs.len()).collect();
    order.sort_by_key(|&i| (defs[i].body.start, Reverse(defs[i].body.end)));
    let mut next = 0;
    let mut open: Vec<usize> = Vec::new();

    let mut refs = Vec::new();
    let mut cursor = tree.walk();
    // 木の再帰では深い入れ子でスタックが溢れるので、祖先の種類は自前のスタックに持つ
    let mut ancestors: Vec<Step> = Vec::new();
    // 各階層で、いまの節点より後ろにある名前付きの兄弟の数。next_named_sibling は根からたどるので使わない
    let mut afters: Vec<usize> = vec![0];
    loop {
        let node = cursor.node();
        if node.child_count() == 0 && node.is_named() && is_reference_kind(node.kind()) {
            let start = node.start_byte();
            let pos = node.start_position();
            let (line, col) = (pos.row as u32 + 1, utf16_col(src, start));
            if !def_names.contains(&(line, col)) {
                while next < order.len() && defs[order[next]].body.start <= start {
                    open.push(order[next]);
                    next += 1;
                }
                while open.last().is_some_and(|&i| defs[i].body.end <= start) {
                    open.pop();
                }
                let impl_item = (lang == Lang::Rust).then(|| trait_impl_of(node)).flatten();
                let (kind, container) = match impl_item {
                    Some(item) => (REF_INHERIT, impl_type_name(item, src)),
                    None => (
                        classify(&ancestors, step(&cursor, &afters)),
                        open.last().map(|&i| defs[i].name.clone()),
                    ),
                };
                refs.push(Ref {
                    name: String::from_utf8_lossy(&src[start..node.end_byte()]).into_owned(),
                    line,
                    col,
                    kind,
                    container,
                });
            }
        }
        let here = step(&cursor, &afters);
        if cursor.goto_first_child() {
            ancestors.push(here);
            let named = node.named_child_count() - usize::from(cursor.node().is_named());
            afters.push(named);
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                if cursor.node().is_named()
                    && let Some(n) = afters.last_mut()
                {
                    *n -= 1;
                }
                break;
            }
            if !cursor.goto_parent() {
                return Some(refs);
            }
            ancestors.pop();
            afters.pop();
        }
    }
}

/// `impl Trait for Type` のトレイト名そのもの（`a::Tr<T>` なら `Tr`）なら、その `impl_item`。
fn trait_impl_of(node: Node) -> Option<Node> {
    if node.kind() != "type_identifier" {
        return None;
    }
    let mut top = node;
    // トレイトの名前は impl_item の直下から数段のパスとジェネリクスの中にしか入らない
    for _ in 0..IMPL_TRAIT_DEPTH {
        let parent = top.parent()?;
        if parent.kind() == "impl_item" {
            let named = last_type_name(parent.child_by_field_name("trait")?)?;
            return (named.id() == node.id()).then_some(parent);
        }
        top = parent;
    }
    None
}

/// `impl` の型の名前。パスとジェネリクスは最後の名前だけを見る。
fn impl_type_name(item: Node, src: &[u8]) -> Option<String> {
    let name = last_type_name(item.child_by_field_name("type")?)?;
    std::str::from_utf8(&src[name.byte_range()])
        .ok()
        .map(str::to_string)
}

/// `a::b::Tr<T>` の `Tr`。これ以外の型（参照、タプルなど）は名前を持たない。
fn last_type_name(mut node: Node) -> Option<Node> {
    loop {
        node = match node.kind() {
            "generic_type" => node.child_by_field_name("type")?,
            "scoped_type_identifier" => node.child_by_field_name("name")?,
            "type_identifier" => return Some(node),
            _ => return None,
        };
    }
}

fn step<'a>(cursor: &TreeCursor<'a>, afters: &[usize]) -> Step<'a> {
    let node = cursor.node();
    Step {
        kind: node.kind(),
        field: cursor.field_name(),
        last: afters.last() == Some(&0),
    }
}

fn is_reference_kind(kind: &str) -> bool {
    kind.ends_with("identifier") || kind == "constant"
}

/// 木の一つの節点。`field` は親の中のフィールド名、`last` は親の最後の名前付きの子かどうか。
#[derive(Clone, Copy)]
struct Step<'a> {
    kind: &'a str,
    field: Option<&'a str>,
    last: bool,
}

const MEMBER_WORDS: [&str; 9] = [
    "member",
    "field_expression",
    "selector_expression",
    "navigation",
    "attribute",
    "scoped_identifier",
    "scoped_type_identifier",
    "scope_resolution",
    "qualified",
];

/// 引数の中の識別子と `obj.method()` の `obj` は呼び出しではない。
/// `argument` を通ったら打ち切り、メンバー参照では最後の名前だけを先へ進める。
/// Ruby の `call` は receiver と method を子に持つので、`receiver` フィールドで受け手を見分ける。
/// method は `a.b.c` の `.b` も呼び出しなので、種類に `call` を含む規則のまま CALL になる。
/// Python の基底クラスは `class_definition` の `superclasses` フィールドの argument_list に入る。
fn classify(ancestors: &[Step], leaf: Step) -> u8 {
    let mut below = leaf;
    for a in ancestors.iter().rev().take(CLASSIFY_DEPTH) {
        if below.field == Some("receiver") {
            return REF_PLAIN;
        }
        if a.field == Some("superclasses") {
            return REF_INHERIT;
        }
        if a.kind.contains("argument") {
            return REF_PLAIN;
        }
        if !below.last && MEMBER_WORDS.iter().any(|w| a.kind.contains(w)) {
            return REF_PLAIN;
        }
        if CALL_WORDS.iter().any(|w| a.kind.contains(w)) {
            return REF_CALL;
        }
        if INHERIT_WORDS.iter().any(|w| a.kind.contains(w)) {
            return REF_INHERIT;
        }
        if SCOPE_WORDS.iter().any(|w| a.kind.contains(w)) {
            return REF_PLAIN;
        }
        below = *a;
    }
    REF_PLAIN
}

/// 1 始まりの UTF-16 列。`start` は文字の境界で、src 全体は UTF-8 として検査済み。
fn utf16_col(src: &[u8], start: usize) -> u32 {
    let line_start = src[..start]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |i| i + 1);
    let prefix = std::str::from_utf8(&src[line_start..start]).unwrap_or_default();
    prefix.encode_utf16().count() as u32 + 1
}
