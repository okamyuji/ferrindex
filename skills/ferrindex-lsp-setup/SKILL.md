---
name: ferrindex-lsp-setup
description: ferrindex の --precise が使う言語サーバーを、利用者が選んだものだけ入れる。「ferrindex の言語サーバーを入れて」「--precise を使えるようにして」「ferrindex servers で no の言語を入れて」と頼まれたときに使う。
---

# ferrindex の言語サーバーを入れる

`ferrindex refs/callers/impls/supers --precise` は、言語ごとの言語サーバーを起動して答えを確かめます。
このskillはリポジトリにある言語のうち、サーバーが無いか前提が足りないものを利用者に示します。入れるのは選ばれたものだけです。

## 守ること

- 利用者が選ばなかったものは入れません。選択を聞く前に何も入れません。
- `sudo`を使いません。権限が足りずに失敗したら、出力を見せて止めます。
- コマンドが失敗したら、その出力をそのまま見せて止めます。次のコマンドに進みません。
- シェルの設定ファイル（`~/.zshrc`など）を書き換えません。PATHの追加が要るときは、追加する行を利用者に示します。
- `ferrindex trust`は実行しません。リポジトリの信頼は利用者が端末で行います。

## 手順

### 1. 現状を読む

リポジトリの中で次を実行します。

```sh
ferrindex servers
```

出力はタブ区切りで、1行目が見出しです。
列は`language`、`files`、`server`、`installed`、`prerequisite`、`install`の順に並びます。
終了コード3ならgitリポジトリの外なので、リポジトリの中で頼み直すよう伝えて終わります。
終了コード1はコードのファイルが索引に無いことを示すので、入れるものが無いと伝えて終わります。

### 2. 候補を選ぶ

`installed`が`no`か、`prerequisite`が`ok`でない行だけを候補にします。
同じ`server`の行（`typescript`、`tsx`、`javascript`はどれもtypescript-language-server）は1つの候補にまとめ、`files`を足します。
候補が無ければ、すべて揃っていると伝えて終わります。

### 3. 利用者に聞く

AskUserQuestionを`multiSelect: true`で使います。

- 1問の選択肢は2〜4個にします。問は最大4問です。
- 候補が4個以下なら1問にまとめます。
- 候補が5個以上なら、言語のまとまりで問を分けます。たとえばWeb（TypeScript、JavaScript、Python、Ruby）、システム（Rust、Go、C、C++、Swift）、JVMと.NET（Java、C#、Dart、Lua）のように分けます。
- 1問の候補が1個だけになったら、「入れない」を2個目の選択肢にします。問を分けたときも同じです。
- 各選択肢の`label`はサーバー名と言語、`description`には`install`列のコマンドと、足りない前提を書きます。

前提ごとの扱いは次のとおりです。

- `JDK 25以上` jdtlsの前提です。`brew install openjdk`で入ります。`install`列のコマンドに含まれています。
- `.NET SDK（DOTNET_ROOT）` csharp-lsの前提です。`brew install dotnet`で入ります。`install`列のコマンドに含まれています。
- `compile_commands.json` clangdの前提です。コマンドでは入りません。ビルドの仕組み（CMakeの`-DCMAKE_EXPORT_COMPILE_COMMANDS=ON`、`bear -- make`など）で作るよう、説明だけを選択肢に書きます。リポジトリにファイルを作るのは、利用者が別に頼んだときだけです。

### 4. 入れる

選ばれた候補ごとに、`install`列のコマンドをBashで1つずつ実行します。
失敗したら、出力を見せて止めます。

### 5. 確かめて報告する

もう一度`ferrindex servers`を実行し、選んだ行の`installed`と`prerequisite`を報告します。
入れた後も`installed`が`no`のときは、実行ファイルがPATHに無いことがほとんどです。
次の行を利用者に示します。

- clangd Homebrewの`llvm`はPATHに載りません。`export PATH="/opt/homebrew/opt/llvm/bin:$PATH"`をシェルの設定に足すよう伝えます。
- gopls `go install`は`$(go env GOPATH)/bin`に入れます。そのディレクトリをPATHに足すよう伝えます。
- npmやgemで入れたもの そのパッケージ管理の`bin`ディレクトリがPATHにあるかを確かめるよう伝えます。

最後に、`--precise`を使うにはリポジトリの中の端末で`ferrindex trust`を実行する必要があることを1行で伝えます。
