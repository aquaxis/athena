# ATHENA

> 英語版: [README.md](./README.md)(同一内容)

ATHENA は、自然言語で依頼された仕事を複数の AI Agent で組織して遂行するシステムアーキテクチャ
です。人間は一段の自然言語指示を出すだけです。中心にいる **ATHENA Core Agent 自身も AI Agent**
であり、固定 Workflow を実行するのではなく、指示を理解し(Understand)、計画し(Plan)、
Director / Lead / Worker の階層へ委任し(Delegate)、戻ってきた結果を評価し(Evaluate)、
必要ならば再計画します(Re-plan)。

> 名称の由来は頭字語です — **ATHENA = Autonomous Task Hierarchy & Execution Network
> Architecture**。仕様書(§1.1)では製品名ではなく、複数の AI Agent を組織化してタスクを
> 遂行するための **システムアーキテクチャ名称** とされています。

構成は次の 5 つの実行ユニットと、永続状態を一手に担う単一の権威あるストア
(`athena.db`、`redb` エンジン — 純 Rust 埋め込み型・ACID・ファイルバック)です。

| ユニット | 種別 | 役割 |
|---|---|---|
| `athena-api` | 常駐プロセス | Core API(HTTP + SSE)。既定で 8080 番ポートで待受け。Core Host と Adapter 実装を同一プロセスで結線する構成ルート(composition root) |
| `athena-core` | ライブラリ | Core Host — 監督・状態管理・Event 記録・ポリシー強制。**判断は行わない** |
| `athena-runtime-agent-cli` | ライブラリ | `agent-cli` Runtime Adapter + Process Manager |
| `athena` | CLI | コマンドライン UI(`athena chat` を第一級機能として設計) |
| `athena-studio` | Rust → WebAssembly | ブラウザ UI。既定で 5173 番ポート |

