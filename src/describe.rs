use crate::lang::Lang;
use crate::markdown::headings;

const DESCRIBE_LINES: usize = 20;

/// 文字数からの token 見積もり。コードは 3.5 字、それ以外は 4.0 字で 1 token（小数は切り捨て）。
pub fn tokens(chars: usize, lang: Option<Lang>) -> usize {
    match lang {
        Some(Lang::Markdown) | None => chars / 4,
        Some(_) => chars * 2 / 7,
    }
}

/// Markdown は最初の見出し。それ以外は先頭 20 行にある最初のコメントの本文。
pub fn description(text: &str, lang: Option<Lang>) -> String {
    if lang == Some(Lang::Markdown) {
        return headings(text)
            .into_iter()
            .next()
            .map(|d| d.name)
            .unwrap_or_default();
    }
    if lang == Some(Lang::Python)
        && let Some(doc) = py_docstring(text)
    {
        return doc;
    }
    text.lines()
        .take(DESCRIBE_LINES)
        .find_map(comment_text)
        .unwrap_or_default()
        .to_string()
}

/// 先頭の文が `"""` か `'''` の文字列ならその最初の行。`#` の行（shebang、エンコーディング宣言）は読み飛ばす。
fn py_docstring(text: &str) -> Option<String> {
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'));
    let first = lines.next()?;
    let rest = ["\"\"\"", "'''"]
        .iter()
        .find_map(|q| first.strip_prefix(q))?;
    let clean = |l: &str| l.trim().trim_end_matches(['"', '\'']).trim().to_string();
    let head = clean(rest);
    if !head.is_empty() {
        return Some(head);
    }
    lines.map(|l| clean(l)).find(|l| !l.is_empty())
}

/// `#!` と `#include` は `# ` で始まらないのでコメントに数えない。
fn comment_text(line: &str) -> Option<&str> {
    let t = line.trim();
    let body = if t.starts_with("//") {
        t.trim_start_matches('/').trim_start_matches('!')
    } else if t.starts_with("--") {
        t.trim_start_matches('-')
    } else {
        t.strip_prefix("# ")?
    };
    Some(body.trim()).filter(|b| !b.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_ratios() {
        assert_eq!(tokens(7, Some(Lang::Rust)), 2);
        assert_eq!(tokens(6, Some(Lang::Rust)), 1);
        assert_eq!(tokens(8, Some(Lang::Markdown)), 2);
        assert_eq!(tokens(7, None), 1);
    }

    #[test]
    fn comment_prefixes() {
        assert_eq!(comment_text("  /// doc "), Some("doc"));
        assert_eq!(comment_text("//! inner"), Some("inner"));
        assert_eq!(comment_text("-- lua"), Some("lua"));
        assert_eq!(comment_text("# py"), Some("py"));
        assert_eq!(comment_text("#!/bin/sh"), None);
        assert_eq!(comment_text("#include <a.h>"), None);
        assert_eq!(comment_text("//"), None);
        assert_eq!(comment_text("let a = 1;"), None);
    }

    #[test]
    fn python_docstring_cases() {
        let d = |t: &str| description(t, Some(Lang::Python));
        assert_eq!(d("\"\"\"Tool for x.\"\"\"\ndef f(): pass\n"), "Tool for x.");
        assert_eq!(
            d("#!/usr/bin/env python\n# -*- coding: utf-8 -*-\n\n'''Second quote.\nmore\n'''\n"),
            "Second quote."
        );
        assert_eq!(d("\"\"\"\n  Next line doc.\n\"\"\"\n"), "Next line doc.");
        assert_eq!(d("\"\"\"\n\"\"\"\n# note\n"), "note");
        assert_eq!(
            d("import os\n\"\"\"not a docstring\"\"\"\n# real comment\n"),
            "real comment"
        );
        assert_eq!(d("# only comment\ndef f(): pass\n"), "only comment");
        assert_eq!(d(""), "");
    }

    #[test]
    fn markdown_uses_the_first_heading() {
        assert_eq!(
            description("intro\n# Title\n## Sub\n", Some(Lang::Markdown)),
            "Title"
        );
        assert_eq!(description("no heading", Some(Lang::Markdown)), "");
    }
}
