# ferrindex

## 概要

ferrindexはgitリポジトリの定義とファイルを一瞬で探すCLIです。
LLMエージェントが`grep`でリポジトリ全体を読み進める代わりに使います。
定義の名前から場所を引けるので、読み込むトークン量を減らせます。

## 導入

Rust 1.100が必要です。リポジトリのルートで次を実行します。

```sh
cargo install --path .
```

対象はmacOSだけです。常駐プロセスはありません。

## 使い方

`outline`はファイルを直接読むので、gitリポジトリの外でも使えます。
それ以外のサブコマンドはgitリポジトリの中で実行します。
検索のたびに索引を最新にするので、事前の準備は要りません。

- `ferrindex update [--rebuild] [--stats]`索引を更新します。`--rebuild`は全件を作り直します。
- `ferrindex find <name> [--limit N]`名前で定義を探します。各行の末尾にタブで区切って、その定義の行（前後の空白を除き160字まで）を付けます。
- `ferrindex files <substr> [--limit N]`パスの一部でファイルを探します。
- `ferrindex outline <file>`ファイル内の定義を一覧します。
- `ferrindex show <name> [--limit N]`定義の本体を表示します。
- `ferrindex map [<dir>] [--limit N]`ファイルごとの概要を一覧します。
- `ferrindex refs <name> [--limit N] [--precise]`名前を参照しているコードの位置を一覧します。コメントと文字列の中は数えません。`refs`は名前を含むファイルを検索のたびに読んで解析します。OSのファイルキャッシュが冷えた最初の実行は遅く、大きなリポジトリでは数十秒かかることがあります。
- `ferrindex callers <name> [--limit N] [--precise]`名前を呼び出している位置を一覧します。Rubyで括弧を付けずに書いた呼び出し（`helper`だけの行）は呼び出しと数えず、`refs`に`ref`として出ます。
- `ferrindex impls <name> [--limit N] [--precise]`名前の型を継承、実装している位置を一覧します。
- `ferrindex supers <name> [--limit N] [--precise]`名前の型が継承、実装している型を一覧します。
- `ferrindex trust`と`ferrindex untrust`は、カレントディレクトリのリポジトリを`--precise`に使ってよいかを切り替えます。

参照の4コマンドは各行に参照の種類（`call`、`inherit`、`ref`）と、参照を囲む最も内側の定義を`in <定義>`の形で付けます。

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

`map`の`~N`列はファイルのtoken数の見積もりです。
コードは文字数を3.5で、それ以外は4.0で割ります。

終了コードは次のとおりです。

- 0結果があります。
- 1結果がありません。
- 2使い方が誤っています。
- 3索引が使えません。
- 4その他のI/Oエラーです。

## --precise

参照の4コマンドは既定では名前の一致で答えます。
同じ名前の別の定義への参照も混ざりますが、索引から即座に返せます。
`--precise`を付けると、ferrindexは定義の言語の言語サーバーをその場で起動して問い合わせ、答えを得たらサーバーを止めます。
サーバーは常駐しません。

`--precise`が起動する言語サーバーは、リポジトリの中のコードを実行することがあります。
rust-analyzerはCargoのbuild scriptとproc macroを動かします。
typescript-language-serverは、リポジトリに入っているtsserverを使います。
jdtlsはMavenを、csharp-lsはMSBuildを、sourcekit-lspは`Package.swift`を実行します。
`--precise`は信頼できるリポジトリでだけ使ってください。

ferrindexは、利用者が信頼したリポジトリでだけ言語サーバーを起動します。
既定ではどのリポジトリも信頼していません。
信頼していないリポジトリで`--precise`を付けると、サーバーを起動せずに名前一致の結果を返し、標準エラーに理由を出します。
リポジトリの中で`ferrindex trust`を端末から実行すると、そのリポジトリを信頼します。`ferrindex untrust`で取り消します。
`trust`は標準入力が端末のときだけ受け付け、端末でなければ終了コード2で終わります。
Claude CodeのBashツールは端末を持たないので、Claudeが自分でリポジトリを信頼することはできません。
gitリポジトリの外では、どちらも終了コード3で終わります。
信頼の一覧は`$XDG_DATA_HOME/ferrindex/trusted`に置きます。未設定なら`~/.local/share/ferrindex/trusted`です。
一覧には、根のパスそのものでなく、そのSHA-256だけを書きます。

多くのサーバーはプロジェクトを読み込み終える前にも、そうと知らせずに少ない答えを返します。
そのためferrindexは最初の答えを得た後も、同じ問い合わせを約1秒おきに送り直します。
続けて2回、同じ位置の集合が返ったときに、その答えを結果として採ります。

