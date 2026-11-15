//! 言語サーバーを問い合わせのたびに起動し、答えを得たら止める。

mod client;
mod servers;

use std::env;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::lang::Lang;
use crate::store;

pub use client::query_with;
pub use servers::{
    ServerSpec, Setup, install_command, pick_java_home, server_for, server_name, setup,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    References,
    IncomingCalls,
    Implementation,
    Supertypes,
}

/// 行と列は1始まり。列は UTF-16 の符号単位で数える（LSP の既定）。
/// 順序はパス、行、列の順（フィールドの並びがそのまま比較の順になる）。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
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
    /// 答えは来たが、続けて問うた答えが期限まで一致しなかった。
    Unsettled,
    Unsupported(&'static str),
    Protocol(String),
}

impl fmt::Display for LspError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LspError::NotInstalled(what) => write!(f, "{what}が見つかりません"),
            LspError::StartFailed(why) => write!(f, "起動できませんでした（{why}）"),
            LspError::Timeout => write!(f, "期限内に答えがありませんでした"),
            LspError::Unsettled => write!(f, "答えが期限内に定まりませんでした"),
            LspError::Unsupported(what) => write!(f, "{what}には言語サーバーがありません"),
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
    // clangd は compile_commands.json が無いと開いたファイルしか見ず、少ない答えを返す。
    if matches!(lang, Lang::C | Lang::Cpp) && !root.join("compile_commands.json").is_file() {
        return Err(LspError::NotInstalled("compile_commands.json".into()));
    }
    let spec = match replaced(lang) {
        Some(spec) => spec,
        None => with_workspace(server_for(lang)?, lang, root),
    };
    query_with(&spec, root, lang, file, line, col, method, deadline)
}

/// 試験用の差し替え口。`FERRINDEX_LSP_RUST="python3 fake.py"` のように起動コマンドを渡す。
/// shortcut: 空白で区切るだけなので、空白を含むパスは渡せない。必要になったら引用を解釈する。
fn replaced(lang: Lang) -> Option<ServerSpec> {
    let var = format!("FERRINDEX_LSP_{}", format!("{lang:?}").to_uppercase());
    let line = env::var(var).ok()?;
    let mut words = line.split_whitespace();
    Some(ServerSpec {
        name: server_name(lang),
        program: words.next()?.into(),
        args: words.map(Into::into).collect(),
        env: vec![],
    })
}

/// jdtls は -data に作業領域を作り、.project などを作業ツリーに作る。rust-analyzer は cargo に
/// target/ を、sourcekit-lsp は SwiftPM に .build/ を作らせる。どれも作業ツリーの外に置く。jdtls はこの設定を `initializationOptions`
/// でなくシステムプロパティからだけ読む（JLSFsUtils.generatesMetadataFilesAtProjectRoot）。
fn with_workspace(mut spec: ServerSpec, lang: Lang, root: &Path) -> ServerSpec {
    match lang {
        Lang::Java => {
            spec.args.push("-data".into());
            spec.args.push(store::cache_dir(root).join("jdtls").into());
            spec.args
                .push("--jvm-arg=-Djava.import.generatesMetadataFilesAtProjectRoot=false".into());
        }
        Lang::Rust => {
            let target = store::cache_dir(root).join("cargo-target");
            spec.env.push(("CARGO_TARGET_DIR", target.into()));
        }
        Lang::Swift => {
            spec.args.push("--scratch-path".into());
            spec.args
                .push(store::cache_dir(root).join("swiftpm").into());
        }
        _ => {}
    }
    spec
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn servers_write_outside_the_tree() {
        let spec = || ServerSpec {
            name: "s",
            program: "s".into(),
            args: vec![],
            env: vec![],
        };
        let root = Path::new("/r");
        let java = with_workspace(spec(), Lang::Java, root);
        let data = store::cache_dir(root).join("jdtls");
        assert_eq!(
            java.args,
            [
                "-data".into(),
                data.into_os_string(),
                "--jvm-arg=-Djava.import.generatesMetadataFilesAtProjectRoot=false".into(),
            ]
        );
        assert!(java.env.is_empty());
        let rust = with_workspace(spec(), Lang::Rust, root);
        let target = store::cache_dir(root).join("cargo-target");
        assert_eq!(rust.env, [("CARGO_TARGET_DIR", target.into_os_string())]);
        assert!(rust.args.is_empty());
        let swift = with_workspace(spec(), Lang::Swift, root);
        let scratch = store::cache_dir(root).join("swiftpm");
        assert_eq!(
            swift.args,
            ["--scratch-path".into(), scratch.into_os_string()]
        );
        let go = with_workspace(spec(), Lang::Go, root);
        assert!(go.args.is_empty() && go.env.is_empty());
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
            (LspError::Unsettled, "答えが期限内に定まりませんでした"),
            (
                LspError::Unsupported("Markdown"),
                "Markdownには言語サーバーがありません",
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
