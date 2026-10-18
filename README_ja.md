# ATHENA

> 英語版: [README.md](./README.md)(同一内容)

ATHENA は、自然言語で依頼された仕事を複数の AI Agent で組織して遂行するシステムアーキテクチャ
です。人間は一段の自然言語指示を出すだけです。中心にいる **ATHENA Core Agent 自身も AI Agent**
であり、固定 Workflow を実行するのではなく、指示を理解し(Understand)、計画し(Plan)、
Director / Lead / Worker の階層へ委任し(Delegate)、戻ってきた結果を評価し(Evaluate)、
必要ならば再計画します(Re-plan)。

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

```sh
# ワンライナー(リポジトリ公開後。クローン済みなら ./install.sh)
curl -fsSL https://raw.githubusercontent.com/aquaxis/athena/main/install.sh | sh

# またはソースからビルド
cargo build --release

# 2. プロジェクトへ導入(.athena/ と aiprj 展開を生成)
./target/release/athena init

# 3. .athena/config.yaml を編集し、運用者が供給する 3 つの値を設定する
#    - core.agent.provider        (既定値なし — ATH-AGENT-008)
#    - core.agent.model           (既定値なし — ATH-AGENT-008)
#    - core.agent.delegable_tools (権限上限は明示が必須 — ATH-SEC-024)
#    シークレットを設定ファイルに書いてはならない(ATH-CFG-005)。
#    API トークンとプロバイダキーは環境変数またはシークレットマネージャから供給する。

# 4. 常駐 Core API を起動する(Store の整合、instance #0 の常駐化、監督、
#    安全な shutdown を行う構成ルート)
./target/release/athena-api

# 5. Core API (/api/v1, 127.0.0.1:8080) を利用する。CLI の各動詞が要求を発行する:
./target/release/athena run "TODO API を実装して"
./target/release/athena status
./target/release/athena task list

#    自然言語対話の入口は athena chat(承認依頼と質問への回答を会話内で
#    受けられる)。実装は crates/athena-cli/src/chat.rs を参照。
```

実装状況の注記: CLI の各動詞は現在、Core API へ発行する要求(method / path / body)を
決定論的に組んで出力する。トークンは `<ATHENA_HOME>/var/api-tokens.json` に SHA-256
ハッシュのみ記録される(`ATH-SEC-005`)。Studio は `scripts/build-studio.sh` で
WebAssembly へビルドし、5173 番ポートから提供する
(`athena-studio --api http://127.0.0.1:8080`)。

## ライセンス

MIT License。詳細は [LICENSE](./LICENSE) を参照。第三者材料のライセンス適合性は
取り込み前に review し、来歴を `docs/license-provenance.md` に記録します
(`ATH-DEV-019`)。
