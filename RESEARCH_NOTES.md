# mini-lm Research Notes

Last updated: 2026-05-15

この文書は、mini-lm の回答精度、網羅性、根拠性をさらに上げるための改善案と、参考にする研究・文献を残すためのメモである。

## 1. 次に実装したい改善案

### 1.1 抽出FACTSを構造化する

現状の抽出FACTSは主に `statement` と `quote` に依存している。これだと、最終回答の段階で主語、目的語、条件、効果が少し変わる余地がある。

次のフィールドを追加する。

```json
{
  "chunk_id": 123,
  "scope": "この事実が有効な範囲。文書名、章、条、対象制度など",
  "subject": "誰・何についての規定か",
  "object": "対象となる制度、手当、行為、書類、状態など",
  "condition": "成立条件、対象条件、除外条件",
  "effect": "支給される、控除される、必要になる、禁止される等の効果",
  "exception": "例外やただし書き",
  "polarity": "positive | negative | conditional",
  "quote": "根拠引用",
  "confidence": "high | medium | low"
}
```

目的:

- 主語の取り違えを防ぐ。
- 目的語の取り違えを防ぐ。
- 条件と効果の関係を固定する。
- 一部規定を全体規定として書く事故を減らす。
- `支給する` と `支給しない` の取り違えを防ぐ。

### 1.2 最終回答で主語・目的語・条件・効果の改変を禁止する

最終回答プロンプトに以下を追加する。

- `subject` を別の主体に置き換えない。
- `object` を別の制度・手当・行為に置き換えない。
- `condition` のない効果として断定しない。
- `effect` を反転させない。
- `scope` を広げない。
- FACTSにない一般化をしない。

### 1.3 広い質問ではsource内から下位概念を作る

例:

- 質問: `手当はどうなっていますか`
- source内の下位概念候補: `扶養手当`, `通勤手当`, `住宅手当`, `単身赴任手当`

重要なのは、一般知識で下位概念を作らないこと。source内の見出し、条、項目名、頻出語から候補を作る。

実装案:

1. 質問が広いか判定する。
2. source内の見出し・条文名・高頻度名詞句から下位概念候補を抽出する。
3. 下位概念ごとに検索・抽出する。
4. 最終回答で下位概念別に整理する。

### 1.4 根拠不足の列挙を質問範囲に絞る

根拠不足を広く書きすぎると、回答がうるさくなる。

禁止:

- 質問に関係ない不足情報を列挙する。
- source外の一般的チェックリストを不足として挙げる。

許可:

- 質問へ直接答えるために必要だが、source内で確認できない点だけを不足として書く。

### 1.5 監査で relationship error と scope error を見る

回答監査JSONを拡張する。

```json
{
  "verdict": "pass | warning | fail",
  "unsupported_claims": ["..."],
  "relationship_errors": [
    {
      "claim": "...",
      "problem": "condition/effect or subject/object relationship is wrong",
      "supported_fact_ids": ["F1", "F2"]
    }
  ],
  "scope_errors": [
    {
      "claim": "...",
      "problem": "claim scope is broader than source scope",
      "supported_fact_ids": ["F3"]
    }
  ],
  "polarity_errors": [
    {
      "claim": "...",
      "problem": "positive/negative/conditional meaning is reversed or overstated",
      "supported_fact_ids": ["F4"]
    }
  ]
}
```

### 1.6 監査後に自動修正する

現状は監査結果を保存するだけ。次は、監査で問題が見つかったら回答を書き直す。

流れ:

1. 最終回答を作る。
2. 監査する。
3. `relationship_errors`, `scope_errors`, `polarity_errors`, `unsupported_claims` があれば修正プロンプトを実行する。
4. 修正版を再監査する。
5. 問題が残る場合は、該当主張を削除または根拠不足として明示する。

## 2. 調査したい方向性

以下の研究・実装パターンを調べる。

- Retrieval-Augmented Generation
- 長文・大規模コーパス向け階層検索
- query-focused summarization
- graph-based RAG
- self-reflection / self-critique RAG
- claim verification
- retrieval evaluation
- long-contextの限界

## 3. 参考文献メモ

### 3.1 RAGの基本形

