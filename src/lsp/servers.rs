use std::env;
use std::ffi::OsString;
use std::fs;
use std::iter;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

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

/// 利用者に見せるサーバーの名前。
pub fn server_name(lang: Lang) -> &'static str {
    command_of(lang).map_or("Markdown", |c| c.0)
}

/// `initialize` の `initializationOptions`。jdtls は既定で自動ビルドの結果（target/）を
/// 作業ツリーに書くので止める。参照の検索は索引で答えるのでビルドは要らない。
pub(super) fn init_options(lang: Lang) -> Value {
    match lang {
        Lang::Java => json!({"settings": {"java": {"autobuild": {"enabled": false}}}}),
        _ => Value::Null,
    }
}

pub(super) fn language_id(lang: Lang) -> &'static str {
    row(lang).map_or("markdown", |r| r.1)
}

pub fn server_for(lang: Lang) -> Result<ServerSpec, LspError> {
    spec_in(lang, &search_dirs(), java_homes(), dotnet_roots())
}

fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = env::var_os("PATH")
        .map(|p| env::split_paths(&p).collect())
        .unwrap_or_default();
    // `dotnet tool install -g` は PATH に載らないことが多い。
    dirs.extend(env::home_dir().map(|h| h.join(".dotnet/tools")));
    dirs
}

fn java_homes() -> impl Iterator<Item = PathBuf> {
    env::var_os("FERRINDEX_JAVA_HOME")
        .map(PathBuf::from)
        .into_iter()
        .chain([PathBuf::from(BREW_JDK)])
        .chain(iter::once_with(|| java_home_from(Path::new(JAVA_HOME_TOOL))).flatten())
}

fn dotnet_roots() -> impl Iterator<Item = PathBuf> {
    env::var_os("DOTNET_ROOT")
        .map(PathBuf::from)
        .into_iter()
        .chain(DOTNET_ROOTS.map(PathBuf::from))
}

/// `ferrindex servers` の 1 行分。`missing` は揃っていない前提。
pub struct Setup {
    pub installed: bool,
    pub missing: Option<String>,
}

/// 言語サーバーが入っているかと、起動に要る前提（JDK、.NET SDK、compile_commands.json）が揃っているか。
pub fn setup(lang: Lang, root: &Path) -> Setup {
    setup_in(lang, root, &search_dirs(), java_homes(), dotnet_roots())
}

fn setup_in(
    lang: Lang,
    root: &Path,
    dirs: &[PathBuf],
    java_homes: impl IntoIterator<Item = PathBuf>,
    dotnet_roots: impl IntoIterator<Item = PathBuf>,
) -> Setup {
    let installed =
        command_of(lang).is_some_and(|(name, _)| dirs.iter().any(|d| d.join(name).is_file()));
    let missing = match lang {
        Lang::Java => pick_java_home(java_homes)
            .is_none()
            .then(|| format!("JDK {MIN_JDK}以上")),
        Lang::CSharp => (!dotnet_roots.into_iter().any(|p| p.is_dir()))
            .then(|| ".NET SDK（DOTNET_ROOT）".to_string()),
        Lang::C | Lang::Cpp => (!root.join("compile_commands.json").is_file())
            .then(|| "compile_commands.json".to_string()),
        _ => None,
    };
    Setup { installed, missing }
}

/// macOS で言語サーバーを入れるコマンド。typescript-language-server は tsserver を同梱しない
/// TypeScript 7 では動かないので、TypeScript は 6 系を入れる。
pub fn install_command(lang: Lang) -> &'static str {
    match lang {
        Lang::Rust => "rustup component add rust-analyzer",
        Lang::Go => "go install golang.org/x/tools/gopls@latest",
        Lang::Python => "npm install -g pyright",
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => {
            "npm install -g typescript-language-server typescript@6"
        }
        Lang::Java => "brew install jdtls openjdk",
        Lang::CSharp => "brew install dotnet && dotnet tool install -g csharp-ls",
        Lang::Ruby => "gem install solargraph",
        Lang::C | Lang::Cpp => "brew install llvm",
        Lang::Swift => "xcode-select --install",
        Lang::Lua => "brew install lua-language-server",
        Lang::Dart => "brew install dart-lang/dart/dart",
        Lang::Markdown => "",
    }
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

    #[test]
    fn setup_reports_the_server_and_each_prerequisite() {
        let t = TempDir::new().unwrap();
        let bin = t.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("jdtls"), "").unwrap();
        let dirs = [t.path().join("none"), bin];
        let none = || Vec::<PathBuf>::new();
        let java = setup_in(Lang::Java, t.path(), &dirs, none(), none());
        assert!(java.installed);
        assert_eq!(java.missing.as_deref(), Some("JDK 25以上"));
        let jdk = t.path().join("jdk");
        fs::create_dir_all(&jdk).unwrap();
        fs::write(jdk.join("release"), "JAVA_VERSION=\"25.0.1\"\n").unwrap();
        assert_eq!(
            setup_in(Lang::Java, t.path(), &dirs, [jdk], none()).missing,
            None
        );
        let cs = setup_in(Lang::CSharp, t.path(), &dirs, none(), [t.path().join("no")]);
        assert!(!cs.installed);
        assert_eq!(cs.missing.as_deref(), Some(".NET SDK（DOTNET_ROOT）"));
        let cs = setup_in(
            Lang::CSharp,
            t.path(),
            &dirs,
            none(),
            [t.path().to_path_buf()],
        );
        assert_eq!(cs.missing, None);
        let cpp = setup_in(Lang::Cpp, t.path(), &dirs, none(), none());
        assert_eq!(cpp.missing.as_deref(), Some("compile_commands.json"));
        fs::write(t.path().join("compile_commands.json"), "[]").unwrap();
        assert_eq!(
            setup_in(Lang::Cpp, t.path(), &dirs, none(), none()).missing,
            None
        );
        // ディレクトリはサーバーの実行ファイルに数えない。
        fs::create_dir_all(dirs[1].join("gopls")).unwrap();
        let go = setup_in(Lang::Go, t.path(), &dirs, none(), none());
        assert!(!go.installed && go.missing.is_none());
        assert!(!setup_in(Lang::Markdown, t.path(), &dirs, none(), none()).installed);
    }

    #[test]
    fn every_language_with_a_server_has_an_install_command() {
        for lang in Lang::ALL {
            assert_eq!(
                install_command(lang).is_empty(),
                lang == Lang::Markdown,
                "{lang:?}"
            );
        }
        assert!(install_command(Lang::Tsx).contains("typescript@6"));
    }

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn jdtls_does_not_build_into_the_tree() {
        let java = init_options(Lang::Java);
        assert_eq!(java["settings"]["java"]["autobuild"]["enabled"], false);
        assert_eq!(init_options(Lang::Go), serde_json::Value::Null);
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
