# ferrindex

## 概要

ferrindexはgitリポジトリにあるソースコードでの定義とファイルを一瞬で探すCLIです。
LLMエージェントが`grep`でリポジトリ全体を読み進める代わりに使います。
定義の名前から場所を引けるので、読み込むtoken量を減らせます。

## rg との違い

ferrindexは名前の分かった定義と参照を探すツールでrgの代わりではありません。
次の値はlinuxのリポジトリ（86,502ファイル）で測ったものです。

ferrindexの優位点は次のとおりです。

- 定義だけを返します。`printk`を探すと`rg -n printk`の出力は3,388,351バイト、`ferrindex find printk`は159バイトでした。
- インデックスを引くので大きなリポジトリでも応答が速いです。linuxで`find`は約170ミリ秒でした。
- コメントと文字列の中の名前は拾いません。
- `callers`は呼び出しだけを、`impls`は継承だけを返し、各行に囲む定義の名前を付けます。
- `show`は定義の本体だけを返します。

rgより劣っている点と差異がない点は次のとおりです。

- 任意の文字列や正規表現では探せません。その用途にはrgを使います。
- 出力の量は定義を探す正規表現を書いたrgとほぼ同じです。ただし、rgでは言語ごとに正規表現が必要になります。複数行にわたる宣言やマクロで作る定義はその正規表現だと取りこぼしてしまいます。
- 名前の分かった定義の場所を尋ねる問いではClaude CodeのGrepを使ったときとtoken量がほぼ変わりません。
- 最初にインデックスを作る必要があります。linuxでは約47秒かかりました。2回目からは更新されたファイルだけを読み直します。
- `refs`はOSのファイルキャッシュに載っていない最初の実行では遅くなります。
- `--precise`は言語サーバーを起動するので数秒から最大30秒ほどかかる場合があります。

## インストール

Rust 1.100が必要です。リポジトリのルートで次を実行します。

```sh
cargo install --path .
```

対応OSはmacOSだけです。なお、常駐プロセスはありません。

## はじめの手順

