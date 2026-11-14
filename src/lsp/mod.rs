//! 言語サーバーを問い合わせのたびに起動し、答えを得たら止める。

mod client;
mod servers;

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::lang::Lang;
use crate::store;

pub use client::query_with;
pub use servers::{ServerSpec, pick_java_home, server_for};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    References,
    IncomingCalls,
    Implementation,
    Supertypes,
}

/// 行と列は1始まり。列は UTF-16 の符号単位で数える（LSP の既定）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LspError {
    NotInstalled(String),
    StartFailed(String),
    Timeout,
    Unsupported(&'static str),
    Protocol(String),
}

impl fmt::Display for LspError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LspError::NotInstalled(what) => write!(f, "{what}が見つかりません"),
            LspError::StartFailed(why) => write!(f, "起動できませんでした（{why}）"),
            LspError::Timeout => write!(f, "期限内に答えがありませんでした"),
            LspError::Unsupported(what) => write!(f, "{what}は言語サーバーで調べられません"),
            LspError::Protocol(why) => write!(f, "応答を読めませんでした（{why}）"),
        }
    }
}

pub fn query(
    root: &Path,
    lang: Lang,
    file: &Path,
    line: u32,
    col: u32,
    method: Method,
    deadline: Instant,
) -> Result<Vec<Location>, LspError> {
    let spec = with_workspace(server_for(lang)?, lang, root);
    query_with(&spec, root, lang, file, line, col, method, deadline)
}

/// jdtls は -data に作業領域を作る。作業ツリーの外に置く。
fn with_workspace(mut spec: ServerSpec, lang: Lang, root: &Path) -> ServerSpec {
    if lang == Lang::Java {
        spec.args.push("-data".into());
        spec.args.push(store::cache_dir(root).join("jdtls").into());
    }
    spec
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_jdtls_gets_a_workspace_outside_the_tree() {
        let spec = || ServerSpec {
            name: "s",
            program: "s".into(),
            args: vec![],
            env: vec![],
        };
        let root = Path::new("/r");
        let java = with_workspace(spec(), Lang::Java, root);
        let data = store::cache_dir(root).join("jdtls");
        assert_eq!(java.args, ["-data".into(), data.into_os_string()]);
        assert!(with_workspace(spec(), Lang::Go, root).args.is_empty());
    }

    #[test]
    fn errors_read_as_one_japanese_line() {
        let cases = [
            (
                LspError::NotInstalled("gopls".into()),
                "goplsが見つかりません",
            ),
            (
                LspError::StartFailed("x".into()),
                "起動できませんでした（x）",
            ),
            (LspError::Timeout, "期限内に答えがありませんでした"),
            (
                LspError::Unsupported("Markdown"),
                "Markdownは言語サーバーで調べられません",
            ),
            (
                LspError::Protocol("y".into()),
                "応答を読めませんでした（y）",
            ),
        ];
        for (err, want) in cases {
            assert_eq!(err.to_string(), want);
        }
    }
}