> **必須記述**(`ATH-DOC-007`): ATHENA Core Agent **= AI Agent** であり、ATHENA Core ≠
> 単純な Scheduler です。`athena-core`(Core Host)は判断を担わず、監督・状態管理・記録・
> ポリシー強制のみを担います。すべての思考は `agent-cli` 上の Agent Process(Core Agent の
> instance #0 を含む)で行われ、Agent Process は信頼境界の外側に置かれます。

## リポジトリレイアウト

```text
athena/
+-- Cargo.toml                        workspace
+-- crates/
|   +-- athena-api/                   bin   Core API(常駐)
|   +-- athena-cli/                   bin   `athena` コマンド
|   +-- athena-studio/                bin   Rust → WebAssembly
|   +-- athena-core/                  lib   Core Host
|   +-- athena-agent/                 lib   Agent 定義 / Instance / 階層
|   +-- athena-runtime/               lib   Runtime Interface(+ 擬似 Runtime)
|   +-- athena-messaging/             lib   Message / Broker
|   +-- athena-memory/                lib   Memory Manager
|   +-- athena-tools/                 lib   Tool 定義と権限
|   +-- athena-skills/                lib   Skill Manager / Registry / Catalog / Resolver
|   +-- athena-state/                 lib   State Manager と遷移表
|   +-- athena-events/                lib   Event 定義 / Store
|   +-- athena-persistence/           lib   Repository 実装(`redb` ストア)
|   +-- athena-config/                lib   設定スキーマと解決
|   +-- athena-types/                 lib   共有型(仕様 §26)
+-- runtimes/
|   +-- athena-runtime-agent-cli/     lib   agent-cli Adapter + Process Manager
+-- agents/  personas/  skills/       `athena init` が配布する既定ファイル
+-- tests/                            コンフォーマンススイート
+-- docs/                             システム文書(英語版 + 日本語版)
+-- spec/                             システム仕様書(唯一の権威あるソース)
+-- LICENSE                           MIT
```

モジュール境界(依存方向)は CI の静的検査で機械的に強制されます
(`tests/dependency_boundaries.rs`)。

## ドキュメント

| 文書 | 内容 |
|---|---|
| `spec/athena_spec_1.0.md` | システム仕様書(Version 1.0)。すべての規範的記述に要求 ID(`ATH-*`)を付与 |
| `docs/system-overview.md` | システム概要(英語) |
| `docs/system-overview_ja.md` | システム概要(日本語) |
| `docs/nfr_compliance.md`(+ `_ja.md`) | 非機能要件(NFR-01…18)の検証記録 |
| `docs/should_compliance.md`(+ `_ja.md`) | SHOULD レベル要件の適合監査と逸脱記録 |
| `docs/license-provenance.md`(+ `_ja.md`) | 第三者材料のライセンス review と来歴 |
| `docs/skill_delivery.md`(+ `_ja.md`) | Skill の Agent 到達メカニズム(確定済み設計) |
| `docs/persona_generation.md`(+ `_ja.md`) | Persona 生成の評価記録(Phase 8) |
| `docs/agent_cli_0290_findings.md`(+ `_ja.md`) | `agent-cli` 0.29.0 検証記録 |
| `docs/diagrams/` | draw.io マスターと生成 PNG |

## クイックスタート

### 1. インストール(ツールをビルドして PATH に配置)

```sh
curl -fsSL https://raw.githubusercontent.com/aquaxis/athena/main/install.sh | sh
```

ワンライナーは前提を確認し、このリポジトリをクローンして `cargo build --release` で
リリースビルドを行い、2 つのバイナリを PATH にインストールします:`athena`(CLI)と
`athena-api`(常駐 Core API)。冪等でツールのみを対象とし、プロジェクトの設定やデータは
一切触りません。クローン済みなら `./install.sh` を実行、またはソースから
`cargo build --release` でビルドしても構いません(この場合バイナリは
`./target/release/` 配下に置かれるため、以下のコマンドにそのパスを付けてください)。

### 2. 使い方

ATHENA はプロジェクトごとに構成されます。対象としたい作業ディレクトリの中に
ファイルが置かれます。

```sh
cd ~/projects/my-project

# .athena/ (config.toml, agents/, personas/, skills/, var/) を生成し、
# aiprj (.aiprj/, .agent-cli/) を展開する — プロジェクトに一度だけ実行
athena init

# 常駐 Core API を起動する(127.0.0.1:8080 で待ち受け)— 構成ルート:
# Store の整合、Core Agent の常駐化、監督、安全な shutdown
athena-api

# .athena/config.toml を編集し、運用者が供給する 3 つの値を設定する:
#   - core.agent.provider        (既定値なし — ATH-AGENT-008)
#   - core.agent.model           (既定値なし — ATH-AGENT-008)
#   - core.agent.delegable_tools (権限上限は明示が必須 — ATH-SEC-024)
# シークレットを設定ファイルに書いてはならない(ATH-CFG-005)。API トークンと
# プロバイダキーは環境変数またはシークレットマネージャから供給する。
# 設定は起動時に適用されるため、編集後は athena-api を再起動する(R-CFG-09)。
#
# core.agent.* の記載例(あくまで EXAMPLE — 既定値は存在しない):
#   プロバイダ・モデル・上限は自分で選ぶこと。

```toml
[core.agent]
provider = "ollama"                          # 例: ランタイムのプロバイダ決定
model = "glm-5.3-flash:cloud"                # 例: プロバイダのモデル
allowed_tools = ["fs_read", "fs_write", "send_to"]  # 既定は最小構成 (ATH-SEC-025)
delegable_tools = [
  "fs_read",
  "fs_write",
  "shell",   # 中リスク帯 — rm -rf の承認行は依然として有効
  "git",
  "send_to",
]
```
```

仕事は自然言語で依頼します:

```sh
athena run "TODO API を実装して"   # Task を作成してキューへ
athena status                       # システム全体の状態
athena task list                    # Task の一覧
```

または会話型の入口を使います。`athena chat` は第一級の自然言語インタフェースで、
Task の進行状況を表示し、承認依頼を会話内に表示して回答を受け付け、
質問(確認対象)への回答もその場で行えます。

その他のコマンド:`athena task create/show/cancel`、`athena plan show`、
`athena agent list/show`、`athena instance list/stop`(admin)、`athena runtime list`、
`athena event tail`、`athena approval list/approve/reject`、および spec §24.3 の Skill 語彙 —
`athena skill install/list/show/search/enable/disable/update/remove/validate`
(enable/disable/update/remove/validate は `(id, version)` のピンを要求、`ATH-SKILL-020`;
`update`/`validate` は要求を記録し — 常駐の skill lifecycle が §12.5 の Skill Manager /
Skill Validator 腿で `UPDATING`/`VALIDATING` 行を確定します —
`SkillUpdated`/`SkillValidated`/`SkillValidationFailed`)。
どのコマンドも `--json` を受け付けます。CLI は Core API(`/api/v1`)のみを経由し、
`agent-cli` を直接起動することはありません(`ATH-CLI-001`/`002`)。

計画: Core Agent が計画を提出すると、常駐はそれを計画として受理します — Core Host が
`PlanId`/`PlanStepId` を発行し、リビジョンを刻み(提出は編集対象のリビジョンを名乗る必要があり、
古い場合は現行リビジョンを返して拒否します)、`depends_on` の循環を書き込み前に検査し、確定を
`PlanCreated` または `PlanRevised` として記録します。旧リビジョンは上書きせず保存されます。
計画の提出は Task を**終了させません**: 計画は仕事の形であって成果物ではないため、Task は実行中の
まま保たれ、Agent には発行結果を名指す `control_response` が返ります。

保持期間の掃除: 常駐は 1 分周期で掃除を回します — `events.retention_days` を過ぎた Event、
期限切れの確認要求、期限切れの承認、冪等キー、配送記録、そして終了した Instance の `.aiprj` 作業領域(同じ
Event の保持期間を過ぎたもの)。領域の削除は、ストアの行とディスク上のディレクトリが揃って
「ATHENA がその Instance のために作った領域」と認められた場合に限ります(記録されたパス、
または `var/instances/<id>/.aiprj` の厳密な一致 — かつ instances ツリー内に解決され、Runtime が
残す `AI_LOG/` の形を備えること)。それ以外は件数として報告し、ディスク上に残します。各周期の
件数は掃除の監査 Event に載ります。承認は期限が過ぎると**拒否**として解決され、沈黙が同意に
なることはありません — 2 つの期限切れ辺(応答のないまま期限が来た要求と、消費前に失効した許可
スコープ)がともに `EXPIRED` へ遷移し、それを待っていた Task は待ち続けずに失敗します。

観測: `GET /api/v1/metrics` は spec §30.3 のカタログ全体を Prometheus テキストで答えます —
23 家族すべてが記録された事実から観測されます。Task・Instance・Approval・Message・Event の各行が
ストア側の家族(状態別件数、所要時間、`error_class` 別の失敗と再試行、`skill_id` 別の Skill
読込、outcome 別の承認、Broker 滞留数ゲージ、Core Agent の最終 Event 経過秒)を満たし、要求側の
家族(`athena_api_requests_total`、`athena_api_request_duration_seconds`)は API 自身が応答した
要求を一致ルートと状態コードで標識して数えます。どの行も答えなかった家族は 0 を 1 行だけ出力し
(収集側のグラフが系列の有無で切り替わらないように)、ストアが答えなかった開いたラベル値は
捏造せず不在のままとします。

## 更新

ツールとプロジェクトの定義を最新に保ちます:

```sh
athena update
```

`athena update` を実行すると、ワンラインインストーラ
(`curl -fsSL https://raw.githubusercontent.com/aquaxis/athena/main/install.sh | sh`)
と同等の作業を行います: 公開リポジトリを取得し、release モードでビルドし、
`athena` と `athena-api` のバイナリを PATH へ再インストールします。
実行ディレクトリから `.athena/` プロジェクトが解決できる場合は、さらに
プロジェクトの `personas/` と `skills/` **のみ**をリポジトリの同梱既定値から
更新します — 追加は積み上げ式で、ユーザーが改変した定義を上書きすることは
ありません(`config.toml`、`agents/`、`var/` には一切触れません)。
`.athena/` のないディレクトリではツール更新のみが実行されます。

実装状況の注記: CLI の各動詞は Core API へ要求を実際に送り、応答を描画します。
計画面は `` `--dry-run` `` で残り、`` `--json` `` は API 自身の応答本文をそのまま返します
(`athena status` は `GET /api/v1/status` — ストアのカウンタと Core Agent の状態 — を読み、
`athena chat` はセッションループで Task を投稿し状態を追跡し、確認質問や承認を
インラインで提示します)。`athena token issue --name <名前> --role <viewer|operator|admin>`
(`--json` 対応) はトークンを発行します — 平文は 1 回だけ表示され、registry には SHA-256
ハッシュのみが記録されます (`ATH-SEC-005`、`<ATHENA_HOME>/var/api-tokens.json`)。
`athena token list` は記録を一覧表示します。API の読み取り面も提供されます (`R-API-09`):
エージェント定義、エージェントインスタンス（`/instances/{id}/stop` の admin 緊急停止を含む）、
ランタイムインスタンス、ツール実行、評価、イベント、階層、各タスクの plan/results/messages は
すべてストアの実行で、カーソルページ分割 (`ATH-API-005`) で応答します。Studio は `scripts/build-studio.sh` で
WebAssembly へビルドし、ビルド済みモジュールを 5173 番ポートから提供します
(`athena-studio --api http://127.0.0.1:8080`)。Studio オリジンのページは CORS 制限付きの
API (ループバックの Studio オリジンのみ許可) を読み、`sessionStorage` に保持した
貼り付けトークンで認証します — boot 時に採用し、API が 401 を返したときは頁内パネルで
1 回だけ再貼り付けします。ディープリンク (`/?view=…`、`/?task=…`) は対応する画面に着地し、
タブバーは画面遷移で消えず、Live 画面は `Last-Event-ID` を錨とする認証付き fetch リプレイで
イベントストリームを追います。頁自体はタスクを作成しません — タスク作成は CLI の担当です
(API の一 lease 規律)。

下位エージェントも稼働します: `.athena/config.toml` の `agents` 一覧は起動ごとにストアへ
登録され (`persona` ファイルの front matter が `level` と `allowed_tools` を補い、
`provider`/`model` は必須のまま — `ATH-AGENT-008`)、委任は実在する定義に対してのみ
受理されます (`ATH-AGENT-002`)。Core Agent はこの一覧には決して現れません
(インスタンス #0 は `core.agent` 自身の面 — `ATH-CORE-022`)。受理された委任はそのまま
**起動**されます: Instance 行は `CREATED → STARTING → IDLE` を歩き、`AgentStarting` /
`AgentStarted` の各 Event が Task の相関チェーンに載り、Runtime Instance 行が 1:1 で
作られ (`ATH-RT-012`)、行には Runtime 自身の ID と実際に書かれた `AI_LOG` の位置が
記録されます。起動が拒否された場合は行を途中状態に残さず `STARTING → FAILED` と
`AgentFailed` (`error_class` / `attempt` / `side_effects = unknown`) を記録します。
Core Agent 自身のライフサイクルもカタログ Event で答えるようになりました
(`CoreAgentStarted` / `CoreAgentRestarted` / `CoreAgentEscalated`、`ATH-CORE-031`) —
これにより `athena_core_agent_restarts_total` は常時 0 ではなく実測の再起動数を報告します。

起動した Instance には**指示が届きます**: Core Agent が書いた委任指示は Broker 経由の
`instruction` Message として届き (`ATH-MSG-001` — 直結経路は作りません)、その委任自身の
thread に乗り (`ATH-MSG-013`)、`timeout.message_reply` の応答期限を持ち (`ATH-MSG-005`)、
経路は階層で照合されます (親子は常に可、兄弟は Core Agent が記録した許可リストのみ —
`ATH-MSG-006`/`018`)。滞留上限 (`messaging.max_queue_depth`、`ATH-MSG-009`) を超える場合は
一時的エラーとして拒否します。行と配送記録は送信の受理より先に永続化され (`ATH-MSG-012`)、
相手が受け取った事実は受理として記録され (`ATH-MSG-014`)、両者は `AgentMessageSent` /
`AgentMessageReceived` を発行します (`ATH-MSG-007`)。相手が受け取らなかった Message は
滞留したままとなり、それがそのまま `athena_message_queue_depth` の値になります。

## ライセンス

MIT License。詳細は [LICENSE](./LICENSE) を参照。第三者材料のライセンス適合性は
取り込み前に review し、来歴を `docs/license-provenance.md` に記録します
(`ATH-DEV-019`)。
