use std::{path::Path, sync::OnceLock};

use tree_sitter::Language;
use tree_sitter_tags::TagsConfiguration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Rust,
    Python,
    TypeScript,
    Tsx,
    JavaScript,
    Go,
    Ruby,
    Java,
    C,
    Cpp,
    CSharp,
    Swift,
    Dart,
    Lua,
    Markdown,
}

impl Lang {
    pub const ALL: [Lang; 15] = [
        Lang::Rust,
        Lang::Python,
        Lang::TypeScript,
        Lang::Tsx,
        Lang::JavaScript,
        Lang::Go,
        Lang::Ruby,
        Lang::Java,
        Lang::C,
        Lang::Cpp,
        Lang::CSharp,
        Lang::Swift,
        Lang::Dart,
        Lang::Lua,
        Lang::Markdown,
    ];
}

pub fn lang_of(path: &Path) -> Option<Lang> {
    let lang = match path.extension()?.to_str()? {
        "rs" => Lang::Rust,
        "py" | "pyi" => Lang::Python,
        "ts" | "mts" | "cts" => Lang::TypeScript,
        "tsx" => Lang::Tsx,
        "js" | "mjs" | "cjs" | "jsx" => Lang::JavaScript,
        "go" => Lang::Go,
        "rb" => Lang::Ruby,
        "java" => Lang::Java,
        "c" => Lang::C,
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "h" => Lang::Cpp,
        "cs" => Lang::CSharp,
        "swift" => Lang::Swift,
        "dart" => Lang::Dart,
        "lua" => Lang::Lua,
        "md" | "markdown" => Lang::Markdown,
        _ => return None,
    };
    Some(lang)
}

/// 同梱の tags.scm が捕捉しない定義。struct と enum は Rust の tags.scm に合わせて class にする。
/// ローカル関数の container は囲む型になる（メソッドは container にならない）。
/// 索引の指紋に入れるので、変えると既存の索引は作り直される。
const CSHARP_EXTRA: &str = "\
(enum_declaration name: (identifier) @name) @definition.class
(struct_declaration name: (identifier) @name) @definition.class
(record_declaration name: (identifier) @name) @definition.class
(local_function_statement name: (identifier) @name) @definition.function";

/// `type A = B` は tags.scm の type_spec に当たらない。同じ名前の位置を取る `@reference.type` より
/// 先に当たるよう、tags.scm の前に置く。
const GO_EXTRA: &str = "(type_alias name: (type_identifier) @name) @definition.type";

/// Java の tags.scm は class、interface、method しか捕捉しない。
const JAVA_EXTRA: &str = "\
(enum_declaration name: (identifier) @name) @definition.class
(record_declaration name: (identifier) @name) @definition.class
(annotation_type_declaration name: (identifier) @name) @definition.class";

/// 追加した query の全文。索引の指紋の入力にする。
pub fn extra_queries() -> String {
    format!("{CSHARP_EXTRA}\n{GO_EXTRA}\n{JAVA_EXTRA}")
}

static CONFIGS: [OnceLock<TagsConfiguration>; Lang::ALL.len()] =
    [const { OnceLock::new() }; Lang::ALL.len()];

/// Markdown の見出しは tags ではなく別経路で取るので None を返す。
/// 言語ごとに初回だけコンパイルする（Swift は約170msかかり、全言語を一度に作ると起動が遅れる）。
pub fn tags_config(lang: Lang) -> Option<&'static TagsConfiguration> {
    if lang == Lang::Markdown {
        return None;
    }
    Some(CONFIGS[lang as usize].get_or_init(|| build(lang)))
}

