use std::env;
use std::ffi::OsString;
use std::fs;
use std::iter;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::LspError;
use crate::lang::Lang;

const BREW_JDK: &str = "/opt/homebrew/opt/openjdk/libexec/openjdk.jdk/Contents/Home";
const JAVA_HOME_TOOL: &str = "/usr/libexec/java_home";
const DOTNET_ROOTS: [&str; 2] = [
    "/opt/homebrew/opt/dotnet/libexec",
    "/usr/local/share/dotnet",
];
/// jdtls 1.63 はこれより古い JDK では `initialize` に答えない。
const MIN_JDK: u32 = 25;

#[derive(Clone, Debug)]
pub struct ServerSpec {
    pub name: &'static str,
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(&'static str, OsString)>,
}

/// 言語、didOpen の languageId、起動コマンド。Markdown には言語サーバーが無いので載せない。
const COMMANDS: [(Lang, &str, &str, &[&str]); 14] = [
    (Lang::Rust, "rust", "rust-analyzer", &[]),
    (Lang::Go, "go", "gopls", &[]),
    (Lang::Python, "python", "pyright-langserver", &["--stdio"]),
    (
        Lang::JavaScript,
        "javascript",
        "typescript-language-server",
        &["--stdio"],
    ),
    (
        Lang::TypeScript,
        "typescript",
        "typescript-language-server",
        &["--stdio"],
    ),
    (
        Lang::Tsx,
        "typescriptreact",
        "typescript-language-server",
        &["--stdio"],
    ),
    (Lang::Java, "java", "jdtls", &[]),
    (Lang::CSharp, "csharp", "csharp-ls", &[]),
    (Lang::Ruby, "ruby", "solargraph", &["stdio"]),
    (Lang::C, "c", "clangd", &[]),
    (Lang::Cpp, "cpp", "clangd", &[]),
    (Lang::Swift, "swift", "sourcekit-lsp", &[]),
    (Lang::Lua, "lua", "lua-language-server", &[]),
    (Lang::Dart, "dart", "dart", &["language-server"]),
];

fn row(lang: Lang) -> Option<&'static (Lang, &'static str, &'static str, &'static [&'static str])> {
    COMMANDS.iter().find(|r| r.0 == lang)
}

fn command_of(lang: Lang) -> Option<(&'static str, &'static [&'static str])> {
    row(lang).map(|&(_, _, name, args)| (name, args))
}

pub(super) fn language_id(lang: Lang) -> &'static str {
    row(lang).map_or("markdown", |r| r.1)
}

pub fn server_for(lang: Lang) -> Result<ServerSpec, LspError> {
    let mut dirs: Vec<PathBuf> = env::var_os("PATH")
        .map(|p| env::split_paths(&p).collect())
        .unwrap_or_default();
    // `dotnet tool install -g` は PATH に載らないことが多い。
    dirs.extend(env::home_dir().map(|h| h.join(".dotnet/tools")));
    let java_homes = env::var_os("FERRINDEX_JAVA_HOME")
        .map(PathBuf::from)
        .into_iter()
        .chain([PathBuf::from(BREW_JDK)])
        .chain(iter::once_with(|| java_home_from(Path::new(JAVA_HOME_TOOL))).flatten());
    let dotnet_roots = env::var_os("DOTNET_ROOT")
        .map(PathBuf::from)
        .into_iter()
        .chain(DOTNET_ROOTS.map(PathBuf::from));
    spec_in(lang, &dirs, java_homes, dotnet_roots)
}

fn java_home_from(tool: &Path) -> Option<PathBuf> {
    let out = Command::new(tool)
        .args(["-v", &format!("{MIN_JDK}+")])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    Some(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

fn spec_in(
    lang: Lang,
    dirs: &[PathBuf],
    java_homes: impl IntoIterator<Item = PathBuf>,
    dotnet_roots: impl IntoIterator<Item = PathBuf>,
) -> Result<ServerSpec, LspError> {
    let (name, args) = command_of(lang).ok_or(LspError::Unsupported("Markdown"))?;
    let program = dirs
        .iter()
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .ok_or_else(|| LspError::NotInstalled(name.into()))?;
    let env = match lang {
        Lang::Java => {
            let home = pick_java_home(java_homes)
                .ok_or_else(|| LspError::NotInstalled(format!("JDK {MIN_JDK}以上")))?;
            vec![("JAVA_HOME", home.into())]
        }
        Lang::CSharp => {
            let root = dotnet_roots
                .into_iter()
                .find(|p| p.is_dir())
                .ok_or_else(|| LspError::NotInstalled("dotnet".into()))?;
            vec![
                ("DOTNET_ROOT", root.into()),
                ("DOTNET_ROLL_FORWARD", "Major".into()),
            ]
        }
        _ => Vec::new(),
    };
    Ok(ServerSpec {
        name,
        program,
        args: args.iter().map(OsString::from).collect(),
        env,
    })
}

/// 候補の `release` ファイルの `JAVA_VERSION` を読み、最初の JDK 25 以上を選ぶ。
pub fn pick_java_home(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find(|home| {
        fs::read_to_string(home.join("release"))
            .ok()
            .and_then(|text| java_major(&text))
            .is_some_and(|major| major >= MIN_JDK)
    })
}

fn java_major(release: &str) -> Option<u32> {
    let version = release
        .lines()
        .find_map(|l| l.strip_prefix("JAVA_VERSION="))?;
    version
        .trim_matches('"')
        .split(['.', '-', '+'])
        .next()?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn java_home_tool_is_asked_for_25_or_later() {
        let t = TempDir::new().unwrap();
        let found = script(
            t.path(),
            "ok",
            r#"[ "$1 $2" = "-v 25+" ] && echo /jdk/home || exit 1"#,
        );
        assert_eq!(java_home_from(&found), Some(PathBuf::from("/jdk/home")));
        let none = script(t.path(), "none", "echo /jdk/home; exit 1");
        assert_eq!(java_home_from(&none), None);
        assert_eq!(java_home_from(&t.path().join("missing")), None);
    }

    #[test]
    fn every_language_but_markdown_has_a_command() {
        for lang in Lang::ALL {
            assert_eq!(
                command_of(lang).is_none(),
                lang == Lang::Markdown,
                "{lang:?}"
            );
        }
        assert_eq!(command_of(Lang::Ruby), Some(("solargraph", &["stdio"][..])));
        assert_eq!(language_id(Lang::Tsx), "typescriptreact");
        assert_eq!(language_id(Lang::Markdown), "markdown");
    }

    fn touch(dir: &Path, name: &str) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        fs::write(&p, "").unwrap();
        p
    }

    fn none() -> iter::Empty<PathBuf> {
        iter::empty()
    }

    #[test]
    fn program_is_found_in_a_later_dir() {
        let t = TempDir::new().unwrap();
        let (empty, bin) = (t.path().join("a"), t.path().join("b"));
        fs::create_dir(&empty).unwrap();
        let exe = touch(&bin, "dart");
        let spec = spec_in(Lang::Dart, &[empty, bin], none(), none()).unwrap();
        assert_eq!((spec.name, spec.program), ("dart", exe));
        assert_eq!(spec.args, ["language-server"]);
        assert!(spec.env.is_empty());
    }

    #[test]
    fn missing_program_or_runtime_is_not_installed() {
        let t = TempDir::new().unwrap();
        let dirs = [t.path().to_path_buf()];
        let err = spec_in(Lang::Go, &dirs, none(), none()).unwrap_err();
        assert_eq!(err, LspError::NotInstalled("gopls".into()));
        touch(t.path(), "jdtls");
        touch(t.path(), "csharp-ls");
        let err = spec_in(Lang::Java, &dirs, none(), none()).unwrap_err();
        assert_eq!(err, LspError::NotInstalled("JDK 25以上".into()));
        let not_dir = touch(t.path(), "file");
        let err = spec_in(Lang::CSharp, &dirs, none(), [not_dir]).unwrap_err();
        assert_eq!(err, LspError::NotInstalled("dotnet".into()));
    }

    #[test]
    fn java_and_csharp_get_their_runtime_env() {
        let t = TempDir::new().unwrap();
        let dirs = [t.path().join("bin")];
        touch(&dirs[0], "jdtls");
        touch(&dirs[0], "csharp-ls");
        let jdk = t.path().join("jdk");
        fs::create_dir(&jdk).unwrap();
        fs::write(jdk.join("release"), "JAVA_VERSION=\"27-ea\"\n").unwrap();
        let spec = spec_in(Lang::Java, &dirs, [jdk.clone()], none()).unwrap();
        assert_eq!(spec.env, [("JAVA_HOME", jdk.into_os_string())]);
        let spec = spec_in(
            Lang::CSharp,
            &dirs,
            none(),
            [t.path().join("no"), dirs[0].clone()],
        )
        .unwrap();
        let want = [
            ("DOTNET_ROOT", dirs[0].clone().into_os_string()),
            ("DOTNET_ROLL_FORWARD", "Major".into()),
        ];
        assert_eq!(spec.env, want);
    }

    #[test]
    fn java_major_reads_the_first_number() {
        assert_eq!(java_major("A=1\nJAVA_VERSION=\"25.0.1\"\n"), Some(25));
        assert_eq!(java_major("JAVA_VERSION=\"1.8.0_392\""), Some(1));
        assert_eq!(java_major("JAVA_VERSION=\"26+35\""), Some(26));
        assert_eq!(java_major("IMPLEMENTOR=\"x\""), None);
    }
}