同じ名前の定義が複数あるときは、定義ごとに問い合わせて結果を合わせます。
定義が5件を超える名前では問い合わせずに名前一致の結果を返します。
待ちの上限はすべての問い合わせを合わせて30秒です。

言語サーバーで確かめられないとき、ferrindexは名前一致の結果を返し、標準エラーに理由を出します。
理由の行はどれも`ferrindex: --precise: <理由>。名前一致の結果です（未確定）`の形です。サーバーが失敗したときの`<理由>`は`<サーバー名>: <エラー>`で、終了コードは名前一致と同じです。名前一致も0件のときも理由の行を出し、終了コード1で終わります。
Goの`impls`と`supers`は名前一致で答えられないので、理由の行は`ferrindex: --precise: <理由>`だけで、終了コード1で終わります。
次の場合が該当します。

- リポジトリを信頼していません。
- 名前と完全に一致する定義が索引に無いか、5件を超えています。`--precise`は部分一致の定義を使わず、Markdownの見出しも問い合わせません。
- `impls`と`supers`で、名前一致の継承の行の中に、サーバーの答えに無いものがあります。`impls`は各行の実装している型を、`supers`は基底の型の名前を答えの型と比べます。`impls`の答えからは、問い合わせた型そのものを除きます。
- サーバーが見つからないか、起動に失敗しました。
- 30秒以内に答えが無いか、答えが定まりませんでした。
- サーバーの答えが0件でした。読み込みの途中でも0件と答えるサーバーがあるので、0件は確かな答えとして扱いません。
- `refs`と`callers`で、名前一致で見つかった呼び出しの中に、サーバーの答えに無いものがあります。修飾の無い名前ではすべての定義に問い合わせて答えを合わせるので、どの呼び出しもその中に入るはずです。`require`の分割代入、`local f = require(...).f`のような別名、型を推論できない受け手を経た呼び出しは、追えないサーバーがあります。プロジェクトの設定が試験のファイルを除いている場合も、そのファイルの呼び出しは答えに入りません。索引の外にある同名の関数（ライブラリなど）への呼び出しでも名前一致に戻ります。Pythonの`from m import f as _f`のように別の名前で取り込んだ関数の呼び出し`_f()`は、pyrightのcallersも返さず、名前一致も`f`の呼び出しとは見なしません。どちらの結果にも入らない呼び出しがあることに注意してください。`Class.method`や`A::B`のように修飾した名前でも、修飾の無い名前のすべての定義に問い合わせて同じように確かめます。確かめが通れば、修飾した名前の定義の答えだけを返します。修飾の無い名前の定義が5件を超えるときは問い合わせず、名前一致に戻ります。
- C、C++のリポジトリの根に`compile_commands.json`がありません。clangdはこのファイルが無いと開いたファイルしか見ないので、ferrindexはclangdを起動しません。
- サーバーが問い合わせの種類に対応していません。solargraphとlua-language-serverは呼び出しの階層（callHierarchy）に対応しないので、`callers --precise`はいつも名前一致に戻ります。

`refs --precise`は、サーバーが返した定義そのものの位置を答えから除きます。
呼び出しと継承のどちらでもない参照は、名前一致との突き合わせをしません。
`refs`が返す型の名前の参照などがこれに当たります。
サーバーのプロジェクトの外にあるファイルの参照は、答えに入りません。
tsconfigの`include`から外れた試験のファイルなどが該当します。

rust-analyzerは初回の起動で、build scriptとproc macroを作ります。
大きなリポジトリでは30秒を越えて名前一致に戻ることがあります。
作ったものは索引のキャッシュの下に残るので、2回目からは速く答えます。

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
| C、C++ | clangd | リポジトリの根の`compile_commands.json` |
| Swift | sourcekit-lsp | |
| Lua | lua-language-server | |
| Dart | dart language-server | |

サーバーはPATHと`~/.dotnet/tools`から探します。

### 作業ツリーに作られるもの

サーバーが作業領域やビルドの結果を作業ツリーに書かないよう、ferrindexは次の設定で起動します。

- jdtlsの作業領域（`-data`）は索引のキャッシュの下に置きます。`.project`などのメタデータもそこに書かせ、自動ビルドは止めます。
- rust-analyzerが呼ぶcargoの`target/`は、`CARGO_TARGET_DIR`で索引のキャッシュの下に向けます。
- sourcekit-lspが使うSwiftPMの`.build/`は、`--scratch-path`で索引のキャッシュの下に向けます。

それでもサーバーが作業ツリーに作るものがあります。

| サーバー | 作られるもの |
|---|---|
| csharp-ls | 各プロジェクトの`obj/`。MSBuildが依存の復元とプロジェクトの評価に使います。 |
| rust-analyzer | `Cargo.lock`。無いCargoのプロジェクトで、依存を解決したcargoが作ります。 |
| clangd | `compile_commands.json`の隣の`.cache/clangd/index/`。背景の索引と、その中の`.gitignore`です。 |

