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

fn read_input(stdin: impl Read) -> Option<String> {
    let mut input = String::new();
    stdin
        .take(MAX_INPUT_BYTES)
        .read_to_string(&mut input)
        .ok()?;
    Some(input)
}

fn cwd_of(input: &str) -> Option<PathBuf> {
    let v: serde_json::Value = serde_json::from_str(input).ok()?;
    v.get("cwd")?.as_str().map(PathBuf::from)
}

const GUIDANCE: &str = "このリポジトリでは ferrindex の索引を使えます。定義の場所は `ferrindex find <名前>`（`Class.method` も可）で引いてください。各行に定義の行が付くので、確認のための grep や Read は要りません。
本体が要るときは find と Read ではなく `ferrindex show <名前>` を直接使ってください。ファイルの定義一覧は `ferrindex outline <file>`、ファイル名は `ferrindex files <部分文字列>` で引けます。
呼び出し元と使用箇所は `rg -n` で探してください。終了コード 3 のときも `rg -n` を使ってください。
";

const BUILDING: &str = "索引を背景で作成中です。最初の検索は待つことがあります。\n";

/// git ルートの中なら標準出力に出す案内を返す。それ以外と失敗では空文字列を返す。
/// SessionStart の標準出力は Claude の文脈に入る。
/// 索引が無いときは `update` を切り離して起動する。hook は止めない。
pub fn session_start(stdin: impl Read) -> String {
    let Some(input) = read_input(stdin) else {
        return String::new();
    };
    let Some(root) = cwd_of(&input).and_then(|c| find_root(&c)) else {
        return String::new();
    };
    if cache_dir(&root).join(MAIN_FILE).exists() {
        return GUIDANCE.to_string();
    }
    let Ok(exe) = std::env::current_exe() else {
        return String::new();
    };
    // 子は待たない。親が先に終わっても子は process_group(0) で端末のシグナルから離れて続く。
    let spawned = Command::new(exe)
        .arg("update")
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
    match spawned {
        Ok(_) => format!("{GUIDANCE}{BUILDING}"),
        Err(_) => GUIDANCE.to_string(),
    }
}

const ANSWER_NOW: &str = "上の ferrindex の出力は、いまの作業ツリーから引いた位置と定義の行（show は本体）です。問いがこの位置と定義行で答えられるなら、Read、rg、grep、sed で確かめ直さずに、そのまま回答してください。足りない情報があるときだけ、その部分に絞って追加で調べてください。";

/// コマンドのどこかで `ferrindex find` か `ferrindex show` を実行しているか。
fn runs_lookup(command: &str) -> bool {
    command.split(['&', '|', ';', '\n']).any(|seg| {
        let mut w = seg.split_whitespace();
        w.next() == Some("ferrindex") && matches!(w.next(), Some("find" | "show"))
    })
}

/// PostToolUse の hook。`ferrindex find` か `show` の直後にだけ、確かめ直さずに答えるよう添える。
/// 確認の Read や rg で 1 ターン増え、そのたびに固定の文脈を読み直すのを防ぐ。
/// 当たらない入力と失敗では空文字列を返す。
pub fn post_tool_use(stdin: impl Read) -> String {
    let Some(input) = read_input(stdin) else {
        return String::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&input) else {
        return String::new();
    };
    match v["tool_input"]["command"].as_str() {
        Some(c) if runs_lookup(c) => serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PostToolUse",
                "additionalContext": ANSWER_NOW,
            }
        })
        .to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guidance_fits_four_lines_with_building_note() {
        assert_eq!(GUIDANCE.lines().count(), 3);
        assert_eq!(format!("{GUIDANCE}{BUILDING}").lines().count(), 4);
    }

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
