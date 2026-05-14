# mini-lm

ローカルのテキスト資料だけを根拠に回答する、NotebookLM 風のデスクトップアプリです。macOS / Windows 両対応を前提に、Tauri 2 + Rust + Vite で構成しています。

大きなテキストをローカルで分割・索引化し、質問時は全文をそのまま LLM に投げず、全文検索、n-gram 検索、ローカルベクトル検索を組み合わせて根拠候補を作ります。回答は選択中の資料だけを根拠に生成します。

## 特徴

- ローカルフォルダ内のテキスト資料を読み込み
- ファイル単位で回答対象を選択
- 高精度網羅モードで固定
- DeepSeek Chat Completions API を使用
- API キーは OS Keychain / Credential Manager に保存
- SQLite + FTS5 + n-gram index + ローカル embedding で検索
- チャット履歴、アクティブチャット、アーカイブ済みチャットを管理
- macOS / Windows 向けネイティブアプリとしてビルド可能

## 技術構成

- UI: Vite + vanilla JavaScript
- Native shell: Tauri 2
- Backend: Rust
- DB: SQLite + FTS5
- Vector: ローカル日本語 n-gram hash embedding
- LLM: DeepSeek OpenAI-compatible Chat Completions API

## セットアップ

```bash
npm install
```

Rust と Tauri のビルド環境も必要です。

## 開発実行

```bash
npm run tauri:dev
```

## ビルド

macOS:

```bash
npm run tauri:build
```

Windows:

```bash
npm run tauri:build:windows
```

全 bundle:

```bash
npm run tauri:build:all
```

## チェック

```bash
npm run check
cargo test --manifest-path src-tauri/Cargo.toml
```

`npm run check` はフロントエンドの production build と Rust の型チェックを行います。

## 使い方

1. アプリを起動する
2. 資料フォルダを選択する
3. 更新ボタンでインデックスを作成する
4. 回答対象のファイルを選択する
5. 質問する

対応拡張子:

- `.txt`
- `.md`
- `.markdown`
- `.csv`
- `.tsv`
- `.json`
- `.log`
- `.text`

## 検索と回答の流れ

回答は常に高精度網羅モードで処理します。

1. 質問観点を整理
2. 関連資料を検索
3. SQLite FTS5、n-gram、ローカルベクトル検索を統合
4. 選択資料の候補チャンクを batch 確認
5. fact を抽出
6. fact を統合
7. 回答を生成
8. 回答の scope error / relationship error を監査
9. Markdown として表示

処理中、API 送信中、rate limit 待機、キャンセル、停止、完了などはチャット欄に状態表示します。

## DeepSeek API キー

API キーはアプリ内で設定します。UI 上には詳細なモデル設定を出さず、通常利用では迷わない構成にしています。

保存先:

- macOS: Keychain
- Windows: Credential Manager
- それらが使えない場合: SQLite 平文フォールバック

平文フォールバック時は画面に警告を表示します。API キーはログへ出力しません。

## ローカルデータ

アプリデータディレクトリに `mini-lm.sqlite3` を作成します。保存対象は次の通りです。

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

これらはアプリのローカルデータとして保存されます。

## 既知の制限

- 現在の embedding は外部モデルを使わないローカル n-gram hash embedding です。
- 高精度網羅モードは精度重視のため、質問によっては処理時間と API コストが大きくなります。
- Windows 用 bundle は Windows 環境または Windows CI での確認が必要です。
