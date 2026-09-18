# 開発ガイド

ソースコードからアプリを実行したり、配布用のファイルを作成したりする方向けの手順です。
アプリの導入と操作方法は[README](README.md)を参照してください。

## 開発環境

- Rust 1.98.1（GitHub Actionsと同じ版）
- Windows：Visual StudioのC++ビルドツール
- macOS：Xcode Command Line Tools
- 配布ファイルの作成：Python 3.11以降
- Windows用インストーラーの作成：[Inno Setup](https://jrsoftware.org/isinfo.php) 6.7以降

Windows版はWindows上で、macOS版はmacOS上でビルドします。

## ソースコードから実行

プロジェクトのルートフォルダで実行します。

```sh
cargo run --locked --release
```

利用枠を取得するには、公式CodexのアプリまたはCLIにChatGPTアカウントでログインしておく必要があります。

## コードの確認

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --check
```

## Codexとの接続

公式CodexのApp Serverを起動し、`account/read` と `account/rateLimits/read` でアカウントと利用枠を取得します。
仕様は[公式App Serverドキュメント](https://learn.chatgpt.com/docs/app-server)を参照してください。

Codexの起動には、利用者の既存のログイン状態と `CODEX_HOME` 設定を引き継ぎます。
アプリとCLIの両方がある場合はCLIを優先し、ウィジェットの設定で接続先を指定できます。

取得結果だけを確認するには、次のコマンドを使います。成功時は残量・リセット日時・リセット権の情報をJSONで出力します。

```sh
cargo run --locked --release -- --check
```

デスクトップアプリ経由の接続を確認する場合は、次のコマンドを使います。

```sh
cargo run --locked --release -- --check-app
```

## 配布ファイルの作成

`scripts/package.py` はアプリとライセンス文書をまとめ、ZIPとSHA-256ファイルを `dist/` に出力します。
`--build` を付けると、ビルド環境のホームフォルダやソースの絶対パスを共通表記に置き換えてから梱包します。

### Windows

```sh
python scripts/package.py --target x86_64-pc-windows-msvc --build
```

インストーラーも作成する場合：

```sh
python scripts/package.py --target x86_64-pc-windows-msvc --build --installer
```

Inno Setupを自動検出できない場合は、`--iscc "Inno Setupのフォルダ/ISCC.exe"` でコンパイラを指定します。

### macOS

Apple Silicon向けの例です。Intel向けはターゲットを `x86_64-apple-darwin` に変更します。

```sh
MACOSX_DEPLOYMENT_TARGET=13.0 python3 scripts/package.py --target aarch64-apple-darwin --build
```

### 署名

macOSのDeveloper ID署名と公証を利用する場合は、Macに設定済みの署名IDを `--sign-identity`、notarytoolのキーチェーンプロファイルを `--notary-profile` で指定します。
これらのオプションを省略すると、証明書を使わないアドホック署名を付けます。

Windowsの発行元署名を利用する場合は、署名済みの実行ファイルを `--binary` で指定して梱包します。
`--binary` と `--build` はそれぞれ別の実行方法です。

### GitHub Actions

`.github/workflows/build.yml` は、Windows x64・macOS Apple Silicon・macOS Intelのコードを検査し、配布ファイルを作成します。
Windowsではテストとインストーラーの作成も行います。生成したファイルはActionsの成果物から取得できます。

## ライセンス表記の更新

依存ライブラリを変更した場合は、cargo-about 0.9.2でライセンス一覧を更新します。

```sh
cargo install cargo-about --version 0.9.2 --locked
cargo about generate --locked scripts/licenses.hbs --output-file THIRD-PARTY-LICENSES.md
```

対象ライセンスとOSの設定は `about.toml`、文書の形式は `scripts/licenses.hbs` で管理します。
画像と日本語フォントの出典・変更内容は[素材のライセンス](assets/NOTICE.md)に記載しています。