試すリポジトリには[対応言語](#対応言語)のファイルが多くてコミットが続いているものが向いています。
定義と参照が多いほど、rgとの出力の差が見えやすくなるからです。
次の手順はそのリポジトリで一通りのサブコマンドを試す流れです。

1. 試すリポジトリのルートへ移ります。

   ```sh
   cd <リポジトリ>
   ```

2. インデックスを作ります。`--stats`を付けるとインデックスのサイズも出ます。検索のたびにインデックスは最新になるのでこの手順は省いてもかまいません。

   ```sh
   ferrindex update --stats
   ```

3. ディレクトリの下にあるファイルの概要を一覧し、リポジトリの構成をつかみます。

   ```sh
   ferrindex map <ディレクトリ>
   ```

4. 名前で定義を探し、その本体を読みます。

   ```sh
   ferrindex find <名前>
   ferrindex show <名前>
   ```

5. 呼び出し、継承、参照の位置を探します。各行にはその位置を囲む定義の名前が付きます。

   ```sh
   ferrindex callers <名前>
   ferrindex impls <型の名前>
   ferrindex refs <名前>
   ```

6. 必要なら言語サーバーで結果を確かめます。`servers`でインストール状況を見て、無ければ`install`列のコマンドでインストールします。続けて、利用者が端末で`trust`を一度だけ実行すると、`--precise`を使えます。

   ```sh
   ferrindex servers
   ferrindex trust
   ferrindex callers <名前> --precise
   ```

各サブコマンドの動作は[使い方](#使い方)に、`--precise`の条件は[--precise](#--precise)にまとめています。

## 使い方

`outline`以外のサブコマンドはgitリポジトリの中で実行します。
`outline`はファイルを直接読むので、gitリポジトリの外でも使えます。
検索のたびにインデックスを最新にするので事前準備は不要です。

| コマンド | 動作 | オプション |
|---|---|---|
| `update` | インデックスを更新します。 | `--rebuild` 全件を作り直す<br>`--stats` インデックスのサイズを出す |
| `find <name>` | 名前で定義を探します。各行の末尾に、タブ区切りで定義の行（前後の空白を除き160字まで）を付けます。 | `--limit N` |
| `show <name>` | 定義の本体を表示します。 | `--limit N` |
| `files <substr>` | パスの一部でファイルを探します。 | `--limit N` |
| `outline <file>` | ファイル内の定義を一覧します。 | |
| `map [<dir>]` | ファイルごとの概要とtoken数の見積もり（`~N`列）を一覧します。 | `--limit N` |
| `refs <name>` | 名前を参照している位置を一覧します。コメントと文字列の中は数えません。 | `--limit N`<br>`--precise` |
| `callers <name>` | 名前を呼び出している位置を一覧します。 | `--limit N`<br>`--precise` |
| `impls <name>` | その型を継承または実装している位置を一覧します。 | `--limit N`<br>`--precise` |
| `supers <name>` | その型が継承または実装している型を一覧します。 | `--limit N`<br>`--precise` |
| `servers` | `--precise`が使う言語サーバーのインストール状況を言語ごとに出します。 | |
| `trust` / `untrust` | カレントディレクトリのリポジトリで`--precise`を許可または取り消します。 | |

`refs`、`callers`、`impls`、`supers`の各行には参照の種類（`call`、`inherit`、`ref`）と参照を囲む最も内側の定義（`in <定義>`）が付きます。

次の挙動に注意してください。

- `refs`は名前を含むファイルを検索のたびに解析します。OSのファイルキャッシュに載っていない最初の実行では大きなリポジトリで数十秒かかることがあります。
- `callers`はRubyで括弧を付けずに書いた呼び出し（`helper`だけの行）を呼び出しと数えません。この行は`refs`に`ref`として出ます。
- `callers`はRustのマクロの引数に書いた名前を直後に`(`、`::<…>(`、`!`と括弧のどれかが続くときだけ呼び出しと数えます。
- `map`のtoken数はコードなら文字数を3.5で、それ以外なら4.0で割った値です。

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

### 終了コード

| コード | 意味 |
|---|---|
| 0 | 結果があります。 |
| 1 | 結果がありません。 |
| 2 | 使い方が誤っています。 |
| 3 | インデックスが使えません（[インデックスが使えないとき](#インデックスが使えないとき)）。 |
| 4 | その他のI/Oエラーです。 |

## --precise

`refs`、`callers`、`impls`、`supers`は既定で名前の一致を使うので、同じ名前の別の定義への参照も混ざります。
`Foo.bar`と修飾しても結果は`bar`の呼び出しすべてです。
同じ名前の定義が複数あるときと修飾した名前を渡したときは、標準エラー出力に`--precise`を勧める注意が出ます。
信頼していないリポジトリでこの注意が勧めるのは、端末で`ferrindex trust`を実行してから使う手順です。

`--precise`を付けるとferrindexは言語サーバーを起動して結果を絞り込み、終わればサーバーを止めます。
`Class.method`や`A::B`と修飾した名前では修飾の無い名前のすべての定義に問い合わせて確かめ、確認ができたときに返すのは修飾した定義の答えだけになります。

```text
$ ferrindex callers walk_builder --precise --limit 2
crates/core/main.rs:111:10 call in search	.walk_builder()?
crates/core/main.rs:175:10 call in search_parallel	args.walk_builder()?.build_parallel().run(|| {
… 2 more
```

### 信頼したリポジトリでだけ使う

言語サーバーはリポジトリの中のコードを実行することがあります。
rust-analyzerはCargoのbuild scriptとproc macroを動かし、typescript-language-serverはリポジトリに入っているtsserverを使います。
jdtlsはMavenを、csharp-lsはMSBuildを、sourcekit-lspは`Package.swift`を実行するサーバーです。
`--precise`は信頼できるリポジトリでだけ使ってください。
ferrindexが言語サーバーを起動するのも、利用者が信頼したリポジトリだけです。
既定ではどのリポジトリも信頼していません。

- リポジトリの中で端末から`ferrindex trust`を実行すると信頼します。`ferrindex untrust`で取り消します。
- `trust`は標準入力が端末のときだけ受け付けます。Claude CodeのBashツールのような呼び出しは終了コード2で拒否します。
- 信頼済みリストは`$XDG_DATA_HOME/ferrindex/trusted`（未設定なら`~/.local/share/ferrindex/trusted`）にルートのパスのSHA-256だけを記録します。
- 同じユーザー権限で動くプログラムが疑似端末で`trust`を実行したり、リストを書き換えたりすることは防げません。
- gitリポジトリの外では`trust`と`untrust`はどちらも終了コード3で終わります。

### 名前の一致に戻る場合

言語サーバーで確かめられないときは名前の一致の結果を返します。
このとき標準エラー出力に`ferrindex: --precise: <理由>。名前一致の結果です（未確定）`を出します。
サーバーが失敗したときの`<理由>`は`<サーバー名>: <エラー>`で、終了コードは名前の一致と同じです。
理由は次のとおりです。

- リポジトリを信頼していません。
- 名前と完全に一致する定義が無いか、5件を超えています。部分一致の定義は使わず、Markdownの見出しも問い合わせません。修飾した名前で戻るのは修飾の無い名前の定義が5件を超えるときです。
- サーバーが見つからないか、起動に失敗しました。
- 30秒以内に答えが無いか、答えが定まりませんでした。
- サーバーの答えが0件でした。読み込み中のサーバーも0件を返すので確定扱いしません。
- サーバーが問い合わせの種類に対応していません。solargraphとlua-language-serverは呼び出しの階層（callHierarchy）に対応しないので、`callers --precise`は常に名前の一致に戻ります。
- C、C++のリポジトリのルートに`compile_commands.json`がありません。clangdはこのファイルが無いと開いたファイルしか見ないので、ferrindexはclangdを起動しません。
- `refs`と`callers`で名前の一致で見つかった呼び出しの一部がサーバーの答えにありません。修飾の無い名前ではすべての定義に問い合わせて答えを合わせるので本来はどの呼び出しも答えに入るはずです。
- `impls`と`supers`で名前の一致で見つかった継承の一部がサーバーの答えにありません。`impls`は各行の実装している型を`supers`は基底の型の名前を、答えの型と比べます。

呼び出しがサーバーの答えから漏れる主な原因は次のとおりです。

- `require`の分割代入、`local f = require(...).f`のような別名、型を推論できない受け手を経た呼び出しは追えないサーバーがあります。
- プロジェクトの設定がテストファイルを除いているとそのファイルの呼び出しは答えに入りません。
- インデックスの外にある同名の関数（ライブラリなど）への呼び出しも答えに入りません。

名前の一致も0件のときは理由を出して終了コード1で終わります。
Goの`impls`と`supers`は名前の一致で答えられないので、理由の行は`ferrindex: --precise: <理由>`だけです。終了コードは1です。

サーバーは読み込みの途中でも少ない答えを返すことがあります。
そこでferrindexは約1秒おきに同じリクエストを送り、続けて2回同じ答えが返ったら採用します。
待つのはすべての問い合わせを合わせて30秒までです。

### 結果に入らない参照

次の参照は名前の一致に戻らず、注意も出ないまま`--precise`の結果から欠落します。

- サーバーのプロジェクトの外にあるファイルの参照は`refs --precise`の結果に入りません。tsconfigの`include`から外れたテストファイルなどが該当します。`refs`は呼び出しと継承以外の参照（型の名前の参照など）を名前の一致と突き合わせないので、欠けても気づけません。
- Pythonの`from m import f as _f`のように別名でimportした関数の呼び出し`_f()`は、pyrightの`callers`も名前の一致も`f`の呼び出しとして返しません。
- `refs --precise`はサーバーが返した定義そのものの位置を結果から除きます。
- `impls --precise`は問い合わせた型そのものを結果から除きます。

### 対応する言語サーバー

言語サーバーはferrindexには同梱していません。使う言語のものを別にインストールしてください。
ferrindexはサーバーをPATHと`~/.dotnet/tools`から探します。

| 言語 | サーバー | 必要なもの・制限 |
|---|---|---|
| Rust | rust-analyzer | 大きなリポジトリでは初回のbuild scriptとproc macroのビルドで30秒を超え、名前の一致に戻ることがあります。ビルドの成果物はインデックスのキャッシュの下に残るので、2回目からは速く答えます。 |
| Go | gopls | |
| Python | pyright-langserver | |
| TypeScript、JavaScript | typescript-language-server | |
| Java | jdtls | JDK 25以上（`FERRINDEX_JAVA_HOME`、Homebrewのopenjdk、`/usr/libexec/java_home`の順に探します） |
| C# | csharp-ls | .NET SDK（`DOTNET_ROOT`、Homebrewのdotnet、`/usr/local/share/dotnet`の順に探します）と`global.json`が指定するSDK |
| Ruby | solargraph | `callers`は常に名前の一致に戻ります。 |
| C、C++ | clangd | リポジトリのルートの`compile_commands.json` |
| Swift | sourcekit-lsp | |
| Lua | lua-language-server | `callers`は常に名前の一致に戻ります。 |
| Dart | dart language-server | |

`ferrindex servers`はインデックスにある言語ごとのインストール状況をタブ区切りで出します。
1行目は見出しで、2行目から言語が1行ずつ続きます。

| 列 | 内容 |
|---|---|
| `language` | 言語の名前（`rust`、`typescript`、`cpp`など） |
| `files` | インデックスにあるその言語のファイル数 |
| `server` | 使う言語サーバー |
| `installed` | サーバーが見つかれば`yes`、無ければ`no` |
| `prerequisite` | 上の表の「必要なもの」が揃えば`ok`、揃わなければ足りないもの |
| `install` | macOSでサーバーをインストールするコマンド |

Markdownにはサーバーが無いので出力されません。
コードのファイルが無ければ見出しだけを出し、終了コード1で終わります。
インデックスが使えないときは終了コード3です。

```text
$ ferrindex servers
language	files	server	installed	prerequisite	install
rust	2	rust-analyzer	yes	ok	rustup component add rust-analyzer
c	1	clangd	no	compile_commands.json	brew install llvm
```

リポジトリの`skills/ferrindex-lsp-setup/`は足りないサーバーを選んでインストールするClaude Code向けのskillです。
`cp -R skills/ferrindex-lsp-setup ~/.claude/skills/`でコピーし、「ferrindexの言語サーバーを入れて」と頼むと動きます。
このskillは`sudo`を使わず、`ferrindex trust`も実行しません。

### 作業ツリーへの影響

ferrindexは言語サーバーが作業ツリーに書かないよう次の設定で起動します。

- jdtlsのワークスペース（`-data`）はインデックスのキャッシュの下に置きます。`.project`などのメタデータもそこに書かせ、自動ビルドは止めます。
- rust-analyzerが呼ぶcargoの`target/`は`CARGO_TARGET_DIR`でインデックスのキャッシュの下に向けます。
- sourcekit-lspが使うSwiftPMの`.build/`は`--scratch-path`でインデックスのキャッシュの下に向けます。

それでも次のファイルは作業ツリーに作られます。

| サーバー | 作られるもの |
|---|---|
| csharp-ls | 各プロジェクトの`obj/`。MSBuildが依存の復元とプロジェクトの評価に使います。 |
| rust-analyzer | `Cargo.lock`。無いCargoプロジェクトで、依存を解決したcargoが作ります。 |
| clangd | `compile_commands.json`の隣の`.cache/clangd/index/`。バックグラウンドインデックスと、その中の`.gitignore`です。 |

サーバーとその子プロセスは問い合わせのたびに止まります。
Ctrl-Cや`kill`でferrindexを止めた場合も同じです。
そのためにferrindexはサーバーごとに`sh`のプロセスを1つ起動します。
この`sh`はferrindexとの間のパイプが閉じるのを待ち、閉じたらサーバーのプロセスグループを止めます。

## インデックスの更新

ferrindexは検索のたびに全ファイルの更新時刻を調べ、変わったファイルだけを解析し直します。
人が編集しても他のツールが書き換えても結果は常に現在のファイルを反映します。

## 対応言語

Rust、Python、TypeScript（TSXを含む）、JavaScript、Go、Ruby、Java、C、C++、C#、Swift、Dart、Lua、Markdownに対応しています。

なお、次の定義は抽出できません。

- Dartのabstractメソッド
- C++のクラス外定義`A<T>::f`と`A::B::f`

`map`の概要は先頭20行にある最初のコメント、見出し、Pythonのdocstringから取ります。

## インデックスの保存先

インデックスは`$XDG_CACHE_HOME/ferrindex`（未設定なら`~/.cache/ferrindex`）に置きます。
リポジトリの作業ツリーにはファイルを作りません。
名前とパスの文字列はFSTの中にだけ入ります。
インデックスは圧縮しているだけで、暗号化していません。
ファイルを読める人は名前とパスを取り出せます。

## インデックスが使えないとき

終了コード3はインデックスが使えないことを示します。
gitリポジトリの外で実行した場合、インデックスが壊れて作り直せない場合、他のferrindexの更新を待ちきれずロックを取れなかった場合が該当し、理由は標準エラー出力に出ます。
このときは`rg -n`で探してください。

リポジトリに読めないディレクトリやファイルがあるとferrindexはそのパスを示して終了コード4で止まります。
結果が欠けたまま返すのを防ぐためです。
dockerのデータディレクトリのように不要なものは`.gitignore`に足すと走査の対象から外れます。

## Claude Codeのhook

`~/.claude/settings.json`に次を足すとClaude CodeがGrepやReadより先にferrindexを使うようになります。

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

| hook | 動作 |
|---|---|
| `session-start` | gitリポジトリの中ならferrindexの使い方の案内を標準出力に出します。Claude CodeはSessionStartの標準出力をClaudeのcontextに加えるのでClaudeはGrepやReadより先にferrindexを使えます。案内には`find`が定義の行を表示するので確認のgrepやReadは要らないこと、本体は`show`で引くこと、`callers`と`refs`の出力は確かめ直さずに使えること、この2つは名前の一致で探すので`Class.method`の修飾では絞れず、注意の行が出たときや正確な範囲が要るときは`--precise`を付けることを書きます。インデックスが無ければ`ferrindex update`をバックグラウンドで起動し、最初の検索が待つことがある旨を案内に足します。 |
| `post-tool-use` | Bashで`find`、`show`、`callers`、`refs`を実行した直後にだけ働きます。その出力で問いに答えられるならReadやrgで確かめ直さずに答えるようClaudeに添えます。確かめ直すとやり取りが1回増え、そのたびにClaude Codeは会話のcontextを読み直すので入力tokenが増えるからです。 |

gitリポジトリの外、対象外のコマンド、入力の誤りでは、どちらのhookも何も出力しません。終了コードは常に0です。

## テスト

`cargo test`でテストを実行します。
`--precise`のテストが使うのは本物の言語サーバーの代わりの`tests/fixtures/fake_lsp.py`です。
環境変数`FERRINDEX_LSP_<言語>`（例`FERRINDEX_LSP_RUST`）に起動コマンドを空白区切りで渡すとその言語のサーバーとして起動します。
`<言語>`は`RUST`、`GO`、`PYTHON`、`TYPESCRIPT`、`TSX`、`JAVASCRIPT`、`JAVA`、`CSHARP`、`RUBY`、`C`、`CPP`、`SWIFT`、`DART`、`LUA`のどれかです。

## アンインストール

1. `settings.json`から`ferrindex hook session-start`と`ferrindex hook post-tool-use`の項目を消します。
2. `cargo uninstall ferrindex`を実行します。
3. インデックスのディレクトリ`$XDG_CACHE_HOME/ferrindex`（未設定なら`~/.cache/ferrindex`）を削除します。
4. 信頼済みリストのディレクトリ`$XDG_DATA_HOME/ferrindex`（未設定なら`~/.local/share/ferrindex`）を削除します。

## ライセンス

MITライセンスです。全文は`LICENSE`を参照してください。
