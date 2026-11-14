use std::{
    io::Read,
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Command, Stdio},
};

use crate::fresh::MAIN_FILE;
use crate::store::{cache_dir, find_root};

/// SessionStart の入力は小さい JSON。壊れた巨大入力でメモリを使わないための上限。
const MAX_INPUT_BYTES: u64 = 1 << 20;

fn cwd_of(input: &str) -> Option<PathBuf> {
    let v: serde_json::Value = serde_json::from_str(input).ok()?;
    v.get("cwd")?.as_str().map(PathBuf::from)
}

/// 索引が無いときだけ `update` を切り離して起動する。hook は止めず何も出力しない:
/// SessionStart の標準出力は Claude の文脈に入る。
pub fn session_start(stdin: impl Read) {
    let mut input = String::new();
    if stdin.take(MAX_INPUT_BYTES).read_to_string(&mut input).is_err() {
        return;
    }
    let Some(root) = cwd_of(&input).and_then(|c| find_root(&c)) else {
        return;
    };
    if cache_dir(&root).join(MAIN_FILE).exists() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    // 子は待たない。親が先に終わっても子は process_group(0) で端末のシグナルから離れて続く。
    let _ = Command::new(exe)
        .arg("update")
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_cwd_string() {
        assert_eq!(
            cwd_of(r#"{"cwd":"/a/b","x":1}"#),
            Some(PathBuf::from("/a/b"))
        );
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(cwd_of(""), None);
        assert_eq!(cwd_of("{"), None);
        assert_eq!(cwd_of("{}"), None);
        assert_eq!(cwd_of(r#"{"cwd":1}"#), None);
        assert_eq!(cwd_of(r#"["cwd"]"#), None);
    }
}
