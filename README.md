# mini-lm Native

Windows / macOS 両対応を目標にした、ローカルテキスト専用の NotebookLM 風デスクトップアプリです。

`source` ディレクトリ内の大きなテキストをローカルでチャンク化し、SQLiteへ永続保存します。質問時は全文をDeepSeekへ丸投げせず、全文検索、n-gram検索、ローカルベクトル検索を統合して根拠候補を作り、選択された文書だけを根拠に回答します。

## 現在の構成

- UI: Vite + vanilla JavaScript
- Native shell: Tauri 2
- Backend: Rust
- DB: SQLite + FTS5
- Vector: ローカル日本語n-gram hash embedding
- LLM: DeepSeek Chat Completions API
- API key: OS Keychain / Credential Manager。保存できない環境ではSQLite平文フォールバックを明示して使う。

## 開発実行

```bash
npm install
npm run tauri:dev
```

## ビルド

```bash
npm run tauri:build
```

macOSでは `.app` を作成します。WindowsではWindows環境で次を実行します。

```bash
npm run tauri:build:windows
```

全bundleを試す場合は次を使います。

```bash
npm run tauri:build:all
```

## チェック

```bash
npm run check
cargo test --manifest-path src-tauri/Cargo.toml
```

`npm run check` はフロントエンドのproduction buildとRustの型チェックを行います。Rustテストでは、日本語規程見出しのチャンク保持と、`扶養手当` のような短い語句のローカル検索を確認します。

## 使い方

1. アプリを起動する
2. DeepSeek API key を保存する
3. `source` ディレクトリを選択する
4. `インデックス作成/更新` を押す
5. 回答対象ファイルにチェックを入れる
6. 通常または高精度網羅モードで質問する

対応拡張子:

- `.txt`
- `.md`
- `.markdown`
- `.csv`
- `.tsv`
- `.json`
- `.log`
- `.text`

## 検索と回答

通常モード:

- SQLite FTS5
- 日本語n-gram index
- ローカルベクトル検索
- Reciprocal Rank Fusion相当の統合スコア
- ヒットチャンクと近傍チャンク
- DeepSeek streaming回答
- 回答後のclaim check

高精度網羅モード:

- 質問観点の展開
- 選択中の全チャンクをbatch確認
- batchごとの事実抽出
- 抽出事実の永続保存
- 事実統合
- 最終回答生成
- 回答監査

処理中、API送信中、rate limit待機、キャンセル、停止、完了は画面下部に表示されます。

## DeepSeek

既定モデルは `deepseek-v4-flash` です。設定画面で `deepseek-v4-pro` も選べます。

DeepSeek APIはOpenAI互換形式の `/chat/completions` を使います。API keyはOSの安全な保存領域へ保存します。OS Keychain / Credential Managerが使えない環境では、アプリがSQLite平文フォールバックへ切り替え、画面に警告を表示します。ログにはAPI keyを保存しません。

## ローカルデータ

アプリデータディレクトリに `mini-lm.sqlite3` を作成します。保存対象は以下です。

- source path
- documents
- chunks
- FTS index
- n-gram index
- chunk embeddings
- questions
- retrieval runs
- extracted facts
- answers
- logs

ファイルの `mtime`、サイズ、hash を比較し、変更があるファイルだけ再インデックスします。

## Git運用

このリポジトリはローカルGitで管理します。GitHubリポジトリは不要です。作業の節目ごとにコミットします。

除外対象:

- `node_modules/`
- `dist/`
- `src-tauri/target/`
- SQLite DB
- `.env`
- ログ

## 既知の制限

- 現在のベクトルは、外部モデルを使わないローカルn-gram hash embeddingです。ネットワーク不要で安定しますが、将来はONNX/Transformers系の多言語embeddingへ差し替えられる設計にしています。
- 高精度網羅モードは選択チャンクを全件batch確認するため、APIコストと時間が大きくなります。
- Windows用bundleはmacOS上では実作成できません。構成はTauriのクロスプラットフォーム前提ですが、Windows実機またはWindows CIでのビルド確認が必要です。