fn build(lang: Lang) -> TagsConfiguration {
    let js = tree_sitter_javascript::TAGS_QUERY;
    let (language, query): (Language, String) = match lang {
        Lang::Rust => (
            tree_sitter_rust::LANGUAGE.into(),
            tree_sitter_rust::TAGS_QUERY.into(),
        ),
        Lang::Python => (
            tree_sitter_python::LANGUAGE.into(),
            tree_sitter_python::TAGS_QUERY.into(),
        ),
        // TS の tags query は JS の query を継承する前提で書かれている
        Lang::TypeScript => (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            format!("{js}\n{}", tree_sitter_typescript::TAGS_QUERY),
        ),
        Lang::Tsx => (
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            format!("{js}\n{}", tree_sitter_typescript::TAGS_QUERY),
        ),
        Lang::JavaScript => (tree_sitter_javascript::LANGUAGE.into(), js.into()),
        Lang::Go => (
            tree_sitter_go::LANGUAGE.into(),
            format!("{GO_EXTRA}\n{}", tree_sitter_go::TAGS_QUERY),
        ),
        Lang::Ruby => (
            tree_sitter_ruby::LANGUAGE.into(),
            tree_sitter_ruby::TAGS_QUERY.into(),
        ),
        Lang::Java => (
            tree_sitter_java::LANGUAGE.into(),
            format!("{}\n{JAVA_EXTRA}", tree_sitter_java::TAGS_QUERY),
        ),
        Lang::C => (
            tree_sitter_c::LANGUAGE.into(),
            tree_sitter_c::TAGS_QUERY.into(),
        ),
        Lang::Cpp => (
            tree_sitter_cpp::LANGUAGE.into(),
            tree_sitter_cpp::TAGS_QUERY.into(),
        ),
        // `@module` だけを捕捉するパターンを tree-sitter-tags が InvalidCapture で拒否する
        Lang::CSharp => (
            tree_sitter_c_sharp::LANGUAGE.into(),
            tree_sitter_c_sharp::TAGS_QUERY
                .lines()
                .filter(|l| !l.trim_end().ends_with(" @module"))
                .chain([CSHARP_EXTRA])
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        Lang::Swift => (
            tree_sitter_swift::LANGUAGE.into(),
            tree_sitter_swift::TAGS_QUERY.into(),
        ),
        Lang::Dart => (
            tree_sitter_dart::LANGUAGE.into(),
            tree_sitter_dart::TAGS_QUERY.into(),
        ),
        Lang::Lua => (
            tree_sitter_lua::LANGUAGE.into(),
            tree_sitter_lua::TAGS_QUERY.into(),
        ),
        Lang::Markdown => unreachable!("tags_config は Markdown を弾く"),
    };
    TagsConfiguration::new(language, &query, "")
        .unwrap_or_else(|e| panic!("{lang:?} の tags query を読めない: {e}"))
}

/// 種類名（`definition.` を除いた capture 名）を保存用の固定番号にする。
pub fn kind_id(capture: &str) -> u8 {
    match capture {
        "class" => 1,
        "constant" => 2,
        "function" => 3,
        "interface" => 4,
        "macro" => 5,
        "method" => 6,
        "module" => 7,
        "property" => 8,
        "type" => 9,
        "variable" => 10,
        "section" => 11,
        _ => 0,
    }
}

const KIND_NAMES: [&str; 12] = [
    "other",
    "class",
    "constant",
    "function",
    "interface",
    "macro",
    "method",
    "module",
    "property",
    "type",
    "variable",
    "section",
];

/// `kind_id` の逆引き。未知の番号は "other"。
pub fn kind_name(id: u8) -> &'static str {
    KIND_NAMES
        .get(usize::from(id))
        .copied()
        .unwrap_or(KIND_NAMES[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_language_query_compiles() {
        for lang in Lang::ALL {
            assert_eq!(
                tags_config(lang).is_some(),
                lang != Lang::Markdown,
                "{lang:?}"
            );
        }
    }

    #[test]
    fn extra_queries_feed_the_index_fingerprint() {
        let all = extra_queries();
        assert!(
            [CSHARP_EXTRA, GO_EXTRA, JAVA_EXTRA]
                .iter()
                .all(|q| all.contains(q))
        );
    }

    #[test]
    fn extension_mapping() {
        let of = |p: &str| lang_of(Path::new(p));
        assert_eq!(of("a.h"), Some(Lang::Cpp));
        assert_eq!(of("a.tsx"), Some(Lang::Tsx));
        assert_eq!(of("a.md"), Some(Lang::Markdown));
        assert_eq!(of("a.kt"), None);
        assert_eq!(of("Makefile"), None);
    }

    #[test]
    fn kind_ids_are_fixed() {
        let names = [
            "other",
            "class",
            "constant",
            "function",
            "interface",
            "macro",
            "method",
            "module",
            "property",
            "type",
            "variable",
            "section",
        ];
        for (id, name) in names.iter().enumerate() {
            assert_eq!(kind_id(name) as usize, id, "{name}");
            assert_eq!(kind_name(id as u8), *name);
        }
        assert_eq!(kind_id("nope"), 0);
        assert_eq!(kind_name(12), "other");
        assert_eq!(kind_name(255), "other");
    }
}
