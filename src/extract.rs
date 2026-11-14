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
    let mut outer: Vec<Option<String>> = Vec::new();
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
        outer.push(outer_type(lang, body, name_node, src));
    }
    let containers = containers(&defs, &names);
    for (d, (inner, out)) in defs.iter_mut().zip(containers.into_iter().zip(outer)) {
        d.container = out.or(inner);
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
        // 自分自身は container になれないので、判定の間だけスタックから外す
        let own = stack.pop_if(|h| *h == i);
        while stack
            .last()
            .is_some_and(|&h| defs[h].body.end < names[i].end)
        {
            stack.pop();
        }
        out[i] = stack.last().map(|&h| defs[h].name.clone());
        stack.extend(own);
    }
    out
}

/// 型の本体の外に書かれるメソッドの所属型（Rust の impl、Go のレシーバ、C++ の `A::f`、
/// Swift の extension、Lua の `T.f` / `T:f`、Ruby の `def T.f`、JS の `T.prototype.f = ...`）。
fn outer_type(lang: Lang, body: Node<'_>, name: Node<'_>, src: &[u8]) -> Option<String> {
    let owner = match lang {
        Lang::Rust => {
            let imp = body
                .parent()
                .filter(|p| p.kind() == "declaration_list")?
                .parent()
                .filter(|p| p.kind() == "impl_item")?;
            imp.child_by_field_name("type")?
        }
        // レシーバ名ではなく型だけを見る
        Lang::Go => body
            .child_by_field_name("receiver")?
            .named_child(0)?
            .child_by_field_name("type")?,
        Lang::Cpp => name
            .parent()
            .filter(|p| p.kind() == "qualified_identifier")?
            .child_by_field_name("scope")?,
        Lang::Swift => ancestor(body, |p| {
            matches!(p.kind(), "class_declaration" | "protocol_declaration")
        })?
        .child_by_field_name("name")?,
        Lang::Lua => {
            let table = name
                .parent()
                .filter(|p| matches!(p.kind(), "dot_index_expression" | "method_index_expression"))?
                .child_by_field_name("table")?;
            // `a.b.f` の所属は最も近い `b`
            table.child_by_field_name("field").unwrap_or(table)
        }
        // `def self.f` の object は self なので型名が見つからず、囲む class のままになる
        Lang::Ruby => match name.parent().filter(|p| p.kind() == "scope_resolution") {
            // `class A::B::C` の所属は `A.B`。`::` は検索語と同じ `.` にそろえる
            Some(path) => {
                let scope = path.child_by_field_name("scope")?;
                let text = String::from_utf8_lossy(&src[scope.byte_range()]);
                return Some(text.trim_start_matches("::").replace("::", "."));
            }
            None => body.child_by_field_name("object")?,
        },
        // `Foo.prototype.f` の最初の識別子は Foo
        Lang::JavaScript | Lang::TypeScript | Lang::Tsx => name
            .parent()
            .filter(|p| p.kind() == "member_expression")?
            .child_by_field_name("object")?,
        _ => return None,
    };
    first_type_name(owner, src, lang)
}

/// 型名の探索で辿る段数の上限。深く入れ子にした入力でスタックや時間を使い切らないため。
const TYPE_NAME_STEPS: usize = 64;
/// 親をたどる段数の上限。メンバーは囲む宣言の数段下にある。
const ANCESTOR_STEPS: usize = 16;

/// 型名の識別子。`name` / `type` フィールドか最初の子を辿るので、ジェネリクスや `*`、`&`、
/// `a::Foo`、`Foo.prototype` を読み飛ばせる。上限を超えたら None。
fn first_type_name(mut n: Node<'_>, src: &[u8], lang: Lang) -> Option<String> {
    // Go のレシーバ名や Rust のパスなど、型名でない identifier を拾わない言語を分ける
    let ident = match lang {
        Lang::Lua | Lang::JavaScript | Lang::TypeScript | Lang::Tsx => "identifier",
        Lang::Ruby => "constant",
        Lang::Cpp => "namespace_identifier",
        _ => "type_identifier",
    };
    for _ in 0..TYPE_NAME_STEPS {
        if n.kind() == ident {
            return Some(String::from_utf8_lossy(&src[n.byte_range()]).into_owned());
        }
        n = n
            .child_by_field_name("name")
            .or_else(|| n.child_by_field_name("type"))
            .or_else(|| n.named_child(0))?;
    }
    None
}

/// 範囲が同じ入れ子のノードの一番外側（Dart は method_signature と function_signature が同じ範囲になる）。
fn outermost(mut n: Node<'_>) -> Node<'_> {
    while let Some(p) = n.parent().filter(|p| p.byte_range() == n.byte_range()) {
        n = p;
    }
    n
}

fn ancestor<'a>(mut n: Node<'a>, pred: impl Fn(Node<'a>) -> bool) -> Option<Node<'a>> {
    for _ in 0..ANCESTOR_STEPS {
        let p = n.parent()?;
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

#[cfg(test)]
mod tests {
    use super::*;

    type Item<'a> = (&'a str, &'a str, Range<usize>, Range<usize>);

    fn run(items: &[Item]) -> Vec<Option<String>> {
        let defs: Vec<Def> = items
            .iter()
            .map(|i| Def {
                name: i.0.into(),
                line: 1,
                col: 1,
                kind: kind_id(i.1),
                container: None,
                body: i.2.clone(),
            })
            .collect();
        let names: Vec<_> = items.iter().map(|i| i.3.clone()).collect();
        containers(&defs, &names)
    }

    #[test]
    fn holder_ending_exactly_at_name_end_still_contains_it() {
        let c = run(&[("A", "class", 0..10, 0..1), ("x", "function", 5..10, 5..10)]);
        assert_eq!(c[1].as_deref(), Some("A"));
    }

    #[test]
    fn holder_ending_before_the_name_does_not_contain_it() {
        let c = run(&[
            ("A", "class", 0..10, 0..1),
            ("x", "function", 20..30, 25..26),
        ]);
        assert_eq!(c[1], None);
    }

    #[test]
    fn holder_starting_exactly_at_name_start_contains_it() {
        let c = run(&[("A", "class", 5..20, 5..6), ("x", "function", 5..20, 5..6)]);
        // 同じ範囲の2定義。x は A の中、A は自分自身を除くので None
        assert_eq!((c[0].as_deref(), c[1].as_deref()), (None, Some("A")));
    }

    #[test]
    fn innermost_holder_wins_and_siblings_do_not_leak() {
        let c = run(&[
            ("A", "module", 0..100, 0..1),
            ("B", "class", 10..40, 11..12),
            ("C", "class", 50..90, 51..52),
            ("m", "function", 20..30, 21..22),
            ("n", "function", 60..70, 61..62),
        ]);
        let got: Vec<_> = c.iter().map(|x| x.as_deref()).collect();
        assert_eq!(got, [None, Some("A"), Some("A"), Some("B"), Some("C")]);
    }

    #[test]
    fn non_holder_kinds_are_never_containers() {
        let c = run(&[
            ("f", "function", 0..50, 0..1),
            ("g", "function", 10..20, 11..12),
        ]);
        assert_eq!(c, [None, None]);
    }
}