### サーバーのプロセス

ferrindexは問い合わせのたびにサーバーのプロセスグループを止め、サーバーが起動した子プロセスも残しません。
Ctrl-Cや`kill`でferrindexを途中で止めた場合も、同じように止まります。
そのために、ferrindexはサーバーごとに`sh`のプロセスを1つ起動します。
このプロセスはferrindexとの間のパイプが閉じるのを待ち、閉じたらサーバーのプロセスグループを止めます。

## 鮮度

検索のたびに全ファイルの更新時刻を走査し、変わったファイルだけを解析し直します。
編集したのが人でも他のツールでも結果は常に現在のファイルを反映します。

## 対応言語

Rust、Python、TypeScript（TSXを含む）、JavaScript、Go、Ruby、Java、C、C++、C#、Swift、Dart、Lua、Markdownに対応します。

### 抽出できない定義

- Dartのabstractメソッド
- C++ のクラス外定義`A<T>::f`と`A::B::f`

`map`の概要は先頭20行にある最初のコメント、見出し、Pythonのdocstringからだけ取ります。

## 索引の保存

索引は`$XDG_CACHE_HOME/ferrindex`に置きます。未設定なら`~/.cache/ferrindex`です。
リポジトリの作業ツリーにはファイルを作りません。

名前とパスの文字列はFSTの中にだけ入ります。
索引は圧縮されているだけで、暗号化されていません。
ファイルを読める人は名前とパスを取り出せます。

## 索引が使えないとき

ferrindexが終了コード3を返したら、索引が使えません。
gitの外で実行した場合や、索引が壊れていて作り直せない場合です。
そのときは`rg -n`で探します。

リポジトリの中に読めないディレクトリやファイルがあると、ferrindexはそのパスを示して終了コード4で止まります。
結果を欠かしたまま返さないためです。
dockerのデータディレクトリのように索引に要らないものは、`.gitignore`に足すと走査から外れます。

## Claude Codeのhook

セッションを始めたときに索引を作っておくには、`~/.claude/settings.json`に次を足します。

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

hookは標準入力のJSONから`cwd`を読みます。
cwdがgitリポジトリの中にあるときは、ferrindexの使い方を短い案内として標準出力に出します。
案内は、`find`が各定義の行を表示するので確認のgrepやReadは要らないこと、本体は`show`で直接引くことを伝えます。
呼び出し元は`callers`、使用箇所は`refs`で探し、その出力を確かめ直さずに使うことも伝えます。
Claude CodeはSessionStartの標準出力をClaudeの文脈に加えるので、ClaudeはGrepやReadより先にferrindexを使えます。
索引がまだ無いときは、`ferrindex update`を切り離して起動し、最初の検索が待つことがある旨を案内に足します。
gitリポジトリの外や入力の誤りでは何も出力しません。いずれの場合も終了コードは0です。

`ferrindex hook post-tool-use`は、Bashで`ferrindex`の`find`、`show`、`callers`、`refs`を実行した直後にだけ働きます。
その出力で問いに答えられるなら、Readやrgで確かめ直さずにそのまま答えるよう、Claudeに短く添えます。
確かめ直すとやり取りが1回増え、そのたびにClaude Codeは会話の文脈を読み直すので、入力のtokenが増えます。
ほかのコマンドと入力の誤りでは何も出力しません。終了コードは常に0です。

## 試験

`cargo test`で試験を実行します。
`--precise`の試験は本物の言語サーバーの代わりに`tests/fixtures/fake_lsp.py`を使います。
環境変数`FERRINDEX_LSP_<言語>`（たとえば`FERRINDEX_LSP_RUST`）に起動コマンドを空白区切りで渡すと、ferrindexはその言語のサーバーとしてそのコマンドを起動します。
`<言語>`は`RUST`、`GO`、`PYTHON`、`TYPESCRIPT`、`TSX`、`JAVASCRIPT`、`JAVA`、`CSHARP`、`RUBY`、`C`、`CPP`、`SWIFT`、`DART`、`LUA`のどれかです。

## 撤去

1. `settings.json`から`ferrindex hook session-start`と`ferrindex hook post-tool-use`の項目を消します。
2. `cargo uninstall ferrindex`を実行します。
3. 索引のキャッシュディレクトリ`$XDG_CACHE_HOME/ferrindex`（未設定なら`~/.cache/ferrindex`）を削除します。
4. 信頼の一覧のディレクトリ`$XDG_DATA_HOME/ferrindex`（未設定なら`~/.local/share/ferrindex`）を削除します。

## ライセンス

MITライセンスです。全文は`LICENSE`を参照してください。