- [Retrieval-Augmented Generation for Knowledge-Intensive NLP Tasks](https://arxiv.org/abs/2005.11401)
  - RAGの基礎文献。
  - LLM本体の記憶だけでなく、外部の明示的な記憶を検索して生成する設計。
  - mini-lmでは、ローカル文書を唯一の外部記憶として扱う。
  - 重要な示唆: 回答はモデルの一般知識ではなく、検索された根拠に結び付ける。

### 3.2 長いコンテキストの限界

- [Lost in the Middle: How Language Models Use Long Contexts](https://arxiv.org/abs/2307.03172)
  - 長いコンテキストを入れられるモデルでも、中央付近にある重要情報を安定して使えるとは限らない。
  - mini-lmでは、10万字以上をそのまま一度に投げ込まない。
  - 重要な示唆: 大量文書は「全部入れる」ではなく、「検索、抽出、構造化、圧縮、監査」で扱う。

### 3.3 階層検索と長文向けRAG

- [RAPTOR: Recursive Abstractive Processing for Tree-Organized Retrieval](https://arxiv.org/abs/2401.18059)
  - 短いチャンクだけでなく、再帰的にクラスタリングと要約を行い、複数抽象度のツリーを作る。
  - mini-lmでは、`chunk -> section -> document -> corpus` の階層索引として参考にする。
  - 重要な示唆: 細部質問は下位チャンク、広い質問は上位要約も使う。

- [LongRAG: Enhancing Retrieval-Augmented Generation with Long-context LLMs](https://arxiv.org/abs/2406.15319)
  - 100語程度の短い単位だけで検索するのではなく、関連文書をまとめた長めの単位を検索する設計。
  - mini-lmでは、短いチャンク検索に加えて、親セクションや文書単位の周辺文脈を復元する。
  - 重要な示唆: 短すぎるチャンクは条件や例外を切り落とすため、親範囲の取得が必要。

### 3.4 広い質問、全体質問、GraphRAG

- [From Local to Global: A Graph RAG Approach to Query-Focused Summarization](https://arxiv.org/abs/2404.16130)
  - 従来RAGは、コーパス全体に対する広い質問に弱い。
  - エンティティ、関係、コミュニティ要約を作り、広い質問に対して部分回答を作って統合する。
  - mini-lmでは、本格的なグラフDBまでは入れず、まずは `GraphRAG-lite` として source由来の下位概念、関係、範囲を抽出する。
  - 重要な示唆: `手当はどうなっていますか` のような質問では、上位数チャンクだけでなく、source内の下位概念を列挙して全部見る。

- [Microsoft GraphRAG documentation](https://microsoft.github.io/graphrag//index/overview/)
  - 実装面では、エンティティ、関係、claims抽出、コミュニティ検出、複数粒度の要約、ベクトル埋め込みを組み合わせている。
  - mini-lmでは、claimsを `FACTS` として扱い、`scope / subject / object / condition / effect` を持たせる。

- [Microsoft Research: GraphRAG tool article](https://www.microsoft.com/en-us/research/blog/graphrag-new-tool-for-complex-data-discovery-now-on-github/?lang=zh-cn)
  - グローバル質問では、上位k件だけを見るnaive RAGが誤誘導しやすいことを強調している。
  - mini-lmでは、広い質問を検出したら、文書全体の見出し、制度名、頻出語、関係を先に展開してから検索する。

### 3.5 検索クエリ拡張と多段検索

- [Precise Zero-Shot Dense Retrieval without Relevance Labels](https://arxiv.org/abs/2212.10496)
  - HyDEの文献。質問から仮想文書を作り、その埋め込みで近い実文書を探す。
  - mini-lmでは、外部知識で仮想事実を作るのは危険なので、次の限定版だけ採用する。
  - 採用案: 「質問に答える文書なら出そうな表現」を検索語として作る。ただし、最終回答の根拠には使わない。

- [Interleaving Retrieval with Chain-of-Thought Reasoning for Knowledge-Intensive Multi-Step Questions](https://arxiv.org/abs/2212.10509)
  - 一回検索して読むだけでは、多段質問に足りないことがある。
  - mini-lmでは、最初の抽出FACTSから追加検索語を作り、必要なら再検索する。
  - 重要な示唆: `Aの場合、Bはどうなるか` のような条件付き質問では、条件、対象、効果を段階的に探す。

### 3.6 検索失敗の検出と修正

- [Corrective Retrieval Augmented Generation](https://arxiv.org/abs/2401.15884)
  - 検索結果が悪い場合に、検索結果の質を評価して補正する考え方。
  - mini-lmでは、外部Web検索は使わないが、ローカル内での補正は使う。
  - 採用案: 低信頼検索、0件検索、根拠不足が多い場合は、全選択文書スキャン、語形変換、下位概念検索へ切り替える。

### 3.7 監査、評価、ファクト分解

- [Self-RAG: Learning to Retrieve, Generate, and Critique through Self-Reflection](https://arxiv.org/abs/2310.11511)
  - 検索、生成、批評を組み合わせて factuality と citation accuracy を上げる方向性。
  - mini-lmでは、生成後に監査し、問題があれば自動修正する。

- [FActScore: Fine-grained Atomic Evaluation of Factual Precision in Long Form Text Generation](https://arxiv.org/abs/2305.14251)
  - 長文回答を atomic facts に分解し、それぞれが根拠に支えられているか評価する。
  - mini-lmでは、回答文を主張単位に分け、各主張がどの `FACTS` に支えられているか検査する。

- [QAFactEval: Improved QA-Based Factual Consistency Evaluation for Summarization](https://arxiv.org/abs/2112.08542)
  - 要約の事実整合性をQAベースで見る手法。
  - mini-lmでは、重要主張に対して「この主張を確認する質問」を作り、FACTSだけで同じ答えが出るかを見る方式の参考にする。

- [Ragas: Automated Evaluation of Retrieval Augmented Generation](https://arxiv.org/abs/2309.15217)
  - RAG評価を、検索文脈の関連性、回答の忠実性、回答の関連性などに分ける。
  - mini-lmでは、手動テストだけでなく、固定質問セットに対して `context_recall`, `faithfulness`, `answer_relevance` 相当を記録する。

- [ARES: An Automated Evaluation Framework for Retrieval-Augmented Generation Systems](https://arxiv.org/abs/2311.09476)
  - RAGを context relevance, answer faithfulness, answer relevance で評価する設計。
  - mini-lmでは、小さな評価セットを作り、変更のたびに回答品質を落としていないか確認する。

## 4. mini-lmに落とす実装方針

### 4.1 採用する基本設計

1. 質問を分類する。
   - 狭い質問: 特定語、制度、条項、条件を聞いている。
   - 広い質問: `手当`, `休暇`, `規程`, `どうなっていますか` のように範囲が広い。
   - 多段質問: 条件、対象、例外、比較、可否判断を含む。

2. 検索計画を作る。
   - exact / FTS / n-gram / vector を併用する。
   - source内の見出し、条、項目名、頻出名詞句から下位概念を作る。
   - 広い質問では、下位概念ごとの検索を必ず行う。
   - 検索0件でも止めず、選択文書の全体スキャンに進む。

3. FACTSを構造化して抽出する。
   - `scope`, `subject`, `object`, `condition`, `effect`, `exception`, `polarity`, `quote`, `confidence` を必須に近い扱いにする。
   - `statement` は表示用であり、最終回答の唯一の根拠にはしない。

4. FACTSを統合する。
   - 重複FACTSをまとめる。
   - 矛盾、例外、範囲違いを分ける。
   - 広い質問では下位概念別にFACTSを保持する。

5. 最終回答を生成する。
   - 最初に質問へ直接答える。
   - その後、条件、例外、根拠、根拠不足を整理する。
   - 主語、目的語、条件、効果、範囲をFACTSから改変しない。
   - 主要主張にはFACT IDを付ける。

6. 監査して自動修正する。
   - unsupported claim
   - relationship error
   - scope error
   - polarity error
   - insufficient evidence overreach
   - 問題があれば回答を修正し、再監査する。

### 4.2 重要な禁止事項

- 10万字以上をそのまま一発でLLMへ投げない。
- 上位k件の検索結果だけで、広い質問に答えない。
- source外の一般知識で下位概念を補わない。
- `AならB` を `常にB` に言い換えない。
- `扶養手当` の根拠を `手当全般` の根拠として扱わない。
- 根拠不足を、質問範囲外の一般チェックリストに広げない。

### 4.3 次の実装優先順位

1. FACTS schemaを拡張する。
2. 最終回答プロンプトに、主語、目的語、条件、効果、範囲の改変禁止を入れる。
3. 監査JSONに `relationship_errors`, `scope_errors`, `polarity_errors` を追加する。
4. 監査失敗時の自動修正パスを追加する。
5. 広い質問判定と、source由来の下位概念展開を追加する。
6. 検索0件でも全選択文書スキャンへ進むようにする。
7. `chunk -> section -> document` の親文脈復元を強化する。
8. 固定質問セットによるローカル評価ログを作る。
