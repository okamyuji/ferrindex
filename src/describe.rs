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
    text.lines()
        .take(DESCRIBE_LINES)
        .find_map(comment_text)
        .unwrap_or_default()
        .to_string()
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
    fn markdown_uses_the_first_heading() {
        assert_eq!(
            description("intro\n# Title\n## Sub\n", Some(Lang::Markdown)),
            "Title"
        );
        assert_eq!(description("no heading", Some(Lang::Markdown)), "");
    }
}
