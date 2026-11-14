# ferrindex

## 概要

ferrindex は git リポジトリの定義とファイルを一瞬で探す CLI です。
LLM エージェントが `grep` でリポジトリ全体を読み進める代わりに使います。
定義の名前から場所を引けるので、読み込むトークン量を減らせます。

## 導入

Rust 1.100 が必要です。リポジトリのルートで次を実行します。

```sh
cargo install --path .
```

対象は macOS だけです。常駐プロセスはありません。

## 使い方

`outline` はファイルを直接読むので、git リポジトリの外でも使えます。
それ以外のサブコマンドは git リポジトリの中で実行します。
検索のたびに索引を最新にするので、事前の準備は要りません。

- `ferrindex update [--rebuild] [--stats]` 索引を更新します。`--rebuild` は全件を作り直します。
- `ferrindex find <name> [--limit N]` 名前で定義を探します。各行の末尾にタブで区切って、その定義の行（前後の空白を除き 160 字まで）を付けます。
- `ferrindex files <substr> [--limit N]` パスの一部でファイルを探します。
- `ferrindex outline <file>` ファイル内の定義を一覧します。
- `ferrindex show <name> [--limit N]` 定義の本体を表示します。
- `ferrindex map [<dir>] [--limit N]` ファイルごとの概要を一覧します。

出力例です。

```text
$ ferrindex find main
build.rs:110:4 function main	fn main() {
$ ferrindex files cli
difftest/scripts/cli_table.py
$ ferrindex outline src/main.rs
src/main.rs:1:4 function main
$ ferrindex show main
build.rs:110-186 function main
fn main() {
$ ferrindex map src
src/cli/alias.rs  ~2806  CLI aliases (`awscli/alias.py`): the `[toplevel]` aliases of the main
```

`map` の `~N` 列は、ファイルの token 数の見積もりです。
コードは文字数を 3.5 で、それ以外は 4.0 で割ります。

終了コードは次のとおりです。

- 0 結果があります。
- 1 結果がありません。
- 2 使い方が誤っています。
- 3 索引が使えません。
- 4 その他の I/O エラーです。

## 鮮度

検索のたびに全ファイルの更新時刻を走査し、変わったファイルだけを解析し直します。
編集したのが人でも他のツールでも、結果は常に現在のファイルを反映します。

## 対応言語

Rust、Python、TypeScript（TSX を含む）、JavaScript、Go、Ruby、Java、C、C++、C#、Swift、Dart、Lua、Markdown に対応します。

### 抽出できない定義

- Dart の abstract メソッド
- C++ のクラス外定義 `A<T>::f` と `A::B::f`

`map` の概要は、先頭 20 行にある最初のコメント、見出し、Python の docstring からだけ取ります。

## 索引の保存

索引は `$XDG_CACHE_HOME/ferrindex` に置きます。未設定なら `~/.cache/ferrindex` です。
リポジトリの作業ツリーにはファイルを作りません。

名前とパスの文字列は FST の中にだけ入ります。
索引は圧縮されているだけで、暗号化されていません。
ファイルを読める人は、名前とパスを取り出せます。

## 索引が使えないとき

ferrindex が終了コード 3 を返したら、索引が使えません。
git の外で実行した場合や、索引が壊れていて作り直せない場合です。
そのときは `rg -n` で探します。

リポジトリの中に読めないディレクトリやファイルがあると、ferrindexはそのパスを示して終了コード4で止まります。
結果を欠かしたまま返さないためです。
dockerのデータディレクトリのように索引に要らないものは、`.gitignore`に足すと走査から外れます。

## Claude Code の hook

セッションを始めたときに索引を作っておくには、`~/.claude/settings.json` に次を足します。

```json
{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          { "type": "command", "command": "ferrindex hook session-start" }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          { "type": "command", "command": "ferrindex hook post-tool-use" }
        ]
      }
    ]
  }
}
```

hook は標準入力の JSON から `cwd` を読みます。
cwd が git リポジトリの中にあるときは、ferrindex の使い方を短い案内として標準出力に出します。
案内は、`find` が各定義の行を表示するので確認の grep や Read は要らないこと、本体は `show` で直接引くこと、呼び出し元の検索は `rg -n` を使うことを伝えます。
Claude Code は SessionStart の標準出力を Claude の文脈に加えるので、Claude は Grep や Read より先に ferrindex を使えます。
索引がまだ無いときは、`ferrindex update` を切り離して起動し、最初の検索が待つことがある旨を案内に足します。
git リポジトリの外や入力の誤りでは何も出力しません。いずれの場合も終了コードは 0 です。

`ferrindex hook post-tool-use` は、Bash で `ferrindex find` か `ferrindex show` を実行した直後にだけ働きます。
その出力で問いに答えられるなら、Read や rg で確かめ直さずにそのまま答えるよう、Claude に短く添えます。
確かめ直すとやり取りが 1 回増え、そのたびに Claude Code は会話の文脈を読み直すので、入力の token が増えます。
ほかのコマンドと入力の誤りでは何も出力しません。終了コードは常に 0 です。

## 撤去

1. `settings.json` から `ferrindex hook session-start` と `ferrindex hook post-tool-use` の項目を消します。
2. `cargo uninstall ferrindex` を実行します。
3. 索引のキャッシュディレクトリ `$XDG_CACHE_HOME/ferrindex`（未設定なら `~/.cache/ferrindex`）を削除します。

## ライセンス

MIT ライセンスです。全文は `LICENSE` を参照してください。
