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

構成は次の 5 つの実行ユニットと、永続状態を一手に担う単一の権威ある SQLite ストア
(`athena.db`)です。

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
|   +-- athena-persistence/           lib   Repository 実装(SQLite)
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

# .athena/ (config.yaml, agents/, personas/, skills/, var/) を生成し、
# aiprj (.aiprj/, .agent-cli/) を展開する — プロジェクトに一度だけ実行
athena init

# 常駐 Core API を起動する(127.0.0.1:8080 で待ち受け)— 構成ルート:
# Store の整合、Core Agent の常駐化、監督、安全な shutdown
athena-api

# .athena/config.yaml を編集し、運用者が供給する 3 つの値を設定する:
#   - core.agent.provider        (既定値なし — ATH-AGENT-008)
#   - core.agent.model           (既定値なし — ATH-AGENT-008)
#   - core.agent.delegable_tools (権限上限は明示が必須 — ATH-SEC-024)
# シークレットを設定ファイルに書いてはならない(ATH-CFG-005)。API トークンと
# プロバイダキーは環境変数またはシークレットマネージャから供給する。
# 設定は起動時に適用されるため、編集後は athena-api を再起動する(R-CFG-09)。
#
# core.agent.* の記載例(あくまで EXAMPLE — 既定値は存在しない):
#   プロバイダ・モデル・上限は自分で選ぶこと。

```yaml
core:
  agent:
    provider: "ollama"                                # 例: ランタイムのプロバイダ決定
    model: "glm-5.3-flash:cloud"                      # 例: プロバイダのモデル
    allowed_tools: ["fs_read", "fs_write", "send_to"] # 既定は最小構成 (ATH-SEC-025)
    delegable_tools:
      - "fs_read"
      - "fs_write"
      - "shell"     # 中リスク帯 — rm -rf の承認行は依然として有効
      - "git"
      - "send_to"
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

その他のコマンド:`athena task show/cancel`、`athena plan show`、
`athena agent list/show`、`athena instance list/stop`(admin)、`athena runtime list`、
`athena event tail`、`athena approval list/approve/reject`、
`athena skill list/show/search/register/enable/disable`。
どのコマンドも `--json` を受け付けます。CLI は Core API(`/api/v1`)のみを経由し、
`agent-cli` を直接起動することはありません(`ATH-CLI-001`/`002`)。

実装状況の注記: CLI の各動詞は現在、Core API へ発行する要求(method / path / body)を
決定論的に組んで出力する。トークンは `<ATHENA_HOME>/var/api-tokens.json` に SHA-256
ハッシュのみ記録される(`ATH-SEC-005`)。Studio は `scripts/build-studio.sh` で
WebAssembly へビルドし、5173 番ポートから提供する
(`athena-studio --api http://127.0.0.1:8080`)。

## ライセンス

MIT License。詳細は [LICENSE](./LICENSE) を参照。第三者材料のライセンス適合性は
取り込み前に review し、来歴を `docs/license-provenance.md` に記録します
(`ATH-DEV-019`)。
