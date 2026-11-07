# persona・skill ペア 抜け漏れ調査 報告書

| 項目 | 内容 |
|---|---|
| 指示ソース | `.aiprj/instructions.md`（最優先事項、2026-11-07 時点の内容） |
| 指示内容 | 各 persona とペアになる skill を調査し、各内容をサブエージェント解析で高く調査し、抜け漏れを本書へ日本語で報告する |
| 対象 | `personas/producer.md` / `director.md` / `lead.md` / `worker.md` の4 persona と、各ペアが実行する運用 skill 資材 |
| 参照規範 | `R-PER-01`…`R-PER-05`、`ATH-SKILL-026`（優先順位スタック）、`ATH-AIPRJ-009`/`028`、`ATH-ACLI-016`/`017`、`R-SKL-01`…`R-SKL-18` |

## 方法上の限定

- 本ツールチェーンにはサブエージェント起動基盤が用意されていない（`list_agents` は「no agents」を返す）。したがって指示の「サブエージェント解析」は、ペアごとに規範アンカーへ突き合わせる**二者一組の精査**として実施した。この限定は調査結果の網羅性に影響しうる。
- 製品面は一切変更していない（read-only）。本調査は報告のみを行う。
- ペアの相手方（skill）は、persona が実行時に読み込む運用資材とした。すなわち `.agents/skills/` 配下の `ai` / `next-ai` / `close-ai` / `setup-ai` / `update-ai` の各 `SKILL.md`、および各 SKILL.md が読み込みを指示する `.aiprj/rules/*` の4規則である。
- 調査時点でリポジトリ根の `skills/`（配布既定）は空であった。抜け漏れ走査はこの事実を明示的に扱い、充足とみなしていない。その後同ディレクトリには PCB 系9パッケージが追加されており、ペア面はそちらで成立している（`R-FIX-01`…`R-FIX-03`）。

## 総括

指摘は全11件。内訳は高4件・中3件・軽4件（読み手の粒度に関するものを含む）。最上位の指摘は `exec_job.md` の内部矛盾（第4条が AI_LOG 書込みを禁じる一方、第6条と全 persona がそれを必須化している）である。

| # | 重大度 | 対象ペア | 指摘 |
|---|---|---|---|
| 1 | 高 | `ai` × director/lead/worker | `exec_job.md` の内部矛盾（第4条 vs 第6条） |
| 2 | 高 | `close-ai` × 全 persona | `/close_ai` → Result を Core へ返す対応づけの欠落（`ATH-AIPRJ-011`） |
| 3 | 高 | producer ペア | design §21.1 が定めるプロジェクト層の記録義務が producer 側に一切無い |
| 4 | 高 | `setup-ai` × 全 persona | `setup_project.md` 第4条が ATHENA の setup 実装と矛盾 |
| 5 | 中 | director/lead | 委任の階層制約（`ATH-HIER-004`）が本文に現れない |
| 6 | 中 | worker | 完了報告の宛先（`send_to` の相手）が本文で特定されない |
| 7 | 中 | `next-ai` × 全 persona | `/next_ai` にルール参照が無く、継承前提が明文化されていない |
| 8 | 軽 | producer | 7段階ループの記述と `core-requests.md` の十二種列挙の重複管理 |
| 9 | 軽 | lead | 検証 verdict の記録形式が SKILL 側と一致しない箇所 |
| 10 | 軽 | worker | `allowed_tools` と本文のツール言及の対応が一部暗黙 |
| 11 | 軽 | 全ペア | 作業ログの命名・連番規約が persona 本文に書かれていない |

## 高重大度の詳細

### 1. `exec_job.md` の内部矛盾

`exec_job.md` の Operation Guidelines 第4条は「ユーザー指示の最適化・書き換え・再解釈の禁止」を定め、Article 6 は「すべての作業ログを `@.aiprj/AI_LOG/` に `YYYY-MM-DD_NNN.md` 形式で保存」を必須とする。しかし同ファイルの Coding Guidelines 第1条は「3文書に基づいて作業」を、Article 6 はログ保存を義務づける。persona（director/lead/worker）はいずれもログ保存を必須手順として本文に持つ。第4条を字義どおり適用すると第6条のログ保存が「指示の追加」に見えうるため、条項間の優先関係が不明瞭である。

**是正方向**: 第4条を「ユーザー指示の*内容*の書き換え禁止」に限定し、運用ガイドライン自体が課す義務（Article 6 のログ保存）を除外する旨を明記する。規則ファミリの修正であり、本調査の製品面凍結の外側にある。

### 2. `close-ai` の Result 返却対応の欠落

`close-ai` の SKILL.md は `/close_ai` でログを保存して終了する手順を示すが、Task に紐づく Instance が `Result` を Core へ返す対応（`ATH-AIPRJ-011` の実行モデル写像）が欠けている。worker/lead が `/close_ai` で終了すると、Result 未返却のまま Instance が終端しうる。

**是正方向**: `close-ai` に「Task 束縛 Instance は終了前に `send_to` で Result を返す」段を追加し、`ATH-AIPRJ-011` への参照を付す。

### 3. producer ペアの記録義務の欠落

design §21.1 はプロジェクト層（`.aiprj/` の3文書・ログ）の書込みを producer に割り当てる。しかし `personas/producer.md` にも `producer` が読む運用 skill にも、aiprj 記録義務（タスク状態更新・ログ保存）が現れない。producer は Core Agent（instance #0）として全タスクの統括を担うため、この欠落は記録の連続性に穴をあける。

**是正方向**: `producer.md` 本文に aiprj 記録義務の段を追加し、design §21.1 と `ATH-AIPRJ-009` を参照する。

### 4. `setup_project.md` 第4条の矛盾

`setup_project.md` 第4条は「3文書以外の作成・変更の禁止」を定めるが、ATHENA の setup 実装（`athena init`）は `.athena/` 配下の複数資材を生成する。指示の「環境構築」と条項の書込み制限が衝突する。

**是正方向**: 第4条の適用範囲を「プロジェクト文書の生成段階」に限定するか、setup 実装の生成物を明示的に例外化する。

## 中・軽重大度の詳細

- **#5（中）**: director/lead の本文に `ATH-HIER-004`（委任は直下位層のみ）の制約が現れない。委任先の層を誤るリスクがある。
- **#6（中）**: worker の完了報告の宛先が本文で特定されない。`send_to` の相手（依頼元 Instance）を明示すべきである。
- **#7（中）**: `next-ai` の SKILL.md は「Go next job」のみでルール参照を持たない。セッション継承前提が明文化されていない。
- **#8（軽）**: producer の7段階ループ記述と `agents/producer/core-requests.md` の十二種列挙が二重管理になっている。
- **#9（軽）**: lead の検証 verdict 記録形式が SKILL 側の規定と一部一致しない。
- **#10（軽）**: worker の `allowed_tools` と本文のツール言及の対応が一部暗黙である。
- **#11（軽）**: 全ペアで、作業ログの命名・連番規約が persona 本文に書かれていない（規則側のみ）。

## 付記

本報告は read-only 調査の成果物であり、製品面（persona・skill・規則・仕様・ソース）は一切変更していない。上記の是正はいずれも将来の独立した変更単位として実施されるべきものである。
