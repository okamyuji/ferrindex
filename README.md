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
- `ferrindex refs <name> [--limit N] [--precise]` 名前を参照しているコードの位置を一覧します。コメントと文字列の中は数えません。
- `ferrindex callers <name> [--limit N] [--precise]` 名前を呼び出している位置を一覧します。
- `ferrindex impls <name> [--limit N] [--precise]` 名前の型を継承、実装している位置を一覧します。
- `ferrindex supers <name> [--limit N] [--precise]` 名前の型が継承、実装している型を一覧します。

参照の4コマンドは、各行に参照の種類（`call`、`inherit`、`ref`）と、参照を囲む最も内側の定義を`in <定義>`の形で付けます。

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

## --precise

参照の4コマンドは、既定では名前の一致で答えます。
同じ名前の別の定義への参照も混ざりますが、索引から即座に返せます。
`--precise`を付けると、ferrindexは定義の言語の言語サーバーをその場で起動して問い合わせ、答えを得たらサーバーを止めます。
サーバーは常駐しません。

同じ名前の定義が複数あるときは、定義ごとに問い合わせて結果を合わせます。
定義が5件を超える名前では、問い合わせずに名前一致の結果を返します。
待ちの上限は、すべての問い合わせを合わせて30秒です。

問い合わせられないとき、ferrindexは名前一致の結果を返し、標準エラーに理由を出します。
該当するのは、定義が索引に無い場合、定義が多すぎる場合、サーバーが見つからない場合、起動に失敗した場合、30秒以内に答えが無い場合です。
理由の行は`ferrindex: <サーバー名>: <理由>。名前一致の結果です（未確定）`の形になり、終了コードは名前一致と同じです。

```text
$ ferrindex callers walk_builder --precise --limit 2
crates/core/main.rs:111:10 call in search	.walk_builder()?
crates/core/main.rs:175:10 call in search_parallel	args.walk_builder()?.build_parallel().run(|| {
… 2 more
```

ferrindexが起動するサーバーは次のとおりです。
サーバーはferrindexに同梱していないので、使う言語のものを別に入れておきます。

| 言語 | サーバー | 起動の条件 |
|---|---|---|
| Rust | rust-analyzer | |
| Go | gopls | |
| Python | pyright-langserver | |
| TypeScript、JavaScript | typescript-language-server | |
| Java | jdtls | JDK 25以上。`FERRINDEX_JAVA_HOME`、Homebrewのopenjdk、`/usr/libexec/java_home`の順に探します。 |
| C# | csharp-ls | .NET。`DOTNET_ROOT`、Homebrewのdotnet、`/usr/local/share/dotnet`の順に探します。リポジトリの`global.json`が求めるSDKも要ります。 |
| Ruby | solargraph | |
| C、C++ | clangd | |
| Swift | sourcekit-lsp | |
| Lua | lua-language-server | |
| Dart | dart language-server | |

サーバーはPATHと`~/.dotnet/tools`から探します。

### 作業ツリーに作られるもの

ferrindexは、サーバーが作業領域やビルドの結果を作業ツリーに書かないよう、次の設定で起動します。

- jdtlsの作業領域（`-data`）は索引のキャッシュの下に置きます。`.project`などのメタデータもそこに書かせ、自動ビルドは止めます。
- rust-analyzerが呼ぶcargoの`target/`は、`CARGO_TARGET_DIR`で索引のキャッシュの下に向けます。

それでもサーバーが作業ツリーに作るものがあります。

| サーバー | 作られるもの |
|---|---|
| csharp-ls | 各プロジェクトの`obj/`。MSBuildが依存の復元とプロジェクトの評価に使います。 |

ほかのサーバーは、上の設定で作業ツリーにファイルを作りません。

### サーバーのプロセス

ferrindexは問い合わせのたびにサーバーのプロセスグループを止め、サーバーが起動した子プロセスも残しません。
Ctrl-Cや`kill`でferrindexを途中で止めた場合も、同じように止まります。
そのために、ferrindexはサーバーごとに`sh`のプロセスを1つ起動します。
このプロセスはferrindexとの間のパイプが閉じるのを待ち、閉じたらサーバーのプロセスグループを止めます。

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

## 試験

`cargo test`で試験を実行します。
`--precise`の試験は、本物の言語サーバーの代わりに`tests/fixtures/fake_lsp.py`を使います。
環境変数`FERRINDEX_LSP_<言語>`（たとえば`FERRINDEX_LSP_RUST`）に起動コマンドを空白区切りで渡すと、ferrindexはその言語のサーバーとしてそのコマンドを起動します。
`<言語>`は`RUST`、`GO`、`PYTHON`、`TYPESCRIPT`、`TSX`、`JAVASCRIPT`、`JAVA`、`CSHARP`、`RUBY`、`C`、`CPP`、`SWIFT`、`DART`、`LUA`のどれかです。

## 撤去

1. `settings.json` から `ferrindex hook session-start` と `ferrindex hook post-tool-use` の項目を消します。
2. `cargo uninstall ferrindex` を実行します。
3. 索引のキャッシュディレクトリ `$XDG_CACHE_HOME/ferrindex`（未設定なら `~/.cache/ferrindex`）を削除します。

## ライセンス

MIT ライセンスです。全文は `LICENSE` を参照してください。
