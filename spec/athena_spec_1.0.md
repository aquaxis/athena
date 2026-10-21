# ATHENA システムアーキテクチャ仕様書

| 項目 | 内容 |
|---|---|
| Version | 1.0（rev. 2026-10-22） |
| Status | System Architecture Specification |
| Owner | ATHENA Architecture |
| 実装言語 | Rust |
| 本文言語 | 日本語（技術用語は英語表記） |

本仕様書は、AI Agent オーケストレーション・アーキテクチャ **ATHENA** の構造、実行モデル、
インターフェース、基盤要件、および開発方針を定義する。ATHENA の中心は固定 Workflow を実行する
Scheduler ではなく、人間の自然言語指示を理解し、計画し、Agent 階層へ委任し、結果を評価して
再計画する **ATHENA Core Agent** である。本書は AI コーディングエージェントおよび人間の開発者が
実装の根拠として参照できる水準を目標とし、すべての規範的記述に要求 ID を付与する。

#### 改訂履歴

| 日付 | 改訂内容 | 根拠 |
|---|---|---|
| 2026-10-22 | 永続化エンジンの確定（open item O-6 の決着）。第 27.2 節を改訂（`ATH-PERS-004` に SQLite 使用禁止、v1 エンジンを純 Rust の埋め込み型トランザクションストア `redb` と明記。`ATH-PERS-005` をエンジン中立の耐久性条項へ改訂）。第 4.3.1 / 4.3.2 / 4.3.4 / 5 / 23.1 / 32.1 節の SQLite 表記をエンジン中立または `redb` へ整合。SQLite 使用禁止の指示（2026-10-18 指示ソース 最優先事項）に基づく。エンジン固有の NFR 計測（NFR-02/03/04/13）は新エンジンで再記録 | `ATH-PERS-004`、`ATH-PERS-005`、`ATH-GEN-006` |
| 2026-10-18 | Open item O-3 を確定: 第 18.3 節の既定値を v1 の確定値として確定し、`ATH-SCHED-015` を確定後の文言に改訂。根拠は Phase-1 飽和計測(AI_LOG 017)と本改訂セッションの実ランナー計測(NFR-02/03/11/13/14/16)。全項目の設定変更可能性は維持 | ATH-SCHED-015 |
| 2026-10-18 | 第 32.2 節の依存許可表へ `athena-api → runtimes/*`(構成ルート)を追加。常駐プロセス(athena-api)が Core Host と Adapter 実装を同一プロセスで結線し registry へ登録する役割を明示した(ATH-DEV-023 の不明依存を許可として明記)。`athena-core → runtimes/*` の禁止は不変 | ATH-DEV-023、ATH-RT-008 |
| 2026-10-18 | `agent-cli` の検証基盤を 0.29.0 へ更新(open item O-1 の再検証)。0.28.0 で観測された `serve` 起時 panic と `spawn` の登録不全は 0.29.0 で解消を確認。第 2.2 節に検証基準の注記を追記し、第 2.2.4 節を新設して設定キーの正準綴りを必須化(`ATH-ACLI-018`)— legacy 綴りのキーは読み込み時に正準化して書き戻されるため、Adapter は正準綴りのみを出力しなければならない(制御表面のバイト安定性、`ATH-CORE-044`) | `ATH-DEV-022`、`ATH-ACLI-002` |
| 2026-10-18 | Skill の到達メカニズムを確定(open item O-2)。第 12.2.1 節を新設し `ATH-SKILL-039` を追加。Loader が配置した各 Skill に対し Instance の `.agent-cli/commands/athena-skill-<id>.md` を生成し、絶対パスで配置済み `SKILL.md` を読むよう指示する(`athena-cli` 0.23.0 検証に基づく IPC 経路の手順ファイル読取り(`ATH-ACLI-017`)を利用)。`docs/skill_delivery.md` に詳細 | ATH-SKILL-017 |
| 2026-10-14 | `agent-cli` 0.23.0 の実挙動検証に基づき、コマンド定義の展開経路を改訂。第 2.2.3 節を新設し、ATH-ACLI-016 / ATH-ACLI-017 を追加。第 2.2 節の表、ATH-ACLI-003、ATH-ACLI-014、ATH-AIPRJ-011、ATH-AIPRJ-028 を更新。slash command の展開は `run`（REPL）専用であり IPC 経路では展開されないため、`commands/` 配下は Agent が絶対パスで読み取る手順ファイルとして扱う | ATH-DEV-022、ATH-ACLI-002 |

---

## 目次

**Part I 総論**

1. はじめに
2. 参照文書
3. 用語集
4. コンセプトと設計原則
5. 制約とポリシー

**Part II アーキテクチャ**

6. 全体構成
7. ATHENA Core Agent
8. Agent と Agent Hierarchy
9. Persona
10. Agent間通信
11. Tool
12. Skill
13. Memory
14. Runtime と agent-cli Adapter
15. Human-in-the-Loop

**Part III 実行モデル**

16. Task と Plan
17. 状態遷移
18. Scheduler と並行制御
19. Result と Evaluation
20. Failure / Retry / Timeout / Recovery
21. 実行フロー

**Part IV インターフェース**

22. Core API
23. Event Architecture
24. ATHENA CLI
25. ATHENA Studio

**Part V 基盤**

26. データモデル
27. 永続化
28. Configuration
29. Security
30. Observability
31. 非機能要件

**Part VI 開発**

32. Repository 構成とモジュール境界
33. 開発フェーズと MVP 受け入れ条件
34. テスト戦略
35. ドキュメント生成規約
36. aiprj 運用規約
37. 初期実装でやらないこと

---

## Part I 総論

### 1. はじめに

#### 1.1 目的

本書の目的は、ATHENA の設計を実装可能な粒度で定義することである。

ATHENA は製品名ではなく、複数の AI Agent を組織化してタスクを遂行するための
**システムアーキテクチャ名称**である。

> **ATHENA = Autonomous Task Hierarchy & Execution Network Architecture**

本書は以下を定義する。

```text
ATHENA の概念と設計原則
コンポーネント構成とモジュール境界
Core Agent の動作モデル
Agent 階層と Agent間通信
Skill / Tool / Memory の機構
Runtime 抽象と agent-cli 統合
Task / Plan / 状態遷移 / 失敗処理
Core API / Event / CLI / Studio
データモデル / 永続化 / 設定 / Security / Observability
非機能要件と開発フェーズ
```

#### 1.2 適用範囲

- 本書は ATHENA 本体（Core、Runtime、API、CLI、Studio）の仕様を対象とする。
- ATHENA が操作対象とする外部プロジェクトの仕様は対象外とする。
- LLM そのものの実装、推論エンジンの開発は対象外とする（第 37 章）。

#### 1.3 想定読者

```text
ATHENA を実装する AI コーディングエージェント
ATHENA を実装・レビューする開発者
ATHENA を運用する管理者
ATHENA へ Agent / Skill を追加する開発者
```

#### 1.4 要求キーワード

本書の規範的記述は以下のキーワードを用いる。キーワードは RFC 2119 の MUST / SHOULD / MAY に
対応する。

| キーワード | 表記 | 意味 |
|---|---|---|
| 必須 | MUST / MUST NOT | 実装が満たさなければならない要求。満たさない実装は仕様違反とする。 |
| 推奨 | SHOULD / SHOULD NOT | 原則として満たすべき要求。逸脱する場合は理由を記録する。 |
| 任意 | MAY | 実装者の判断に委ねる。 |

規範的要求は必ず上記キーワードを伴う。キーワードを伴わない記述は説明・背景であり、規範的要求
ではない。

- **ATH-GEN-001** (MUST) 実装は、本書の MUST 要求をすべて満たさなければならない。
- **ATH-GEN-002** (MUST) SHOULD 要求から逸脱する場合、実装は逸脱理由をリポジトリ内の設計記録へ
  残さなければならない。
- **ATH-GEN-003** (MUST NOT) 実装者および AI コーディングエージェントは、本書の規範的記述を
  独自に緩和・再解釈してはならない。緩和が必要な場合は本書を改訂する。

#### 1.5 文書規約

- 本文は日本語、技術用語は英語表記とする。日本語文字と ASCII 字句の間には可読性のため半角空白を
  入れる表記を基本とし、文書全体で統一する。ただし確立した複合語（`Agent間通信`、`Agent階層` 等）
  および識別子・コードブロック内では空白を入れない。
- 外来語の語尾が `-er` / `-or` / `-ar` に当たるカタカナ語は、**長音符を付けない**
  （コンテナ、ヘッダ、パラメータ、フィルタ、ポインタ、マネージャ）。
- `interface` のカタカナ表記は「インターフェース」とする。
- 既定値を本文中で示す場合は半角コロンと半角空白を用いる（`既定: 8`）。
- 規範的な定義（状態、権限、エンドポイント、メトリクス）は表で表現する。
- 図は構造理解のための補助であり、図のみが要求を担わない。各図には不変条件を文章で併記する。
- 相互参照は章番号と要求 ID で行う。
- コードブロックには必ず言語識別子を付与する（`text` / `rust` / `json` / `yaml` / `toml` /
  `bash` / `http` / `xml`）。
- パスを示す場合は基準を明示する。`<PROJECT_ROOT>/` または `<ATHENA_HOME>/` を接頭するか、
  直前の文で基準を述べる。接頭辞のない `var/` のような表記は、基準が直前に示されている場合に
  限る（`ATHENA_HOME` と `PROJECT_ROOT` は別のディレクトリであるため、省略すると配置が
  一意に定まらない）。
- 本文は 1 行あたり概ね 100 桁で折り返す。折り返しの結果、行頭が `（` で始まることを許容する。

- **ATH-GEN-004** (MUST) 本書を改訂する場合、図と本文の規範的記述を同時に更新しなければならない。
- **ATH-GEN-005** (MUST NOT) 本書は、図（ASCII art を含む）のみを根拠とする要求を含んではならない。

#### 1.6 要求 ID 体系

規範的記述は以下の形式の ID を持つ。

```text
ATH-<AREA>-<nnn>
```

`AREA` は以下を用いる。

| AREA | 対象 | AREA | 対象 |
|---|---|---|---|
| GEN | 文書全般 | API | Core API |
| CORE | Core Agent | EVENT | Event |
| AGENT | Agent | CLI | ATHENA CLI |
| HIER | Agent Hierarchy | STUDIO | ATHENA Studio |
| PERSONA | Persona | DATA | データモデル |
| MSG | Agent間通信 | PERS | 永続化 |
| TOOL | Tool | CFG | Configuration |
| SKILL | Skill | SEC | Security |
| MEM | Memory | OBS | Observability |
| RT | Runtime | NFR | 非機能要件 |
| ACLI | agent-cli 統合 | TEST | テスト |
| HITL | Human-in-the-Loop | DOC | ドキュメント規約 |
| TASK | Task | AIPRJ | aiprj 運用 |
| PLAN | Plan | DEV | 開発方針 |
| SCHED | Scheduler | OPS | 運用・配備 |
| STATE | 状態遷移 | POLICY | 制約・ポリシー |
| RESULT | Result | FAIL | 失敗処理 |
| EVAL | Evaluation | | |

- **ATH-GEN-006** (MUST) 要求 ID は公開後に再利用してはならない。要求を削除する場合、ID は
  欠番として残さなければならない。削除した ID を別の要求へ割り当ててはならない。

---

### 2. 参照文書

本章は ATHENA が前提とする外部プロジェクトと、その**前提インターフェース**を定義する。
本書の執筆時点でこれらの実装を読み取って検証していないため、以下は**前提**であり、実装着手時に
検証する対象である。

#### 2.1 参照プロジェクト

| 名称 | URL | ATHENA における役割 |
|---|---|---|
| `aquaxis/agent-cli` | https://github.com/aquaxis/agent-cli | Agent 実行基盤。初期 Runtime の実装対象。 |
| `aquaxis/aiprj` | https://github.com/aquaxis/aiprj | 各 Agent の AI 作業規約・プロジェクト管理基盤。 |
| `aquaxis/layers` | https://github.com/aquaxis/layers | Producer / Director / Lead / Worker 階層型 Multi-Agent の参考実装。 |

#### 2.2 agent-cli のインターフェース

ATHENA Runtime が利用する `agent-cli` の機能面は以下とする。

| 機能 | `agent-cli` の提供手段 | 動的指定 |
|---|---|---|
| 設定ファイル | 既定は `AGENT_CLI_CONFIG` → `~/.config/agent-cli/config.toml` へ `./.agent-cli/config.toml` を重ね合わせ。`--config` は**チェーンを置換**し、繰り返し指定で後者が優先 | **可**（`--config` / `AGENT_CLI_CONFIG`） |
| コマンド定義 | `[runtime] commands_dir`。**絶対パス可**。ただし slash command としての展開は `run`（REPL）**専用**であり、IPC 経路（`send` / `ask`）では展開されない（第 2.2.3 節） | **可** |
| Persona | `--persona` / `[runtime] persona_file`。絶対パス可 | **可** |
| Agent プロセスの起動 | `spawn`（detached。起動元に依存せず自身のセッションで動作し、起動元より長命。peer として自己登録）／ `serve`（headless 前景）／ `run`（REPL） | — |
| 停止 | `stop`（graceful shutdown 要求。転送失敗時は pid へ SIGTERM） | — |
| Agent間 IPC | `send <PEER> <TEXT>` / `ask`（応答取得）/ `list`（稼働 peer 一覧）/ `groups`。宛先は agent-id または表示名。peer レジストリは `[runtime] registry_dir` | — |
| Agent の識別とグループ | `--name`（表示名）/ `--group`（detached な子が継承） | **可** |
| LLM Provider の切り替え | `--provider` / `--model`、`[provider.*]`（claude / claude-code / codex / ollama / opencode / opencode-go / llama.cpp） | **可** |
| 事前診断 | `doctor`（設定、API キー、接続性、レジストリ、シェルツールの検査） | — |
| 委任の抑制 | `[spawn] max_depth`（既定 2）/ `max_children`（既定 4） | **可** |
| 会話文脈の保持 | `[history] enabled`（既定 **false**）/ `max_context_tokens`（既定 24000）/ `keep_recent_turns`（既定 6） | **可** |
| Tool 権限 | `[permissions]` allow / deny / `default_mode`（既定 `ask`）、`[tools] enabled` | **可** |
| MCP 連携 | `[mcp]` servers | **可** |
| **Skill 配置** | **該当する機構を持たない**（`[runtime]` に Skill 用のキーは存在せず、サブコマンドにもない） | — |

v0.2 初版は `.agent-cli/skills/` によるプロジェクト単位の Skill 配置を前提として記述していたが、
**この機構は存在しない**。Skill の提供手段は ATHENA 側で定める（第 12.7 節）。
Skill Manager が ATHENA Runtime の責務である（第 6.1 節、第 12.5 節、ATH-SKILL-012）ことと
整合する。

検証基準: 本表は `agent-cli` 0.23.0 の実挙動検証（2026-10-14、ATH-DEV-022）に基づき、
0.29.0 の実挙動検証（2026-10-18）で再確認した。0.28.0 で観測された `serve` 起時の panic
と `spawn` の detached 登録不全は 0.29.0 で解消されている。0.29.0 は legacy な設定キー綴りを
読み込み時に正準綴りへ改名して書き戻すため（第 2.2.4 節）、ATHENA は正準綴りのみを出力する
（ATH-ACLI-018）。

- **ATH-ACLI-001** (MUST) ATHENA は、`agent-cli` の上記機能面を Runtime Adapter の内部でのみ
  利用しなければならない。
- **ATH-ACLI-002** (MUST) `agent-cli` の更新により本節との差分が生じた場合、本節および
  第 14.4 節を本書の改訂として更新しなければならない。差分を未反映のまま Adapter を実装しては
  ならない。

##### 2.2.1 `.agent-cli` ディレクトリの扱い

`.agent-cli` という**ディレクトリ自体を任意の位置へ移す設定は存在しない**。`.agent-cli` は
以下の 2 箇所で固定されている。

| 箇所 | 内容 | 回避手段 |
|---|---|---|
| プロジェクト overlay の探索パス | `./.agent-cli/config.toml`（cwd 基準、固定） | `--config` または `AGENT_CLI_CONFIG` を指定すると**チェーン全体が置換され、この overlay は読まれない** |
| `commands_dir` の既定値 | `.agent-cli/commands` | 設定で任意の絶対パスへ変更可 |

環境変数は `AGENT_CLI_CONFIG` と `AGENT_CLI_ANCESTORS`（peer の系統記録）の 2 つのみであり、
ディレクトリを指定するものはない。

重要なのは、置換が**併合ではない**ことである。cwd に `.agent-cli/config.toml` が存在しても、
`--config` または `AGENT_CLI_CONFIG` を与えた時点で読まれなくなる。したがって ATHENA は
`.agent-cli` の位置を移すのではなく、**常に明示指定して `.agent-cli` に依存しない**構成を採る。

- **ATH-ACLI-007** (MUST) Agent Instance の `.aiprj/` の**書き込み領域**
  （`AI_PRJ_TASKS.md`、`AI_LOG/`）と `.agent-cli/` は、当該 Instance の cwd（ATH-OPS-009）に
  置かなければならない。両者は cwd 相対で解決されるため、これが Instance ごとの分離が成立する
  配置である。`--config` / `AGENT_CLI_CONFIG` による明示指定は不要となる（ATH-ACLI-011）。
- **ATH-ACLI-012** (MUST) `.agent-cli/` は **Agent Instance ごと**に、当該 Instance の cwd 直下へ
  設けなければならない。共有してはならない。`agent-cli` の設定キーのうち以下は Agent 定義
  （第 26.3 節）ごとに異なり、1 組の設定では表現できないためである。

| `AgentDefinition` | `agent-cli` の設定 | 根拠 |
|---|---|---|
| `allowed_tools` | `[tools] enabled`、`[permissions]` allow / deny | ATH-SEC-008（既定で最小）、ATH-SEC-009 |
| `max_delegation_depth` | `[spawn] max_depth` | ATH-HIER-005、ATH-ACLI-009 |
| `model` | `[provider] kind` / `model` | ATH-AGENT-008 |

  共有設定にすると全 Instance が同一の Tool 権限を得るため、`allowed_tools(child) ⊆
  delegable_tools(parent)`（ATH-SEC-009）が Runtime 層で成立しなくなる。
- **ATH-ACLI-013** (MUST) Instance の `.agent-cli/` は、Adapter が
  `<ATHENA_HOME>/var/agent-cli-template/` を原本として**生成**しなければならない。`config.toml`
  は原本の共通部分へ当該 Agent 定義由来の値を重ねて生成し、`commands/` は原本から複製し、
  `persona.md` は `<ATHENA_HOME>/personas/` の該当 Persona から配置する。
  Instance ごとに `install.sh` を実行してはならない（ATH-AIPRJ-006、ATH-AIPRJ-027）。
  Instance は PlanStep ごとかつ試行ごとに生成されるため（ATH-SCHED-009、ATH-FAIL-007）、
  `install.sh` の実行は「実行のたびの再インストール」に当たる。生成は冪等でなければならない。
- **ATH-ACLI-014** (MUST) `.agent-cli/` 配下には、`agent-cli` が**実際に読み取る**ファイルのみを
  置かなければならない。v1 においてそれは `config.toml`、`commands/`、`persona_file` が指す
  Persona の 3 種である（第 2.2 節）。

| ファイル | `agent-cli` の読み取り経路 | 配置 |
|---|---|---|
| `config.toml` | cwd の overlay（第 2.2.1 節） | `.agent-cli/` |
| `commands/` | `[runtime] commands_dir`（slash command としては `run` 専用。IPC 経路では Agent が絶対パスで読み取る手順ファイルとして用いる：第 2.2.3 節、ATH-ACLI-017） | `.agent-cli/` |
| Persona | `[runtime] persona_file` / `--persona` | `.agent-cli/persona.md` |
| Skill の実体 | **読み取り経路が存在しない**（第 2.2 節） | `.agent-cli/` の**外**（Workspace 直下の `skills/`） |

- **ATH-ACLI-015** (MUST NOT) Skill の実体を `.agent-cli/skills/` へ置いてはならない。`agent-cli`
  は Skill 配置の機構を持たないため（第 2.2 節）、当該パスは実在しない機構を示唆する。
  Skill の実体は ATHENA 側の素材として Workspace 直下に置く（ATH-SKILL-017）。
- **ATH-ACLI-011** (MAY) `--config` / `AGENT_CLI_CONFIG` による明示指定を併用してよい。ただし
  指定した場合、cwd の `.agent-cli/config.toml` は**併合されず読まれない**。したがって指定する
  設定は基底設定の内容を含んだ完全な形としなければならない。`persona_file` は cwd 相対でも
  絶対パスでも与えられる。
- **ATH-ACLI-008** (MUST) Core Agent の起動には `spawn` を用いなければならない。起動元
  （Core Host）より長命である必要があるためである（ATH-CORE-020）。停止は `stop` を用い、
  強制終了（SIGKILL）は `timeout.stop` 超過時に限る（ATH-FAIL-015）。

##### 2.2.2 ATHENA の規定と重複する設定

`agent-cli` の設定には、ATHENA が第 18 章・第 13 章で規定する上限と**同一の対象を制限する**
項目がある。二重に強制されるため、値を整合させなければならない。

| `agent-cli` | 既定 | ATHENA の対応する規定 | ATHENA の既定 |
|---|---|---|---|
| `[spawn] max_depth` | 2 | 委任深さ上限（ATH-HIER-005） | 4 |
| `[spawn] max_children` | 4 | `scheduler.max_instances_per_task`（ATH-SCHED-009） | 4 |
| `[history] enabled` | **false** | 短期記憶が Agent Process 側に存在する前提（ATH-MEM-010） | 有効であることが前提 |
| `[history] max_context_tokens` | 24000 | `memory.short_term_max_tokens`（ATH-MEM-009） | 32000 |
| `[permissions]` allow / deny / `default_mode` | `ask` | Tool 権限とリスク分類（第 29.3 節、ATH-SEC-026） | 多層防御の一部 |

- **ATH-ACLI-009** (MUST) Adapter は Agent Instance の起動時に、`[spawn] max_depth` を
  ATHENA の委任深さ上限（ATH-HIER-005）以上、`[spawn] max_children` を
  `scheduler.max_instances_per_task`（ATH-SCHED-009）以上の値で与えなければならない。
  `agent-cli` 側の既定値（`max_depth = 2`）は ATHENA の既定値（4）より小さく、そのままでは
  ATHENA が許可した委任が `agent-cli` 側で拒否される。方針決定は ATHENA 側に置き、
  `agent-cli` 側の上限を下回る制約として機能させてはならない（ATH-SEC-026 と同じ多層防御の
  位置づけ）。
- **ATH-ACLI-010** (MUST) Adapter は `[history] enabled = true` を与え、
  `[history] max_context_tokens` を `memory.short_term_max_tokens` と一致させなければならない。
  `agent-cli` の既定は `enabled = false` であり、そのままでは会話文脈が保持されず、
  ATH-MEM-010（短期記憶は Agent Process 側に存在する）の前提が成立しない。

##### 2.2.3 コマンド定義の展開経路

`commands_dir` に置いた定義が slash command として展開されるのは `run`（REPL）に限る。
`send` / `ask` による IPC 経路では、`/<name>` は**展開されず文字列としてそのまま** Agent へ渡る。
`run` は非対話の prompt 指定手段を持たないため、ATHENA が `run` を用いて展開を得ることはできない。

ATHENA の Runtime は Agent を IPC 経路で駆動する（第 14.2 節の `send` / `receive`、ATH-MSG-001）。
したがって Instance の `.agent-cli/commands/` 配下の定義は、slash command としては**実行されない**。
本節は agent-cli 0.23.0 の実挙動検証（ATH-DEV-022、2026-10-14）に基づき、0.29.0 の再検証
（2026-10-18）で同じ挙動——IPC 経路で `/<name>` は展開されず、手順ファイルは絶対パスの
読み取り指示で機能する——を確認した。

- **ATH-ACLI-016** (MUST NOT) ATHENA は、Agent への指示手段として slash command の展開に依存して
  はならない。`commands_dir` 配下の定義を `/<name>` の送信によって起動する設計を採ってはならない。
- **ATH-ACLI-017** (MUST) `commands_dir` 配下に置く定義は、Agent が**絶対パスで読み取る手順
  ファイル**として扱わなければならない。Persona 本文および委任の `instruction` が、当該手順
  ファイルを絶対パスで明示的に指示する（ATH-AIPRJ-028）。

##### 2.2.4 設定キーの正準綴りと制御表面の安定性

`agent-cli` 0.29.0 は、legacy な設定キー綴り（`[tools.shell]`、`[permissions.defaultMode]`）を
読み込み時に正準綴り（`[tools.bash]`、`[permissions.default_mode]`）へ改名し、当該ファイルへ
書き戻す。正準綴りのみで書かれた設定ファイルは書き戻されず、バイト列として不変である
（2026-10-18 検証）。Instance の `.agent-cli/config.toml` が実行時に書き変わることは、制御表面
チェックサムの検証（ATH-CORE-044）を起点として再起動時のエスカレーションを誘発するため、
生成される設定は決して書き戻しを引き起こしてはならない。

- **ATH-ACLI-018** (MUST) Adapter は、生成する `config.toml` のすべての設定キーを正準綴りで
  出力しなければならない。legacy 綴りを出力してはならない。また `[tools] enabled` と
  `[permissions]` には `agent-cli` 側の正準 Tool 名（`bash`、`read`、`write`、`edit`、`glob`、
  `grep`、`send_to` ほか）を用いなければならない。ATHENA の Tool 名（第 11.1 節）と
  `agent-cli` の Tool 名は語彙が異なるため、Adapter が明示的に射影する（ATH-ACLI-001 の
  内部詳細）。射影対象外の Tool（`spawn` ほか peer 操作系）は排他的に無効化する
  （ATH-HIER-011、ATH-MSG-001）。

#### 2.3 aiprj の前提インターフェース

```text
.aiprj/instructions.md               作業指示（人間が記述する単一の情報源）
.aiprj/AI_PRJ_REQUIREMENTS.md        要求文書
.aiprj/AI_PRJ_DESIGN.md              設計文書
.aiprj/AI_PRJ_TASKS.md               タスク一覧
.aiprj/AI_LOG/YYYY-MM-DD_NNN.md      作業ログ
.agent-cli/                          Agent CLI 用設定・コマンド
install.sh                           対象ディレクトリへの展開スクリプト
運用コマンド                         /setup_ai /ai /update_ai /next_ai /close_ai
```

上記は `install.sh` が対象ディレクトリへ展開する構成である。`.aiprj/` と `.agent-cli/` の
**両方**が作られる点に注意する。

ATHENA はこの展開を 2 つの階層で行う（第 4.3.4 節、ATH-AIPRJ-024）。

| 展開先 | 用途 | 展開の契機 |
|---|---|---|
| `<PROJECT_ROOT>` | プロジェクトの作業規約と 3 文書。全 Agent が読む | `athena init`（ATH-CLI-009） |
| `<ATHENA_HOME>/var/agent-cli-template/` | Instance の `.agent-cli/` を生成する原本 | `athena init`（ATH-ACLI-013） |

**展開はこの 2 箇所に限る。** Agent Instance ごとの展開を行ってはならない（ATH-AIPRJ-005、
ATH-AIPRJ-027）。Instance は PlanStep ごとかつ試行ごとに生成されるため、Instance ごとの展開は
`ATH-AIPRJ-006`（実行のたびの再インストールの禁止）に反する。

`aiprj` には配置先を指定する設定機構がなく、`.aiprj/` は cwd 相対で解決される。そこで
**書き込み対象のみ**を Instance の cwd へ置き（`AI_PRJ_TASKS.md`、`AI_LOG/`）、規約と 3 文書は
コマンド定義の中で `<PROJECT_ROOT>/.aiprj/` への絶対パスとして参照する
（ATH-AIPRJ-028、ATH-OPS-009）。

`.agent-cli/` は Instance ごとに設ける（ATH-ACLI-012）。Tool 権限・委任深さ・model が Agent
定義ごとに異なるためである。ただし原本からの**生成**とし、`install.sh` を Instance ごとに
実行しない（ATH-ACLI-013）。

詳細な運用規約は第 36 章で定義する。

#### 2.4 layers の採用範囲

`layers` から採用するのは**階層型 Agent モデルの思想**（Producer → Director → Lead → Worker、
Agent間 IPC、Persona による役割定義）である。実装コード、ディレクトリ構成、API をそのまま
継承する要求ではない。

- **ATH-HIER-001** (MUST) ATHENA は Producer / Director / Lead / Worker の 4 階層を基本モデルと
  して採用しなければならない。階層の具体的構成は Task ごとに動的に決定する（第 8 章）。

---

### 3. 用語集

本書で使用する用語を定義する。

| 英語表記 | 日本語 | 定義 |
|---|---|---|
| ATHENA | ATHENA | 本アーキテクチャの名称。Core、Runtime、API、CLI、Studio の総体。 |
| ATHENA Core | Core | オーケストレーション層。Core Agent と支援モジュール群からなる。 |
| ATHENA Core Agent | Core Agent | 最上位の AI Agent。理解・計画・判断・委任・観測・評価・再計画を担う Executive Agent。専用の `agent-cli` インスタンス上で動作する（第 7.5 節）。 |
| Core Host | Core Host | `athena-core` のうち決定論的な部分。Core Agent の起動・監視・再起動、状態遷移、Event 発行、権限強制、承認仲介を担い、**判断を行わない**。 |
| Core Agent Process | Core Agent Process | Core Agent が動作する `agent-cli` インスタンス（instance #0）。下位 Agent のインスタンスとは分離される。 |
| Agent Process | Agent Process | 1 つの Agent Instance を実体化する `agent-cli` プロセス。信頼境界の外側に置く（ATH-SEC-001）。 |
| Core Agent Session | Core Agent Session | Core Host のモジュール。Core Agent Instance の起動・監視・再起動を担う（第 7.5 節）。 |
| Core Control Protocol | Core 制御プロトコル | Core Agent Process と Core Host の間の要求・応答形式（第 7.5.5 節）。 |
| Policy Manager | Policy Manager | Core Host のモジュール。リスク分類と権限検査を行う（第 29 章）。 |
| Producer Agent | Producer | Core Agent の階層上の呼称。階層最上位の Agent。 |
| Director Agent | Director | 領域単位の責任を持つ Agent。Lead へ委任する。 |
| Lead Agent | Lead | 専門領域のタスクを管理する Agent。Worker へ委任する。 |
| Worker Agent | Worker | 具体的な実装・調査・検証作業を実行する Agent。 |
| Agent | Agent | 目的を理解し判断して作業を遂行する論理的な AI 実行主体。 |
| Agent Definition | Agent 定義 | Agent の静的定義（ID、role、capabilities、persona、権限、runtime）。 |
| Agent Instance | Agent Instance | Agent 定義から生成された実行中の個体。状態とライフサイクルを持つ。 |
| Persona | Persona | Agent の役割・責務・権限・専門知識・行動規範を定義する文書。 |
| Task | Task | 利用者から要求された仕事。SubTask へ分解できる。 |
| SubTask | SubTask | Task を分解した下位 Task。親 Task への参照を持つ。 |
| Plan | Plan | Core Agent が生成する実行計画。実行中に変更可能。 |
| PlanStep | PlanStep | Plan を構成する一単位の行動。依存関係を持つ。 |
| Workflow | Workflow | Plan の依存関係表現（DAG）。固定の実行モデルではない。 |
| Runtime | Runtime | Agent の実行環境を抽象化する層。 |
| Runtime Adapter | Runtime Adapter | 特定実行基盤（agent-cli 等）へ接続する Runtime 実装。 |
| Runtime Instance | Runtime Instance | Runtime が管理する実行単位（プロセス、コンテナ等）。 |
| Tool | Tool | Agent が外部システムへ作用するための具体的な操作手段。 |
| Skill | Skill | 特定作業を遂行するための再利用可能な専門能力のパッケージ。 |
| Skill Catalog | Skill Catalog | 利用可能な Skill のメタデータ集合。Core Agent が参照する。 |
| Skill Registry | Skill Registry | Skill の登録・更新・削除を管理する機構。 |
| Skill Resolver | Skill Resolver | Agent 起動時に Skill の依存・版・権限を解決する機構。 |
| Skill Loader | Skill Loader | 解決済み Skill を Agent Workspace へ配置する機構。 |
| Skill Set | Skill Set | ある Agent Instance が実行時に保持する Skill と版の集合。 |
| Message | Message | Agent間通信の単位。instruction / report などの型を持つ。 |
| Event | Event | ATHENA の状態変化を記録する不変の事実。 |
| Result | Result | Agent が返す作業結果。status、output、artifacts 等を持つ。 |
| Artifact | Artifact | Result に付随する成果物（ファイル、差分、レポート）。`base` と相対パスで記録する（ATH-RESULT-002）。 |
| Result Store | Result Store | ATHENA が保管する Artifact の実体の置き場。`<ATHENA_HOME>/var/artifacts/<result_id>/`（ATH-RESULT-002、ATH-RESULT-004）。 |
| Evaluation | Evaluation | Result を最終結果として受理するか判断する処理。 |
| Verification | Verification | Test / Lint / Build / Review など、客観的手段による検証。 |
| Re-plan | 再計画 | 観測・評価結果に基づき Plan を変更する処理。 |
| Approval | Approval | 高リスク操作に対する人間の承認。 |
| Human-in-the-Loop | HITL | 人間の承認・介入を実行経路へ組み込む設計。 |
| Short Term Memory | 短期記憶 | 現在の会話・Task の文脈。 |
| Working Memory | 作業記憶 | Plan、Agent 報告、現在状態。 |
| Long Term Memory | 長期記憶 | プロジェクト知識、履歴、学習済み嗜好。 |
| var | var | `<ATHENA_HOME>/var/`。ATHENA の実行時データ領域。`athena.db`、登録済み Skill、Result Store、`agent-cli` の原本、各 Workspace を含む。版管理対象外（ATH-DEV-020）。実行可能コードは置かない（ATH-OPS-011）。 |
| ATHENA_HOME | ATHENA_HOME | ATHENA が使用するファイルの格納先。`<PROJECT_ROOT>/.athena`（第 4.3.4 節）。設定キー `athena.home` に対応し、カレントディレクトリから上方への探索で解決される（ATH-CFG-010）。 |
| PROJECT_ROOT | プロジェクトルート | ATHENA をインストールした対象プロジェクトのディレクトリ。`ATHENA_HOME` の親（第 4.3.4 節）。 |
| Control Surface | 制御面 | Agent 自身の振る舞いを規定するファイル群（Instance の `.agent-cli/`（`config.toml` / `commands/` / `persona.md`）と `skills/`、`<PROJECT_ROOT>/.aiprj/instructions.md` と `rules/`）。Core Host が配置し、Agent からは読み取り専用とする（ATH-CORE-043）。 |
| Delegation Depth | 委任深さ | 階層委任の連鎖の段数。上限を超える委任要求は `PolicyError` として拒否する（ATH-HIER-005）。 |
| Idempotency Key | 冪等キー | 同一要求の二重実行を防ぐ識別子。委任と Tool 実行では `plan_step_id` + `attempt` とする（ATH-CORE-035）。 |
| Heartbeat | heartbeat | Core Host と Agent Instance の間の生存確認。既定 60 秒途絶した Instance は自己終了する（ATH-CORE-047）。 |
| cwd | カレントディレクトリ | current working directory。プロセス起動時に設定される基準ディレクトリ。相対パスの解決基準となる。`agent-cli` の `.agent-cli/` と `aiprj` の `.aiprj/` はいずれも cwd を基準に解決される（第 2.2.1 節、ATH-OPS-009）。 |
| Workspace | Workspace | Agent Instance ごとのディレクトリ。当該 Instance の cwd となり、書き込み領域（`.aiprj/AI_PRJ_TASKS.md`、`.aiprj/AI_LOG/`）と Persona・Skill を含む（第 4.3.4 節）。作業対象とは別。 |
| Work Target | 作業対象 | Agent が読み書きする対象、すなわちプロジェクトの source / artifacts（`<PROJECT_ROOT>` 配下）。Workspace の外にあるため絶対パスで与える（ATH-OPS-010）。 |
| aiprj | aiprj | Agent の AI 作業規約・プロジェクト管理基盤。 |
| agent-cli | agent-cli | 初期 Agent 実行基盤。 |
| ATHENA API | Core API | Core への統合 API。CLI / Studio / 外部アプリが利用する。 |
| ATHENA CLI | CLI | コマンドライン UI。`athena chat` を第一級機能とする。 |
| ATHENA Studio | Studio | ブラウザ UI。 |
| ATHENA Hub | Hub | Agent / Tool / Model / Skill の Registry。将来拡張。 |
| ATHENA SDK | SDK | 外部 Agent 開発 API。将来拡張。 |

- **ATH-GEN-008** (MUST) 本書および ATHENA の実装・ドキュメントは、本章で定義した用語を
  一貫して使用しなければならない。同一概念に別名を与えてはならない。

---

### 4. コンセプトと設計原則

#### 4.1 ATHENA の定義

ATHENA は、

> **人間の指示を理解し、目的を解釈し、必要な AI Agent を組織化し、Tool と Runtime を使って
> 実際の作業を遂行し、結果を評価しながら計画を変更する Agent Operating Architecture**

である。

したがって ATHENA の中心は Scheduler ではない。**中心は ATHENA Core Agent である。**

```text
Human
  ↓
Understand → Plan → Decide → Delegate → Execute → Observe → Evaluate → Re-plan → Complete
```

Task / Workflow / Scheduler などの機械的機能は、Core Agent を支える**実行基盤**として実装する。

- **ATH-CORE-001** (MUST) ATHENA Core は AI Agent として実装しなければならない。
- **ATH-CORE-002** (MUST NOT) ATHENA Core の意思決定を、固定の Workflow Engine や固定ロジックへ
  置き換えてはならない。
- **ATH-CORE-003** (MUST) 人間の自然言語指示を最上位入力として受理しなければならない。

#### 4.2 5 層概念モデル

```text
+--------------------------------------------------+
|                      User                        |
+----------------------+---------------------------+
                       |
             +---------v---------+
             | CLI / Studio      |  Presentation
             +---------+---------+
                       |
             +---------v---------+
             | Core API          |  Interface
             +---------+---------+
                       |
             +---------v---------+
             | ATHENA Core       |  Orchestration / Decision
             +---------+---------+
                       |
             +---------v---------+
             | Runtime           |  Execution abstraction
             +---------+---------+
                       |
             +---------v---------+
             | Agent             |  Intelligence
             +-------------------+
```

この図が表す不変条件は以下である。

> **ATHENA Core が「何をするか」を決め、Runtime が「どう実行するか」を抽象化し、Agent が
> 「実際に考えて処理する」。**

本節は概念上の層構造であり、プロセス配置を表すものではない。Core Agent の判断も Agent 層の
Agent Process（`agent-cli` instance #0）で行われる。物理配置は第 4.3 節、Core Agent の実行形態は
第 7.5 節、制御経路は第 6.2 節を参照する。

#### 4.3 構成例（実動作するアプリケーション）

第 4.2 節の概念モデルは層の責務を示すものであり、そのままでは実行単位が見えない。本節は
概念モデルの各層が**実際にどのアプリケーション・プロセス・ファイルとして動作するか**の構成例を
示す。本節は例示であり、名称・ポート・配置の細部は規範的要求ではない。規範的なリポジトリ構成は
第 32 章、永続化は第 27 章、設定は第 28 章で定義する。

##### 4.3.1 概念層とアプリケーションの対応

```text
概念層                     実動作するアプリケーション / 実体
-----------------------------------------------------------------------
User                       ブラウザ / 端末
   |
Presentation               athena-studio    (Rust / WebAssembly, http://127.0.0.1:5173)
                           athena           (CLI バイナリ, `athena chat`)
   |
Interface                  athena-api       (Rust HTTP サーバ, http://127.0.0.1:8080)
                                            /api/v1/..., /api/v1/events/stream (SSE)
   |
Orchestration              athena-core      (同一プロセス内の crate)
                                            Core Host: 監督 / 状態 / Event / Queue / 権限強制
                                            ※判断は行わない（第 7.5 節）
                           athena.db        (`redb`: Task / Plan / Event / Result)
   |
Execution abstraction      athena-runtime-agent-cli  (Runtime Adapter)
                           （プロセス / IPC 管理は athena-runtime-agent-cli 内の
                             Process Manager が担う。別プロセスを設けない）
   |
Intelligence               agent-cli #0     Core Agent（Producer persona・常駐）
                           agent-cli #1..N  Director / Lead / Worker（Agent Instance ごとに 1 個）
                           → Claude / Codex / Ollama (LLM Provider)
```

##### 4.3.2 実行単位の一覧

| アプリケーション | 種別 | 実装 | 役割 | 既定の待受 |
|---|---|---|---|---|
| `athena-studio` | Web アプリ | Rust / WebAssembly | Studio UI（第 25 章） | 5173 |
| `athena` | CLI 実行ファイル | Rust | CLI UI（第 24 章） | — |
| `athena-api` | 常駐プロセス | Rust | Core API（第 22 章）、Event Stream | 8080 |
| `athena-core` | crate | Rust | Core Host。Core Agent の監督と支援モジュール（第 6 章、第 7.5 節） | — |
| `athena-runtime-agent-cli` | crate | Rust | agent-cli Adapter と Process Manager（第 14 章） | — |
| `agent-cli` #0 | 外部プロセス | 外部依存 | Core Agent の実行（常駐、Producer persona、専用 Workspace） | — |
| `agent-cli` #1..N | 外部プロセス | 外部依存 | 下位 Agent（Director / Lead / Worker）の実行。#0 とは分離 | — |
| `athena.db` | ファイル | `redb` | 実行状態の永続化（第 27 章。SQLite は使用しない — ATH-PERS-004） | — |

**実装の配置。** 上表の実行単位は `.athena/` の**外**に置く。`.athena/` には実行可能コードを
含めない（第 4.3.4 節は設定・定義・実行時データのみで構成される）。

| 実行単位 | 配置 | プロジェクト単位 / 共有 |
|---|---|---|
| `athena`（CLI） | PATH 上、またはプロジェクト配下へ配置した実行ファイル | 実体は共有可。対象プロジェクトは `.athena/` の上方探索で決まる（ATH-CFG-010） |
| `athena-api` | 同上 | 実体は共有可。**プロセスはプロジェクトごと**（ATH-OPS-007。`athena.api.port` を相違させる） |
| `athena-core` | `athena-api` のバイナリへ静的リンク | 同上。独立して配置しない（第 4.3.3 節） |
| `athena-runtime-agent-cli` | 同上 | 同上 |
| `athena-studio` | WebAssembly の静的配信または開発サーバ | 共有可。接続先の API URL で担当プロジェクトが決まる |
| `agent-cli` | 外部依存。PATH 上 | 実体は共有。インスタンスはプロジェクトかつ Agent Instance ごと（ATH-RT-012） |

- **ATH-OPS-011** (MUST NOT) `<ATHENA_HOME>`（`.athena/`）に実行可能コードを置いてはならない。
  同ディレクトリの内容は設定・定義・実行時データに限る（ATH-OPS-005、ATH-OPS-006）。理由は
  実装の**所有者の分離**である。コードの取得・完全性検証・更新はパッケージマネージャの責務で
  あり、`.athena/` へ取り込むとその機構を ATHENA 側で再実装することになる。加えて `athena init`
  とパッケージマネージャが同一ディレクトリへ書き込む構造となり、`ATH-DEV-020` の版管理境界
  （`var/` の 1 行）も保てない。
- **ATH-OPS-012** (MUST) 実装の解決は、PATH 上の実行ファイル、またはプロジェクトの
  パッケージマネージャによる導入のいずれかとしなければならない。**プロジェクト単位の配置は
  後者で成立する**（例: `cargo install --path` によるプロジェクト配下への配置）。版の固定が
  必要な場合はこれを用いる。
  いずれの場合も**プロセスと実行時データはプロジェクトごとに分離**しなければならない
  （ATH-OPS-007）。実行ファイルの位置から担当プロジェクトを導出してはならない
  （ATH-CFG-010）。グローバル配置と併用した場合でも、担当プロジェクトは常に `.athena/` の
  上方探索で決まる。

##### 4.3.3 プロセス境界と層の関係

`athena-api` は **ATHENA Core ではない**。両者の関係は以下とする。

| 観点 | `athena-api` | `athena-core` |
|---|---|---|
| 概念層 | Interface（第 4.2 節の Core API 層） | Orchestration（ATHENA Core 層） |
| 責務 | 認証・認可、要求検証、公開契約の提供、Event 配信 | Core Agent の起動・監視・再起動、状態遷移、Queue、権限強制、承認仲介、記憶の保存 |
| 実体 | OS プロセス（常駐） | crate（`athena-api` へ静的リンク） |
| 判断の所在 | 持たない | 持たない。判断は `agent-cli` #0 上の Core Agent Process が行う（第 7.5 節） |
| 置換可能性 | 別の公開契約（将来の gRPC 等）へ差し替え可能 | ATHENA の中核であり差し替え対象ではない |

初期実装（第 33 章 Phase 1 から Phase 3）では、`athena-core` と
`athena-runtime-agent-cli` は `athena-api` プロセスの**内部にライブラリとしてロードされて動作する**。
したがって、

```text
OS プロセス     : athena-api 1 個
その内部の層    : Core API 層（athena-api）→ Orchestration 層（athena-core）→ Runtime 層（runtimes/*）
```

となる。プロセスが 1 個であることと、層が同一であることは異なる。分離可能な構造
（第 32.2 節の依存方向）を維持しつつ、初期段階ではプロセス分割を行わない（ATH-DEV-008）。

- **ATH-OPS-001** (MUST) `athena-api` と `athena-core` を別パッケージとして実装し、`athena-api`
  から `athena-core` への一方向依存を維持しなければならない。同一プロセスで動作することを理由に
  両者の責務を混在させてはならない。
- **ATH-OPS-002** (MUST NOT) `athena-core` は HTTP 層の概念（要求、応答、状態コード、ヘッダ）へ
  依存してはならない。
- **ATH-OPS-003** (MAY) 将来、`athena-core` を独立プロセスとして分離し、`athena-api` から内部
  RPC で接続する構成を採用してよい。この変更は Core API の公開契約（第 22 章）に影響してはならない。
- **ATH-OPS-004** (MUST) v1 は**単一利用者・単一ホスト**運用を前提とする（第 4.3.4 節）。
  利用者ごとの Workspace 隔離、利用者単位の資源上限、資源の所有者による絞り込みを v1 の
  要件としてはならない。多利用者運用を導入する場合は、本節のプロセス構成、第 29.2 節の
  認証・認可（ATH-SEC-027）、および第 13 章の Memory の scope を同時に改訂しなければならない。

##### 4.3.4 プロジェクト単位の構成

ATHENA は**プロジェクト単位で構築する**。インストール先は対象プロジェクトのディレクトリであり、
ATHENA が使用するファイルはその直下の `.athena/` に収める。`aiprj`（`.aiprj/`）および
`agent-cli`（`.agent-cli/`）と同じ方式である。利用者のホームディレクトリへ展開しない。

```text
<PROJECT_ROOT>                          athena をインストールしたディレクトリ
|                                        = ATHENA が担当するプロジェクト
|
+-- .athena/                             ATHENA が使用するファイル（= ATHENA_HOME）
|     |
|     +-- config.yaml                    設定（第 28 章）
|     +-- agents/                        本プロジェクトの Agent 定義
|     +-- personas/                      Persona（Agent からは読み取り専用：ATH-CORE-043）
|     +-- skills/                        Skill Package のソース（`skills.local_dir`）
|     |
|     +-- var/                           実行時データ。ATHENA のみが書き込む
|           |
|           +-- athena.db                `redb`: Task / Plan / Event / Result（第 27 章）
|           +-- skills/                  登録済み Skill（`skills.dir`、第 12.4 節）
|           +-- artifacts/               Result Store。成果物の実体（ATH-RESULT-002）
|                 +-- <result_id>/
|           |
|           +-- agent-cli-template/      Instance の .agent-cli/ を生成する原本
|           |     +-- config.toml        Agent 定義に依らない共通部分
|           |     +-- commands/          コマンド定義の原本（ATH-AIPRJ-028）
|           |
|           +-- core-agent/              Core Agent Workspace。cwd（ATH-CORE-023）
|           |     +-- .aiprj/            Core Agent の書き込み領域
|           |     |     +-- AI_PRJ_TASKS.md
|           |     |     +-- AI_LOG/
|           |     +-- .agent-cli/        agent-cli が読む入力のみ（cwd 相対）
|           |     |     +-- config.toml  Core Agent 定義から生成
|           |     |     +-- commands/
|           |     |     +-- persona.md   persona_file で指す（Producer persona）
|           |     +-- skills/            ATHENA が用意する素材。agent-cli は読まない
|           |
|           +-- instances/               下位 Agent Instance ごとの領域
|                 +-- <instance_id>/      Agent Workspace。agent-cli の cwd
|                 |     |                  （ATH-AIPRJ-002、ATH-AIPRJ-024）
|                 |     +-- .aiprj/      書き込み領域のみ。aiprj 全体を展開しない
|                 |     |     +-- AI_PRJ_TASKS.md   この Instance の担当範囲
|                 |     |     +-- AI_LOG/           YYYY-MM-DD_NNN.md
|                 |     +-- .agent-cli/  **Instance ごと**（ATH-ACLI-012）
|                 |     |     +-- config.toml  当該 Agent 定義から生成
|                 |     |     +-- commands/
|                 |     |     +-- persona.md   persona_file で指す
|                 |     +-- skills/      ATHENA が用意する素材
|                 |                       agent-cli は読まない。`.agent-cli/` に置かない
|                 +-- ...
|
+-- .aiprj/                              プロジェクトの作業規約と 3 文書（第 36 章）
|     +-- instructions.md                 全 Agent が読む。Agent は書き込まない
|     +-- AI_PRJ_REQUIREMENTS.md          （ATH-AIPRJ-025）
|     +-- AI_PRJ_DESIGN.md
|     +-- AI_PRJ_TASKS.md
|     +-- AI_LOG/
|
+-- .agent-cli/                          install.sh が .aiprj/ と同時に作る（第 2.3 節）
|     +-- config.toml                     利用者が直接 agent-cli を使う場合の設定
|     +-- commands/                       ATHENA は参照しない（Instance ごとの cwd を使う）
|
+-- source / artifacts                   プロジェクトの作業対象と成果物
```

`.athena/` の内側は、版管理する設定・定義（`config.yaml`、`agents/`、`personas/`、`skills/`）と、
版管理しない実行時データ（`var/`）に分かれる。`skills/` と `var/skills/` は同名だが別物であり、
前者は Skill Package のソース、後者は登録済み Skill（`skills.dir`）である（ATH-DEV-020）。

**Agent Workspace は Instance ごとのディレクトリであり、作業対象とは別である。**

`agent-cli` と `aiprj` の規約は、いずれも**カレントディレクトリ相対**で解決される。

| 規約 | 解決基準 | 根拠 |
|---|---|---|
| `.agent-cli/config.toml`（プロジェクト overlay） | cwd | 第 2.2.1 節 |
| `[runtime] commands_dir`（既定 `.agent-cli/commands`） | cwd（絶対パス指定も可） | 設定の記載「Relative to the current working directory」 |
| `.aiprj/`（instructions、3 文書、`AI_LOG/`） | cwd | `agent-cli` は `aiprj` を解釈せず、コマンド定義中の `@.aiprj/...` が cwd 相対で解決される |

したがって Instance ごとの**書き込み領域**を機能させるには、**cwd を当該 Instance の
ディレクトリに置く**必要がある。cwd を `<PROJECT_ROOT>` に置くと、`.aiprj/` はプロジェクトの
ものに解決され、`ATH-AIPRJ-024` の Instance 単位の分離が成立しない。

`.agent-cli/` も Instance ごとに設ける。Tool 権限（`[tools]` / `[permissions]`）、委任深さ
（`[spawn] max_depth`）、model が Agent 定義ごとに異なり、1 組の設定では表現できないためである
（ATH-ACLI-012）。Instance 数が PlanStep 数 × 試行回数に比例して増えるため、`install.sh` では
なく**原本からの生成**とする（ATH-ACLI-013、ATH-AIPRJ-006）。

```text
cwd = Agent Workspace                 .athena/var/instances/<instance_id>/
  +-- .aiprj/AI_PRJ_TASKS.md           書き込み（cwd 相対で解決）
  +-- .aiprj/AI_LOG/                   書き込み（cwd 相対で解決）
  +-- .agent-cli/config.toml           当該 Agent 定義から生成（cwd 相対で解決）
  +-- .agent-cli/commands/             原本から複製（cwd 相対で解決）
  +-- .agent-cli/persona.md            persona_file で指す
  +-- skills/                          ATHENA の素材。agent-cli は読まない

原本（生成元。直接は参照されない）     .athena/var/agent-cli-template/

作業対象                              <PROJECT_ROOT> の source / artifacts
  プロジェクトの作業規約と 3 文書      <PROJECT_ROOT>/.aiprj/（読み取り。絶対パス）
```

**作業対象は cwd にないため、明示的に与える。** Agent Workspace が Instance のディレクトリで
あることの代償として、プロジェクトの source / artifacts は cwd 外になる。以下で与える。

| 手段 | 内容 |
|---|---|
| `filesystem_scope`（第 29.3 節） | `<PROJECT_ROOT>` の作業対象パスを読み書き範囲へ含める |
| Persona と Instance のコマンド定義 | 作業対象の絶対パスを明記する。Adapter が生成時に埋め込む |
| シェル実行 | 作業対象で実行する必要がある場合、コマンド側で明示的に移動する。cwd の暗黙の前提に依存してはならない |

プロセスと待受は以下となる。いずれも `.athena/` を基準に動作する。

```text
プロジェクトのホスト
|
+-- athena-api            :8080         常駐。athena-core (Core Host) を内包
|                                        .athena/var/athena.db を開く
|
+-- agent-cli #0                        Core Agent（常駐・別 OS プロセス。spawn で起動）
|                                        cwd: .athena/var/core-agent
|
+-- agent-cli #1..N                     Director / Lead / Worker（Agent Instance ごとに 1 個）
|                                        cwd: .athena/var/instances/<instance_id>
|                                        作業対象は絶対パスで与える（ATH-OPS-010）
|
+-- athena-studio         :5173         athena-api へ接続
|
+-- athena                CLI            .athena/ を探索して接続先を決定（ATH-CFG-010）
```

- **ATH-OPS-005** (MUST) ATHENA が使用するファイルは、インストール先プロジェクト直下の
  `.athena/` に収めなければならない。利用者のホームディレクトリ、および
  `<PROJECT_ROOT>/.athena/` の外側へ、ATHENA 用のディレクトリを作成してはならない。
- **ATH-OPS-006** (MUST) 可変の実行時データは `<PROJECT_ROOT>/.athena/var/` 配下に限定しな
  ければならない。`config.yaml`、`agents/`、`personas/`、`skills/` へ実行時に書き込んでは
  ならない。これにより、版管理対象と実行時データの境界が 1 本のパスで定まる。
- **ATH-OPS-007** (MUST) 1 つの ATHENA インスタンスは 1 つのプロジェクトのみを担当しなければ
  ならない。複数プロジェクトを 1 インスタンスで扱ってはならない。同一ホストで複数プロジェクトを
  同時に稼働させる場合、`athena.api.port` をプロジェクトごとに相違させなければならない
  （`athena.db` は `.athena/var/` 配下のため衝突しない）。
- **ATH-OPS-008** (MUST NOT) 下位 Agent の `filesystem_scope`（第 29.3 節）に
  `<PROJECT_ROOT>/.athena/` を含めてはならない。ただし**当該 Instance 自身のディレクトリ**
  （`<ATHENA_HOME>/var/instances/<instance_id>/`）は例外とする。Agent は自身の `.aiprj/` へ
  `AI_PRJ_TASKS.md` と `AI_LOG/` を書き込む必要があるためである（ATH-AIPRJ-009、
  ATH-AIPRJ-010）。当該ディレクトリ内でも制御面（`.agent-cli/`、`skills/`）は
  除外する（ATH-CORE-043）。他 Instance のディレクトリ、`core-agent/`、`athena.db`、
  `config.yaml`、`agents/`、`personas/`、`skills/` を含めてはならない
  （ATH-AIPRJ-018、ATH-CORE-027）。
- **ATH-OPS-009** (MUST) 下位 Agent Instance の cwd は、当該 Instance のディレクトリ
  （`<ATHENA_HOME>/var/instances/<instance_id>/`）としなければならない。Core Agent の cwd は
  `<ATHENA_HOME>/var/core-agent/` とする。cwd を `<PROJECT_ROOT>` としてはならない。
  `aiprj` および `agent-cli` の規約が cwd 相対で解決されるため、cwd を共有すると Instance 単位の
  分離（ATH-AIPRJ-024）が成立しない。
- **ATH-OPS-010** (MUST) 作業対象（`<PROJECT_ROOT>` の source / artifacts）は、
  `filesystem_scope` と Persona・コマンド定義中の絶対パスによって与えなければならない。
  cwd からの相対パスで作業対象を参照する記述を生成してはならない。

##### 4.3.5 起動と接続の例

```bash
# 0. プロジェクトへ ATHENA を導入（.athena/ を生成）
cd /work/fpga-project
athena init

# 1. Core API（Core Host を内包。起動時に agent-cli #0 の Core Agent を立ち上げる）を起動
#    .athena/ は cwd から上方へ探索して解決される（ATH-CFG-010）
athena-api

# 2. Studio を起動（開発時）
cargo run -p athena-studio          # http://127.0.0.1:5173

# 3. CLI から対話
athena chat --api http://127.0.0.1:8080
```

```text
athena chat ──► POST /api/v1/tasks ──► Core Agent
                                          |
            ◄── GET /api/v1/events/stream ─┘ (SSE: TaskStarted, AgentStarted, ...)
```

##### 4.3.6 FPGA 案件での具体例

```text
Human: 「Arria 10 のタイミング違反の原因を調査してください」
   |
athena (CLI)
   |
athena-api  →  athena-core (Core Host)
   |               |
   |               └──► agent-cli #0 (Core Agent) が判断
   |                      +-- Plan: 制約確認 → STA 実行 → 原因特定 → 報告
   |                      +-- Hierarchy: fpga-director → timing-lead → sta-worker
   |                      +-- Skill: fpga-timing-analysis@1.2.0, sdc-analysis@1.0.1
   |                    （Core Host は上記を検査・記録するのみ：ATH-CORE-022）
   |
athena-runtime-agent-cli
   |
Process Manager ─► agent-cli (timing-lead)    cwd: .athena/var/instances/<id>/
   |                  |
   |                  +-- Skill: fpga-timing-analysis（Instance の skills/ から提供）
   |                  +-- 作業対象: /work/arria10（絶対パス：ATH-OPS-010）
   |                  +-- .aiprj/AI_PRJ_TASKS.md 更新、AI_LOG 保存
   |                  +-- Tool: shell (quartus_sta), fs_read, fs_write
   |
   +-- agent-cli (sta-worker)                 委任先 Worker
```

#### 4.4 設計原則

| 原則 | 内容 |
|---|---|
| Agent 中心 | 中心は Core Agent であり、Scheduler ではない。 |
| 動的計画 | Plan は実行中に変更できる。固定 Workflow を主役にしない。 |
| 階層委任 | Producer / Director / Lead / Worker による委任を第一級機能とする。 |
| 疎結合 | Core は Agent の実行方法を知らない。Runtime が抽象化する。 |
| 観測可能性 | 状態変化を Event として記録し、常に追跡できる。 |
| 再現性 | 実行履歴、使用 Agent、使用 Skill 版を保存する。 |
| 障害を入力とする | Failure を単純なエラーではなく、次の意思決定の入力として扱う。 |
| 境界付き自律 | 完全自律ではなく、委任深さ制限と承認点を設ける。 |
| 単純性優先 | 分散化・マイクロサービス化より単純性を優先する。 |
| UI 独立性 | CLI / Studio / 外部アプリは同一の Core API を利用する。 |

- **ATH-CORE-004** (MUST) Core は Runtime Interface 経由でのみ Agent を起動しなければならない。
- **ATH-CORE-005** (MUST) Plan は実行中に変更可能でなければならない。
- **ATH-CORE-006** (MUST) Agent が返した Result を無条件に最終結果として扱ってはならない
  （第 19 章）。
- **ATH-CORE-007** (MUST) Agent の失敗は Core Agent の再計画入力として扱わなければならない
  （第 20 章）。

#### 4.5 ATHENA でないもの

```text
ATHENA != 単一 LLM のラッパー
ATHENA != 固定 Workflow Engine
ATHENA != agent-cli のコマンドラッパー
ATHENA != aiprj
ATHENA != Agent Marketplace
```

---

### 5. 制約とポリシー

本章は ATHENA 全体に適用される制約を集約する。

#### 5.1 実装言語ポリシー

- **ATH-POLICY-001** (MUST NOT) ATHENA 本体の実装に Python を使用してはならない。
  `.py`、`pip`、`venv`、FastAPI、Django、Flask、Python 製 Agent Runtime、Python 製 CLI を
  ATHENA 本体の構成要素として採用してはならない。
- **ATH-POLICY-002** (MUST) ATHENA 自身の実装言語は **Rust** としなければならない。すべての
  実行単位（第 4.3.2 節）を Rust で構成する。言語をまたぐ実装分割を設けてはならない。
- **ATH-POLICY-003** (MUST) ブラウザ UI は Rust から WebAssembly へコンパイルして提供しな
  ければならない。UI のために別言語を導入してはならない。
- **ATH-POLICY-004** (MAY) ATHENA が操作対象とするプロジェクトに Python が含まれることは
  許容する。禁止対象は ATHENA 自身の実装である。

技術スタック：

| 構成要素 | 技術 |
|---|---|
| ATHENA Studio | Rust / WebAssembly |
| ATHENA API | Rust（HTTP / SSE） |
| ATHENA Core Agent | Rust |
| Agent Manager / Message / Event | Rust |
| Runtime / Process / IPC | Rust |
| 永続化 | Rust（`redb`） |
| Agent Execution | agent-cli（Rust 実装の外部プロセス） |
| LLM Provider | Claude / Codex / Ollama その他 |

単一言語とする理由は以下である。

| 理由 | 内容 |
|---|---|
| Runtime 境界が同一言語になる | `agent-cli` は Rust 実装である。Runtime Interface の背後が言語境界をまたがないため、型とエラーの表現が一貫する |
| プロセス分割が不要になる | プロセス管理と IPC を別プロセスへ切り出す必要がない。Process Manager は Runtime 層のモジュールとする（第 14 章） |
| 状態遷移の網羅性を型で保証できる | 第 17 章の 5 つの状態機械を列挙型で表現し、未処理の遷移を型検査で検出できる |
| 単一バイナリで配布できる | プロジェクト単位の導入（第 4.3.2 節）でパッケージツリーを持ち込まない |

- **ATH-POLICY-005** (MUST) リポジトリは Cargo workspace 構成とする（第 32 章）。

#### 5.2 モジュール境界ポリシー

| 依存方向 | 可否 |
|---|---|
| Core Host → Runtime Interface | 許可 |
| Core Host → Core Agent Process（Runtime Interface 経由） | 許可 |
| Runtime Adapter → agent-cli | 許可 |
| Core → agent-cli | **禁止**（Core Agent 自身の実行も Runtime Interface 経由） |
| Core Agent Process → 特権操作（Agent 起動 / Tool / 状態遷移） | **Core Host の検査を経る場合のみ許可** |
| Studio / CLI → Core API | 許可 |
| Studio / CLI → Core 内部実装 | **禁止** |
| Skill Manager → agent-cli 内部仕様 | **禁止**（Adapter 経由のみ） |

- **ATH-POLICY-006** (MUST NOT) Core は `agent-cli` 固有のコマンド、内部仕様、ファイル配置へ
  直接依存してはならない。Core Agent 自身が `agent-cli` 上で動作する場合も、その起動と通信は
  Runtime Interface および Message Broker を経由しなければならない（第 7.5 節）。
- **ATH-POLICY-007** (MUST NOT) CLI および Studio は Core の内部実装へ直接アクセスしてはならない。
- **ATH-POLICY-008** (MUST) 上記境界は静的検査によって機械的に強制しなければならない
  （第 5.6 節 ATH-DEV-004、第 32.2 節 ATH-DEV-011）。

#### 5.3 Agent 作業規約ポリシー

- **ATH-POLICY-009** (MUST) ATHENA が実行するすべての Agent は `aiprj` の作業規約を使用しなければ
  ならない。対象には Core Agent 自身を含む（第 36 章）。
- **ATH-POLICY-010** (MUST NOT) Core の判断機能を `aiprj` へ移してはならない。`aiprj` は作業規約・
  プロジェクト管理層であり、Core Agent は Executive Agent である。

#### 5.4 安全性ポリシー

- **ATH-POLICY-011** (MUST) 高リスク操作の実行前に人間の承認を要求しなければならない（第 15 章）。
- **ATH-POLICY-012** (MUST) Skill の権限は Agent の権限を超えてはならない。規範的定義は
  ATH-SEC-012 とする。
- **ATH-POLICY-013** (MUST) 秘密情報を設定ファイル、長期記憶、Event payload、ログへ平文で
  保存してはならない（第 28 章、第 13 章、第 30 章）。

#### 5.5 スコープポリシー

- **ATH-POLICY-014** (MUST NOT) 初期実装において Hub、SDK、Kubernetes、分散 DB を必須構成要素と
  してはならない（第 37 章）。
- **ATH-POLICY-015** (MUST) 最優先の完成対象は Core Agent Loop とする（第 33 章）。
- **ATH-POLICY-016** (SHOULD) 要求が発生するまで Hub / SDK を実装しない。

#### 5.6 AI コーディングエージェントへの実装規則

本書に基づいて ATHENA を実装する AI コーディングエージェントは、以下を遵守する。

- **ATH-DEV-001** (MUST) 実装前に本書の該当章を読まなければならない。
- **ATH-DEV-002** (MUST) Runtime Interface を Adapter 実装より先に定義しなければならない。
- **ATH-DEV-003** (MUST) Core Agent Loop のテストを UI 実装より先に完成させなければならない。
- **ATH-DEV-004** (MUST) 第 5.2 節の境界違反を検出する静的検査を CI に組み込まなければならない。
- **ATH-DEV-005** (MUST) 重要な状態遷移を Event として記録しなければならない（第 23 章）。
- **ATH-DEV-006** (MUST) Task / Plan / Agent Instance の状態を永続化しなければならない
  （第 27 章）。
- **ATH-DEV-007** (MUST NOT) 本書で定義した用語・ID・境界を実装側で改変してはならない。
- **ATH-DEV-008** (SHOULD) 分散化より単純性を優先する。

---

## Part II アーキテクチャ

### 6. 全体構成

#### 6.1 コンポーネント構成

```text
ATHENA
|
+-- Core
|    オーケストレーション層
|      +-- Core Agent          理解 / 計画 / 判断 / 委任 / 観測 / 評価
|      |                       （agent-cli instance #0 上で動作：第 7.5 節）
|      +-- Core Host           決定論的モジュール群。判断は行わない
|           +-- Core Agent Session  Core Agent Instance の起動 / 監視 / 再起動
|           +-- Agent Manager       Agent 定義と Instance の管理
|           +-- Hierarchy Manager   Agent 階層の構築と維持
|           +-- Message Broker      Agent間通信
|           +-- Runtime Manager     Runtime の選択と監視
|           +-- Tool Manager        Tool の提供と権限制御
|           +-- Memory Manager      短期 / 作業 / 長期記憶
|           +-- State Manager       Task / Instance 状態の一元管理
|           +-- Event Manager       Event の発行と配信
|           +-- Approval Manager    Human-in-the-Loop
|           +-- Result Manager      Result と Artifact の管理
|           +-- Scheduler           Queue / 優先度 / 並行制御
|           +-- Policy Manager      リスク分類と権限検査（第 29 章）
|
+-- Runtime
|    Agent 実行抽象化層
|      +-- Runtime Interface
|      +-- agent-cli Adapter
|      +-- Skill Manager       Registry / Catalog / Resolver / Loader / Validator
|      +-- Process Manager
|
+-- API
|    Core への統合 API
|
+-- CLI
|    コマンドライン UI
|
+-- Studio
     Web ブラウザ UI
```

将来拡張として Hub（Registry）と SDK（外部 Agent 開発 API）を追加できる。初期実装では必須と
しない（ATH-POLICY-014）。

- **ATH-CORE-008** (MUST) Core の下位モジュールは Core Agent を支える実行基盤として実装しなければ
  ならない。下位モジュールが Core Agent の判断を代替してはならない。

#### 6.2 制御フローの全体像

```text
                              HUMAN
                                |
                                v
                     ATHENA Studio / CLI                Presentation
                                |  HTTP / SSE
                                v
                           athena-api                   Core API
                                |
                                v
        +------------------------------------------------+
        |            athena-core = Core Host             |  決定論的
        |  Core Agent Session / State / Event / Scheduler |  判断は行わない
        |  Message Broker / Policy / Memory / Result      |
        +------------------------------------------------+
             |                                     ^
             | (1) Core Agent Instance を起動      | (2) 判断・委任要求
             | (4) 検査を通った委任を実行          |     （Core Host が検査）
             v                                     |
                        Runtime Interface ---------+
                                |
          +---------------------+----------------------+
          |                                            |
          v                                            v
   agent-cli #0                              agent-cli #1 .. #N
   Core Agent（Producer・常駐）               Director / Lead / Worker
   Understand / Plan / Decide                 （Agent Instance ごとに 1 プロセス）
   Observe / Evaluate / Re-plan
          |                                            |
          +---------------------+----------------------+
                                |
                                v
                 Claude / Codex / Ollama (LLM Provider)

(3) Agent 間の report / question / status は Core Host 内の Message Broker が配送する
    （Agent Process 同士を直結しない：ATH-MSG-001）
Runtime Interface の背後は交換可能（agent-cli / Docker / SSH / Kubernetes：第 14.3 節）
```

論理階層とプロセス構造は別である。

```text
論理階層（Message ルーティングと権限包含で表現：第 8.2 節）
  Core Agent (producer)        ルート Task
    └ Director      × N        領域（SubTask 群）
         └ Lead     × N        タスク（SubTask 1 件）
              └ Worker × N     詳細タスク＝機能（PlanStep 1 件）

プロセス構造（Runtime 配下に並列：ATH-RT-012）
  Runtime
    ├ agent-cli #0   Core Agent
    ├ agent-cli #1   Director
    ├ agent-cli #2   Lead
    └ agent-cli #3   Worker
```

この図が表す不変条件は以下である。

> **UI は Core API のみを経由する。思考はすべて Runtime 上の Agent Process で行われ、
> Core Host は判断せず監督する。Agent 階層は論理関係であり、プロセスの親子関係ではない。
> Runtime Interface の背後の実行基盤は交換可能である。**

- **ATH-API-001** (MUST) CLI、Studio、外部アプリケーションは同一の Core API を利用しなければ
  ならない。UI 固有の API を Core へ追加してはならない。

#### 6.3 責務分離

| 層 | 決めること | 決めないこと |
|---|---|---|
| Presentation | 表示と入力 | 計画、Agent 選択 |
| Core API | 認証、検証、公開契約 | 計画、実行方法 |
| Core Agent | 目的、計画、Agent / Skill の選択、評価 | Agent の起動方法 |
| Core Host（支援モジュール群） | 状態、順序、記録、権限 | 目的と計画 |
| Runtime | 起動方法、プロセス、Skill 配置 | 何を実行すべきか |
| Agent | 実作業の思考と遂行 | 他 Agent の組織化（委任は階層規則に従う） |

---

### 7. ATHENA Core Agent

#### 7.1 責務

Core Agent の責務は以下とする。

```text
人間の自然言語指示の理解
目的・制約・完了条件の解釈
不足情報の収集
実行計画（Plan）の生成
状況に応じた計画変更
必要な Agent / Skill の選択
Agent 階層の構築
Agent への指示
Agent からの報告の理解
Tool の選択
Result の評価と検証指示
Retry / Recovery の判断
Human-in-the-Loop 判断
最終結果の統合
```

#### 7.2 Agent Loop

Core Agent は以下のループで動作する。

```text
Human Instruction
       |
       v
  Understand  ── 目的 / 制約 / 完了条件を抽出
       |
       v
    Plan      ── Plan と PlanStep を生成
       |
       v
    Decide    ── 次に実行する PlanStep、Agent、Skill、Tool を決定
       |
       v
     Act      ── 委任 / Tool 実行 / 承認要求
       |
       v
   Observe    ── Result、Message、Event を収集
       |
       v
  Evaluate    ── 完了条件との照合、検証の必要性判断
       |
    +--+--+
    |     |
  未完   完了
    |     |
    v     v
 Re-plan  Final Result
    |
    +------> Decide
```

- **ATH-CORE-009** (MUST) Core Agent は Understand / Plan / Decide / Act / Observe / Evaluate /
  Re-plan の各段階を実装しなければならない。
- **ATH-CORE-010** (MUST) 各段階の遷移は Event として記録しなければならない（第 23 章）。
- **ATH-CORE-011** (MUST) Core Agent は、Agent から得た新しい情報に基づいて既存 Plan を破棄・
  変更できなければならない。
- **ATH-CORE-012** (MUST) Core Agent は 1 回の Loop 反復で少なくとも 1 つの観測可能な状態変化
  （Plan 更新、委任、Tool 実行、承認要求、完了）を生成しなければならない。無変化の反復を
  無制限に継続してはならない。
- **ATH-CORE-013** (MUST) Core Agent は Task ごとに反復回数上限と実行時間上限を持たなければ
  ならない。上限到達時は人間へエスカレーションする（第 15 章）。

#### 7.3 Understand 段階の出力

Understand 段階は、自然言語指示から以下の構造化情報を生成する。

```rust
pub struct Understanding {
  goal: String,                  // 達成すべき目的
  constraints: Vec<String>,         // 制約
  completion_criteria: Vec<String>, // 完了条件
  unknowns: Vec<String>,            // 不足情報
  risk_level: RiskLevel,         // 想定リスク
}
```

- **ATH-CORE-014** (MUST) 完了条件を抽出できない場合、Core Agent は推測で実行を開始せず、
  人間へ確認しなければならない。
- **ATH-CORE-015** (SHOULD) `unknowns` が存在する場合、Core Agent は実行前に調査 PlanStep を
  生成する。

#### 7.4 Decide 段階の決定事項

| 決定対象 | 参照 |
|---|---|
| 次に実行する PlanStep | 第 16 章 |
| 使用する Agent | 第 8 章 |
| 使用する Skill Set | 第 12 章 |
| 使用する Tool | 第 11 章 |
| Runtime | 第 14 章 |
| 承認要求の必要性 | 第 15 章 |

- **ATH-CORE-016** (MUST) Decide の結果は、選択理由とともに記録しなければならない。後から
  「なぜこの Agent と Skill が選ばれたか」を追跡できなければならない。

#### 7.5 Core Agent の実行形態

Core Agent も Agent であるため、他の Agent と同様に Runtime 上で実行する。すなわち Core Agent の
思考（LLM 推論）は専用の `agent-cli` インスタンスの中で行われる。下位 Agent は Core Agent とは
**別の `agent-cli` インスタンス**で動作し、プロセス、Workspace、文脈を共有しない。

```text
athena-api プロセス
  └ athena-core = Core Host（決定論的・判断を行わない）
       └ Runtime Interface
            ├ agent-cli instance #0   Core Agent（Producer persona・常駐・専用 Workspace）
            ├ agent-cli instance #1   Director Agent
            ├ agent-cli instance #2   Lead Agent
            └ agent-cli instance #N   Worker Agent
```

この構成が表す不変条件は以下である。

> **思考はすべて `agent-cli` 上の Agent Process で行われ、`athena-core` は思考しない。
> `athena-core` は Core Agent を起動・監視し、その要求を検査して実行させる監督者である。**

##### 7.5.1 Core Host と Core Agent の責務分離

| 区分 | Core Host（`athena-core`） | Core Agent（`agent-cli` #0） |
|---|---|---|
| 性質 | 決定論的なプログラム | LLM 推論を行う Agent |
| 責務 | 起動・監視・再起動、状態遷移、Event 発行、Queue、権限検査、承認仲介、記憶の保存 | 理解・計画・判断・委任・観測・評価・再計画 |
| 判断 | 行わない | 行う |
| 停止時の影響 | ATHENA 全体が停止 | 思考が停止（Core Host が再起動を試行） |

- **ATH-CORE-018** (MUST) Core Agent は専用の Agent Instance として、Runtime Interface 経由で
  起動しなければならない。
- **ATH-CORE-019** (MUST NOT) Core Agent と下位 Agent を同一の Runtime Instance で実行しては
  ならない。Agent Instance と Runtime Instance は 1 対 1 とする（ATH-RT-012）。
- **ATH-CORE-020** (MUST) Core Agent Instance はセッション単位の長命 Instance としなければ
  ならない。Task 単位で起動・停止してはならない。
- **ATH-CORE-021** (MUST) Core Host は Core Agent Instance を起動・監視・再起動しなければならない。
  この起動処理は ATHENA の起動シーケンスの一部とする（ブートストラップ）。
- **ATH-CORE-022** (MUST NOT) Core Host は意思決定を行ってはならない。Core Host の機能は監督、
  状態管理、権限強制、記録に限る（ATH-CORE-002 の帰結）。
- **ATH-CORE-023** (MUST) Core Agent は専用 Workspace を持ち、`aiprj` 管理下としなければならない。
  下位 Agent の Workspace と共有してはならない（第 36 章）。
- **ATH-CORE-024** (MUST) Core Agent の Persona は `producer` レベルとし、provider と model を
  設定で指定しなければならない（第 28 章）。
- **ATH-CORE-025** (MUST) Core Agent と下位 Agent の通信は Message Broker を経由しなければ
  ならない（ATH-MSG-001）。`agent-cli` の内部 IPC を Core Host の管理外で直結してはならない。

##### 7.5.2 特権操作の検査

Core Agent Process は信頼境界の外側にある（ATH-SEC-021）。したがって Core Agent の判断は、
Core Host による検査を経てから実行される。

```text
Core Agent Process（判断）
      | 要求: Agent 起動 / Tool 実行 / 状態遷移 / 承認要求
      v
Core Host（検査: 権限・状態・ポリシー・リスク）
      |
      +-- 適合     → 実行し Event を記録
      +-- 不適合   → PermissionError / PolicyError / ValidationError を返す
```

- **ATH-CORE-026** (MUST) Core Agent からの特権操作要求（Agent 起動、Tool 実行、状態遷移、
  Skill 有効化、承認の消費）は、すべて Core Host の検査を経なければならない。無検証で実行しては
  ならない。
- **ATH-CORE-027** (MUST NOT) Core Agent Process は永続ストアおよび Event Store へ直接書き込んでは
  ならない。記録は Core Host 経由で行う。

##### 7.5.3 Core Agent 障害時の決定論的回復

Core Agent が停止した状態では ATHENA は判断できない。この状態の回復手続きは、推論を伴わない
決定論的処理として Core Host が実行する。

```text
Core Agent Instance FAILED
      |
      v
Core Host（決定論的手続き）
      +-- 1. 実行中 Task への新規委任を停止
      +-- 2. 制御面のチェックサムを検証（ATH-CORE-044）
      +-- 3. Working Memory と Event から文脈文書を再構成
      +-- 4. Core Agent Instance を再起動し、文脈文書を再投入
```

手続きの結果に応じた動作：

| 前提 | 結果 |
|---|---|
| 制御面のチェックサムが不一致 | 再起動しない。`CoreAgentEscalated` を発行し人間へエスカレーション |
| 再起動成功 | `CoreAgentRestarted` を発行し、判断を Core Agent へ戻して実行を継続 |
| 資源不足・`TransientError` による起動失敗 | 試行回数に計上せず再試行（ATH-CORE-045） |
| 試行上限到達（既定: 1 時間以内に 3 回） | `CoreAgentEscalated` を発行。ATHENA は自律実行を停止し人間の判断を待つ |

- **ATH-CORE-028** (MUST) Core Agent Instance の `FAILED` を検出した場合、Core Host は実行中
  Task への新規委任を停止しなければならない。
- **ATH-CORE-029** (MUST) Core Host は Working Memory（第 13 章）と Event（第 23 章）から文脈を
  再構成し、Core Agent Instance を再起動しなければならない。この処理は決定論的でなければならない。
- **ATH-CORE-030** (MUST) 再起動の試行上限（既定 3 回）に達した場合、Core Host は人間へ
  エスカレーションし、ATHENA の自律実行を停止しなければならない。Core Host が判断を代行して
  実行を継続してはならない。
- **ATH-CORE-031** (MUST) Core Agent の停止・再起動・エスカレーションを Event として記録しなければ
  ならない（`CoreAgentStarted` / `CoreAgentFailed` / `CoreAgentRestarted` / `CoreAgentEscalated`）。

##### 7.5.4 Runtime 障害との関係

`agent-cli` 自体が利用不能な場合、Core Agent も下位 Agent も起動できない。この場合、
ATH-FAIL-011（失敗を再計画の入力とする）は適用できないため、Core Host は第 7.5.3 節の手続きに
したがって人間へエスカレーションする。

- **ATH-CORE-032** (MUST) Runtime が利用不能な場合、Core Host は新規 Task の受理を停止し
  （API は 503 を返す）、既存 Task を保留して人間へエスカレーションしなければならない。

##### 7.5.5 Core 制御プロトコル

Core Agent Process と Core Host の間でやり取りされる要求・応答を **Core 制御プロトコル** として
定義する。本プロトコルは Message の `payload` に載せ、`type` には制御用の値を用いる
（第 10.1 節）。

```rust
pub enum CoreRequest {
    SubmitUnderstanding { task_id: TaskId, understanding: Understanding },
    SubmitPlan { task_id: TaskId, base_revision: u64, steps: Vec<PlanStepDraft> },
    ComposeHierarchy { task_id: TaskId, nodes: Vec<HierarchyNodeDraft> },
    Delegate {
        task_id: TaskId, plan_step_id: PlanStepId,
        agent_id: AgentId, skill_set: Vec<SkillRef>, instruction: String,
        idempotency_key: String,                    // = plan_step_id + ":" + attempt
    },
    ExecuteTool {
        task_id: TaskId, tool: ToolName,
        args: serde_json::Value, idempotency_key: String,
    },
    RequestApproval {
        task_id: TaskId, operation_ref: String, reason: String,
        impact: String,                             // リスク分類は Core Host が行う
    },
    ConsumeApproval { approval_id: ApprovalId, operation_ref: String },
    AskHuman { task_id: TaskId, question: String, options: Option<Vec<String>> },
    SubmitEvaluation {
        task_id: TaskId, result_id: ResultId,
        verdict: Verdict, rationale: String,
    },
    TransitionTask { task_id: TaskId, to: TaskState, expected_state: TaskState },
    WriteMemory { tier: WritableMemoryTier, content: String },
    CompleteTask { task_id: TaskId, summary: String, result_ids: Vec<ResultId> },
}

pub enum WritableMemoryTier { Working, Long }

pub enum CoreResponse<T> {
    Ok { request_id: String, data: T },
    Err { request_id: String, error: ErrorEnvelope },   // 第 22.2 節
}
```

- **ATH-CORE-033** (MUST) Core Agent からの特権操作要求は本プロトコルの `CoreRequest` として
  表現しなければならない。プロトコル外の経路（直接の Tool 実行、直接のプロセス起動、ストアへの
  書き込み）を設けてはならない。
- **ATH-CORE-034** (MUST) Core Host はすべての `CoreRequest` に `CoreResponse` を返さなければ
  ならない。拒否の場合は第 20.1 節の分類に従った `ErrorEnvelope` を返す。
- **ATH-CORE-035** (MUST) `delegate` と `execute_tool` は `idempotency_key` を持たなければならない。
  Core Host は同一キーの要求を二重に実行してはならない（第 7.5.3 節の再起動後の二重委任防止）。
- **ATH-CORE-036** (MUST) `transition_task` は `expected_state` を伴わなければならない
  （ATH-STATE-014）。
- **ATH-CORE-037** (MUST) Core Host は `CoreRequest` の拒否を `PrivilegedRequestDenied` Event として
  記録しなければならない。拒否の事実は再起動後の文脈にも含めなければならない（ATH-CORE-029）。
- **ATH-CORE-038** (MUST NOT) Core Host は `request_approval` の `reason` / `impact` をリスク判定の
  根拠としてはならない。リスク分類は設定（第 28.2 節の `policy`）に基づいて Core Host が独立に
  決定する（ATH-HITL-002）。

##### 7.5.6 構造化出力の契約

Core Agent は LLM であるため、その出力を機械可読な構造へ変換する契約を定義する。

- **ATH-CORE-039** (MUST) Core Agent は `CoreRequest` を JSON として出力しなければならない。
  Core Host は JSON Schema による検証を行う。
- **ATH-CORE-040** (MUST) 検証に失敗した出力は、誤り内容を添えて再出力を要求しなければならない。
  再出力の上限は既定 3 回とし、超過時は `InternalError` として当該 Task をエスカレーションする。
- **ATH-CORE-041** (MUST) 識別子（`PlanId`、`PlanStepId`、`InstanceId`、`ResultId`、`ApprovalId`）は
  Core Host が発行しなければならない。Core Agent が採番した識別子を受理してはならない。
- **ATH-CORE-042** (MUST) Plan の `revision` は Core Host が加算しなければならない。`submit_plan` の
  `base_revision` が現行 revision と一致しない場合は `ValidationError` として拒否する。

##### 7.5.7 制御面の保全と再起動の健全性

Core Agent は作業対象へ書き込める（`fs_write`）。一方、Persona、`agent-cli` へ渡す設定、
`.aiprj/instructions.md` など**自身の振る舞いを規定するファイル**は Agent の書き込み範囲に
置いてはならない。これらは `<ATHENA_HOME>/var/` 配下に置かれ、`.athena/` は
`filesystem_scope` から除外される（ATH-OPS-008）。

- **ATH-CORE-043** (MUST NOT) Agent の制御面を当該 Agent の `filesystem_scope` に含めては
  ならない。制御面とは、Instance の領域（第 4.3.4 節）の `.agent-cli/` 配下
  （`config.toml`、`commands/`、`persona.md`）と `skills/`、および
  `<PROJECT_ROOT>/.aiprj/instructions.md`、`.aiprj/rules/` を指す。
  これらは Core Host が配置し、Agent からは読み取り専用とする。
- **ATH-CORE-044** (MUST) Core Host は Core Agent の制御面のチェックサムを起動時および再起動時に
  検証しなければならない。不一致の場合は再起動せず `CoreAgentEscalated` を発行して人間へ
  エスカレーションする。改竄された制御面で再起動してはならない。
- **ATH-CORE-045** (MUST) 再起動試行の計数はスライディングウィンドウとしなければならない
  （既定: 1 時間以内に 3 回）。資源不足や `TransientError` に起因する起動失敗は計数から除外する。
- **ATH-CORE-046** (MUST) 再起動時に再投入する文脈には、未終端の委任（`plan_step_id`、
  `instance_id`、`attempt`、状態）、`GRANTED` かつ未消費の Approval、直近の拒否
  （`PrivilegedRequestDenied`）を含めなければならない（ATH-MEM-007）。
- **ATH-CORE-047** (MUST) Core Host と各 Agent Instance の間にリース（heartbeat）を設けなければ
  ならない。heartbeat が既定 60 秒途絶した Instance は自己終了しなければならない。
- **ATH-CORE-048** (MUST) ATHENA 起動時、非終端の `AgentInstance` のうち対応するプロセスが
  存在しないものを `FAILED`（`side_effects: unknown`）として確定しなければならない。該当
  PlanStep の再試行は、Verification（ATH-EVAL-002）を経てからのみ許可する。
- **ATH-CORE-049** (MUST) 本節の「決定論的」は Core Host の**手続き**が決定論的であることを意味
  する。再開後の Core Agent の振る舞いは決定論的ではない。安全性は ATH-CORE-035 の冪等キーと
  ATH-CORE-048 の照合によって確保しなければならない。

---

### 8. Agent と Agent Hierarchy

#### 8.1 Agent の定義

Agent は、特定の目的を持って AI による処理を実行する論理的な実行主体である。Agent は必ずしも
独立したプログラムではない。ATHENA は以下の抽象を行う。

```text
Agent（論理）
  ↓
Runtime（実行抽象）
  ↓
Agent Process（実体）
```

Agent 定義は以下の情報を持つ。

```yaml
id: timing-lead
name: FPGA Timing Analysis Lead
level: lead
description: FPGA timing violation analysis
capabilities:
  - fpga
  - timing-analysis
  - sta
  - sdc
persona: personas/timing-lead.md
allowed_tools:
  - shell
  - fs_read
  - fs_write
  - send_to
default_skills:
  - id: fpga-timing-analysis
    version: "1.2.0"
runtime:
  type: agent-cli
model:
  provider: claude
  name: claude-opus-5
max_delegation_depth: 2
```

- **ATH-AGENT-001** (MUST) Agent 定義は ID、level、capabilities、persona、allowed_tools、runtime を
  持たなければならない。
- **ATH-AGENT-002** (MUST) Agent 定義（静的）と Agent Instance（動的）を区別しなければならない。
- **ATH-AGENT-003** (MUST) Agent Instance は第 17.2 節の状態機械に従わなければならない。
- **ATH-AGENT-008** (MUST) Agent 定義は `model.provider` と `model.name` を**必須指定**と
  しなければならない。未指定の定義は登録・起動のいずれにおいても `ValidationError` として
  拒否する。ATHENA 側に provider / model の既定値を持ってはならない。コストと品質の再現性を
  定義ファイルのみから判定できる状態を保つためである。Core Agent についても同様とする
  （ATH-CORE-024）。

#### 8.2 階層モデル

```text
 ATHENA Core Agent (Producer)              利用者指示の全体（ルート Task）
            |
   +--------+--------------------+
   |                             |
Director A                  Director B     領域（SubTask 群）
   |                             |
   +----+-----------+            +-----------+
   |                |                        |
Lead A1          Lead A2                  Lead B1    タスク（SubTask 1 件）
   |                |                        |
 +-+--+          +-+--+                   +-+--+
 |    |          |    |                   |    |
 W    W          W    W                   W    W     詳細タスク＝機能
                                                      （PlanStep 1 件）
```

##### 8.2.1 階層と作業単位の対応

各階層は、Task 分解のどの粒度を担当するかが定まっている。

| 階層 | 担当する作業単位 | 対応するデータ（第 26 章） | 多重度 |
|---|---|---|---|
| Producer（Core Agent） | 利用者指示の全体 | ルート `Task` | 1（常駐、第 7.5 節） |
| Director | 領域・分野ごとのまとまり | ルート `Task` 配下の `SubTask` 群 | Producer : Director = 1 : N |
| Lead | 個々のタスク | `SubTask` 1 件 | Director : Lead = 1 : N |
| Worker | 詳細タスク（機能・作業項目） | `PlanStep` 1 件 | Lead : Worker = 1 : N |

```text
ルート Task        ── Producer（Core Agent）
  └ SubTask 群     ── Director（領域単位に束ねる）
       └ SubTask   ── Lead（タスク 1 件の完了責任）
            └ PlanStep ── Worker（機能 1 件の実作業）
```

- **ATH-HIER-012** (MUST) 各階層の担当作業単位を上表に従って割り当てなければならない。
  Lead に `PlanStep` 単位の実作業を直接担わせたり、Worker に `SubTask` 全体の完了責任を
  負わせたりしてはならない。
- **ATH-HIER-013** (MUST) 1 つの Director は複数の Lead を、1 つの Lead は複数の Worker を
  同時に持てなければならない（1 : N）。
- **ATH-HIER-014** (SHOULD) 1 つの Worker Instance は 1 つの `PlanStep`（詳細タスク＝機能 1 件）に
  対応させる。複数の `PlanStep` を 1 Instance へ束ねると、並列実行と失敗の切り分けが困難になる。
  1 つの機能が実装・テスト・ドキュメントの複数工程からなる場合、各工程を独立した `PlanStep`
  とし、それぞれ別の Worker Instance へ割り当てる。工程間で必要な文脈は Artifact と Result
  （第 19 章）および `PlanStep` の `depends_on`（第 16.3 節、ATH-PLAN-006）で受け渡し、
  1 Instance を継続させることで受け渡してはならない。
- **ATH-HIER-015** (MUST) Lead は担当 `SubTask` の完了責任を持ち、配下 Worker の Result を
  集約・評価してから上位へ `report` を返さなければならない。Worker の Result をそのまま
  中継してはならない。
- **ATH-HIER-016** (MUST) 報告は階層を遡って集約しなければならない。

```text
Worker → Lead（機能単位の結果を集約・評価）
       → Director（タスク単位の結果を集約・評価）
       → Core Agent（領域横断で統合し、完了条件と照合）
```

- **ATH-HIER-017** (MUST) 同一階層の Agent Instance は、依存関係がない限り並列実行可能で
  なければならない（第 18.2 節）。Lead 間および Worker 間の直列化を既定としてはならない。

##### 8.2.2 組織の比喩（非規範）

ATHENA の階層構成は、ゲーム開発やアニメ制作の制作体制に近い。本節は理解を助けるための対応で
あり、規範的要求ではない。

| ATHENA | ゲーム開発 | アニメ制作 | 担当単位 |
|---|---|---|---|
| Producer（Core Agent） | プロデューサー / ディレクター | プロデューサー / 監督 | タイトル・作品の全体（ルート Task） |
| Director | 領域ごとの責任者（テクニカル / アート / サウンド） | 各監督（作画監督・美術監督・音響監督） | 領域（SubTask 群） |
| Lead | リードプログラマ / リードアーティスト | 各話演出・セクションのリーダー | タスク（SubTask 1 件） |
| Worker | 各機能の担当者 | 原画・動画・彩色などの各担当 | 詳細タスク＝機能・カット（PlanStep 1 件） |
| Core Host | プロダクションマネージャ | 制作進行 | 判断せず、進行・記録・権限・資源を管理（第 7.5 節） |

制作現場の仕組みと ATHENA の機構も対応する。

| 制作現場の仕組み | ATHENA の対応 |
|---|---|
| リテイク（差し戻し） | Evaluation / Verification と Retry / Re-plan（第 19 章、第 20 章） |
| 監督チェック・OK 出し | Lead / Director による集約評価（ATH-HIER-015）、重要判断は Human-in-the-Loop（第 15 章） |
| 進行表・スケジュール | Scheduler、優先度、`deadline`（第 18 章、第 26 章） |
| 設定資料・仕様書 | Persona（役割と行動規範）、Skill（手順と判断基準）、`aiprj` 文書（第 9 章、第 12 章、第 36 章） |
| 作業日報・作業報告 | `aiprj` AI_LOG と `report` Message（第 36 章、第 10 章） |
| 並行して進む複数カット・複数機能 | 同一階層の並列実行（ATH-HIER-017、第 18.2 節） |

この比喩が示す設計上の要点は以下である。

- 上位は**成果の受け入れ可否を判断する**役であり、作業そのものを行わない（ATH-HIER-012）。
- 各段で品質確認が入り、不合格は差し戻される（ATH-HIER-015、ATH-EVAL-002）。
- 進行管理は判断と分離されている（ATH-CORE-022）。
- 組織構成は案件ごとに編成される（ATH-HIER-007）。固定の組織表を前提としない。

##### 8.2.3 階層属性と階層規則

本節は規範的である。

各 Agent は以下の階層属性を持つ。

```text
level          producer | director | lead | worker
superior       上位 Agent（producer は null）
subordinates   下位 Agent の集合
role
skills
persona
allowed_tools
runtime
provider / model
```

- **ATH-HIER-002** (MUST) Agent 階層は親子関係が一意に定まる木構造でなければならない。
- **ATH-HIER-003** (MUST NOT) 階層に循環を作ってはならない。
- **ATH-HIER-004** (MUST) 委任は原則として直下の階層に対してのみ行わなければならない。階層を
  飛び越える委任は Core Agent が明示的に許可した場合に限る。
- **ATH-HIER-005** (MUST) 実装は委任深さの上限（既定値 4）を持たなければならない。上限超過の
  委任要求は `PolicyError` として拒否する。
- **ATH-HIER-006** (MUST) 下位 Agent の権限は上位 Agent の権限を超えてはならない。
- **ATH-HIER-010** (MUST) Agent 階層は論理的な関係として実装しなければならない。階層は Message
  ルーティング（ATH-MSG-006）と権限包含（ATH-HIER-006）によって表現し、プロセスの親子関係として
  実装してはならない。すべての Agent Instance は Runtime の管理下で並列に存在する（ATH-RT-012）。
- **ATH-HIER-011** (MUST NOT) Agent Process が他の Agent Process を直接起動してはならない。
  Agent Instance の起動は Core Host のみが行う（第 7.5.2 節）。

#### 8.3 Dynamic Delegation

Core Agent は Task ごとに必要な組織を構成する。

FPGA 案件：

```text
ATHENA Core
 |
 +-- FPGA Director
       |
       +-- RTL Lead
       +-- Timing Lead
       +-- Verification Lead
```

Linux 案件：

```text
ATHENA Core
 |
 +-- Linux Director
       |
       +-- Kernel Lead
       +-- Driver Lead
       +-- Test Lead
```

- **ATH-HIER-007** (MUST) Core Agent は Task の内容に応じて階層構成を動的に決定しなければならない。
  固定の階層構成を前提としてはならない。
- **ATH-HIER-008** (MAY) 実装は Agent 定義の動的生成を許容する。動的生成された Agent も
  ATH-AGENT-001 の要件を満たさなければならない。
- **ATH-HIER-009** (MUST) 階層構成の確定時点で、構成内容を Event として記録しなければならない。
- **ATH-HIER-018** (MUST) v1 における Agent 定義の動的生成（ATH-HIER-008）の範囲は、**既存
  Persona の組み合わせに限る**。動的生成は既存の Persona ファイル、既存の capability、既存の
  `allowed_tools` の部分集合のみから定義を構成しなければならない。Persona 本文そのものの生成、
  および既存のどの Agent 定義にも付与されていない権限の付与を行ってはならない。Persona の
  生成は Phase 8（第 33.1 節）で評価する。

階層委任における自律は、抑制するのではなく**境界を与える**。階層委任は第一級機能であり、
ATH-HIER-004（直下の階層への委任）、ATH-HIER-005（委任深さの上限）、ATH-CORE-013、および
第 15 章の承認点が境界となる。Core が Workflow を集中制御する形へ退行させてはならない
（ATH-POLICY-001 系の設計原則、第 4.4 節）。

#### 8.4 Agent Selection

初期実装は Capability Matching を基本とする。

```text
Task 必要能力:
  systemverilog
  rtl-analysis

Agent A: systemverilog, rtl-analysis, synthesis   → 適合
Agent B: python, data-analysis                     → 不適合
```

- **ATH-AGENT-004** (MUST) Agent 選択は、Task が要求する capabilities を Agent の capabilities が
  包含することを条件としなければならない。
- **ATH-AGENT-005** (MUST) 適合 Agent が存在しない場合、Core Agent は Task を再分解するか、
  人間へエスカレーションしなければならない。暗黙に不適合 Agent を割り当ててはならない。
- **ATH-AGENT-006** (SHOULD) 複数候補が存在する場合、以下の総合評価で選択する。

```text
Score = capability_score
      + reliability_score
      + availability_score
      + performance_score
      - cost_score
```

評価因子（将来拡張を含む）：Capability、Model、Cost、Latency、Reliability、Current Load、
History、Priority。

- **ATH-AGENT-007** (MUST) 選択結果と選択理由を Event および実行履歴へ記録しなければならない。

---

### 9. Persona

Persona は Agent の専門性・行動原則・利用可能 Tool を定義する。Persona は人格設定ではなく、
**役割・責務・権限・専門知識・行動規範**の定義である。

```yaml
---
name: timing-lead
role: FPGA Timing Analysis Lead
level: lead

skills:
  - FPGA
  - STA
  - SDC

allowed_tools:
  - shell
  - fs_read
  - fs_write
  - send_to

report_to: fpga-director
---

# Timing Lead

あなたはFPGA Timing Analysisの専門Leadです。

必要に応じてWorker Agentへ作業を委任し、
結果を評価して上位Agentへ報告してください。
```

- **ATH-PERSONA-001** (MUST) Persona は front matter（機械可読な定義）と本文（Agent への指示）
  から構成されなければならない。
- **ATH-PERSONA-002** (MUST) front matter は name、role、level、allowed_tools を持たなければ
  ならない。
- **ATH-PERSONA-003** (MUST) Persona の `allowed_tools` は Agent 定義の権限を超えてはならない。
  超える場合は Agent 起動を拒否する。
- **ATH-PERSONA-004** (MUST NOT) Persona 本文は第 29 章の Security ポリシーおよび第 36 章の
  aiprj 作業規約に反する指示を含んではならない。
- **ATH-PERSONA-005** (MUST) Persona はバージョン管理対象のファイルとして保存しなければならない。
  実行履歴には使用した Persona の識別子とバージョンを記録する。

---

### 10. Agent間通信

Agent間通信は ATHENA の中核機能である。

#### 10.1 Message 型

| type | 方向 | 用途 |
|---|---|---|
| `instruction` | 上位 → 下位 | 作業指示 |
| `report` | 下位 → 上位 | 結果報告 |
| `question` | 任意 | 問い合わせ |
| `answer` | 任意 | 問い合わせへの回答 |
| `status` | 下位 → 上位 | 進捗通知 |
| `error` | 下位 → 上位 | 異常通知 |
| `complete` | 下位 → 上位 | 完了通知 |
| `approval_request` | 任意 → Core | 承認要求 |
| `control_request` | Agent → Core Host | Core 制御プロトコルの要求（第 7.5.5 節） |
| `control_response` | Core Host → Agent | Core 制御プロトコルの応答 |
| `cancel` | Core Host → Agent | 取消通知（制御 Message） |
| `interrupt` | Core Host → Agent | 優先割り込み（制御 Message） |

#### 10.2 Message Envelope

```rust
pub struct Message {
  message_id: String,          // "msg-" + ULID
  type: MessageType,
  from: AgentId,
  to: AgentId,                 // 送信先 Agent 定義
  to_instance_id: Option<InstanceId>,    // 宛先 Instance（null は単一 Instance の Agent）
  from_instance_id: InstanceId,         // 送信元 Instance（試行の識別に用いる）
  task_id: TaskId,
  thread_id: String,           // 一連の対話の識別子
  reply_to: Option<String>,     // 応答対象の message_id
  priority: "low" | "normal" | "high",
  body: String,                // 自然言語本文
  payload: Option<serde_json::Value>,
  deadline: Option<String>,     // RFC 3339
  created_at: String,          // RFC 3339
}
```

```json
{
  "message_id": "msg-01JA2B3C4D5E6F7G8H9J0K1L2M",
  "type": "instruction",
  "from": "athena-core-agent",
  "from_instance_id": "ai-01JA2B3C4D5E6F7G8H9J0K1L20",
  "to": "timing-lead",
  "to_instance_id": "ai-01JA2B3C4D5E6F7G8H9J0K1L2P",
  "task_id": "task-001",
  "thread_id": "thr-001",
  "reply_to": null,
  "priority": "high",
  "body": "タイミング問題の原因を調査してください。",
  "payload": null,
  "deadline": "2026-10-12T12:00:00Z",
  "created_at": "2026-10-12T10:00:00Z"
}
```

#### 10.3 通信規則

```text
Parent Agent
    | instruction
    v
Child Agent
    | report
    v
Parent Agent
```

- **ATH-MSG-001** (MUST) すべての Agent間通信は Message Broker を経由しなければならない。
  Agent 間の直接通信経路を Core の管理外に作ってはならない。
- **ATH-MSG-002** (MUST) Message は第 10.2 節の envelope を満たさなければならない。
- **ATH-MSG-003** (MUST) 配送保証は at-least-once としなければならない。受信側は `message_id` に
  よる冪等処理を実装しなければならない。
- **ATH-MSG-004** (MUST) 同一 `thread_id` 内の Message 順序は保証しなければならない。異なる
  thread 間の順序は保証しない。
- **ATH-MSG-005** (MUST) `instruction` および `question` には応答期限を設定しなければならない。
  期限内に応答がない場合 `TimeoutError` として扱う（第 20 章）。
- **ATH-MSG-006** (MUST) 通信は階層関係（親子）に従わなければならない。兄弟 Agent 間の直接通信は
  Core Agent が明示的に許可した場合に限る。
- **ATH-MSG-007** (MUST) Message の送受信を Event として記録しなければならない
  （`AgentMessageSent` / `AgentMessageReceived`）。
- **ATH-MSG-008** (MUST NOT) Message の body および payload に秘密情報を平文で含めてはならない。
- **ATH-MSG-009** (MUST) Broker は Message の滞留数上限（`messaging.max_queue_depth`、既定 1000）を
  持たなければならない。上限超過時に `TransientError` を返す対象は `instruction` / `status` /
  `question` に限る。
- **ATH-MSG-010** (MUST) 宛先の特定には `to_instance_id` を用いなければならない。`AgentId` のみを
  宛先とする指定は、同時に 1 Instance しか存在しない Agent（Core Agent）に限り許容する。
- **ATH-MSG-011** (MUST NOT) Core Host（`athena-core`）を Message の送信元・宛先としてはならない。
  Core Host は配送基盤であり Agent ではない。Core Agent の `AgentId` は `athena-core-agent` とする。

#### 10.4 Broker の規則

- **ATH-MSG-012** (MUST) `report` / `complete` / `error` および制御 Message（`cancel` /
  `interrupt` / `control_response`）は破棄・拒否してはならない。Broker はこれらを永続化した
  時点で送信元へ受理応答を返さなければならない。完了済み作業の報告を失ってはならない。
- **ATH-MSG-013** (MUST) `thread_id` は Core Host が発行しなければならない。委任 1 件につき
  1 thread とし、その配下の往復は同一 `thread_id` を用いる。
- **ATH-MSG-014** (MUST) 重複排除は Broker 側で行わなければならない。`(to_instance_id,
  message_id)` 単位の配送・受理記録を `messages` へ永続化し、受理済み Message を再配送しては
  ならない。Agent Process 側の記憶に冪等性を依存してはならない。
- **ATH-MSG-015** (MUST) 送信側の再試行とバックオフは Runtime Adapter（Core Host 側）が実装しなければ
  ならない。Agent Process へ再試行責務を委ねてはならない。
- **ATH-MSG-016** (MUST) `instruction` / `question` の応答期限の既定値は
  `timeout.message_reply` とする。委任先の `startup` と `execution` の合計より短い値を設定しては
  ならない（ATH-CFG-008）。
- **ATH-MSG-017** (MUST) 制御 Message（`cancel` / `interrupt`）は滞留中の Message より先に配送しなければ
  ならない。
- **ATH-MSG-018** (MUST) 兄弟間通信の許可は、Core Agent の `compose_hierarchy` 要求に含まれる
  許可リストとして記録し、Core Host が配送時に照合しなければならない（ATH-MSG-006）。

---

### 11. Tool

Agent は Tool を利用して外部システムへ作用する。

#### 11.1 基本 Tool

| Tool | 用途 | 既定リスク |
|---|---|---|
| `fs_read` | ファイル読み取り | low |
| `fs_write` | ファイル書き込み | medium |
| `shell` | シェル実行 | high |
| `git` | バージョン管理操作 | medium |
| `search` | 検索 | low |
| `send_to` | Agent間通信 | low |
| `process` | プロセス操作 | high |
| `terminal` | 対話端末操作 | high |
| `network` | 外部ネットワーク通信 | high |

#### 11.2 Tool 権限

```yaml
allowed_tools:
  - fs_read
  - fs_write
  - shell
  - send_to
```

- **ATH-TOOL-001** (MUST) Tool は Agent ごとに許可しなければならない。既定は拒否とする。
- **ATH-TOOL-002** (MUST) Tool 実行は Core Host の Tool Manager を経由しなければならない。
  許可外の Tool 実行要求は Tool Manager が拒否し `PermissionError` を返さなければならない
  （ATH-SEC-022、ATH-SEC-026）。
- **ATH-TOOL-003** (MUST) リスク `high` の Tool 実行は、第 15 章の承認対象に該当する場合、
  承認取得後に実行しなければならない。
- **ATH-TOOL-004** (MUST) Tool 実行の開始・終了・失敗を Event として記録しなければならない。
- **ATH-TOOL-005** (MUST) `fs_read` / `fs_write` は Agent Workspace を基準としたパス境界内に
  制限しなければならない。境界外アクセスは `PermissionError` とする。

---

### 12. Skill

#### 12.1 Skill の定義

Skill は Agent でも Tool でもなく、**特定の作業を実行するための専門能力をパッケージ化したもの**
である。知識・手順・判断基準・Tool の利用方法・成果物形式をまとめた作業能力として扱う。

| 要素 | 役割 |
|---|---|
| Agent | 目的を理解し、判断し、作業を遂行する AI 実行主体 |
| Skill | 特定分野・特定作業を遂行するための再利用可能な専門能力 |
| Tool | Agent が外部システムへ作用するための具体的な操作手段 |
| Runtime | Agent と Skill を実行環境へ接続する実行基盤 |
| ATHENA Core Agent | Task を理解し、Agent / Skill / Tool を組み合わせる Executive Agent |

例（FPGA タイミング解析）：

```text
Agent: Timing Lead
Skill: fpga-timing-analysis
Tool : Quartus / STA / shell
```

- **ATH-SKILL-001** (MUST) Skill、Agent、Tool の責務を実装上も分離しなければならない。
- **ATH-SKILL-002** (MUST NOT) Core Agent は Skill の実装本体を直接解釈してはならない。
  Skill Catalog のメタデータに基づいて選択する。

#### 12.2 Skill Package

```text
skills/
└── fpga-timing-analysis/
    ├── SKILL.md         Agent に与える専門知識・手順・判断基準
    ├── manifest.yaml    Runtime が機械的に管理するメタデータ
    ├── references/
    ├── templates/
    └── scripts/
```

- **ATH-SKILL-003** (MUST) Skill Package は `SKILL.md` と `manifest.yaml` を含まなければならない。
- **ATH-SKILL-004** (MUST) Runtime は Skill の依存関係、バージョン、必要権限、実行条件を検証
  できなければならない。

#### 12.2.1 Skill の到達(open item O-2)

`agent-cli` に Skill 配置機構が存在しないため(第 2.2 節)、Loader(`ATH-SKILL-017`)が
配置した実体を **Agent** が到達する手段を Skill Manager が自ら実装する。v1 の方式は、
Instance が生成した `.agent-cli/commands/` 配下に `athena-skill-<id>.md` 手順ファイルを
**Skill ごとに生成** し、内容として **絶対パス**で `<workspace>/skills/<id>/SKILL.md` を
読むよう指示するものである(`ATH-ACLI-017` の IPC 経路での手順ファイル扱いに基づく)。配置先として
`.agent-cli/skills/` を使用してはならない(`ATH-ACLI-015`)。

- **ATH-SKILL-039** (MUST) Skill Manager は、Loader が配置した各 Skill に対し、
  `.agent-cli/commands/athena-skill-<id>.md` を生成しなければならない。内容は
  **絶対パス**で `<workspace>/skills/<id>/SKILL.md` を読むよう指示するものとする
  (`ATH-ACLI-017`)。生成先として `.agent-cli/skills/` を使用してはならない
  (`ATH-ACLI-015`)。


#### 12.3 Skill Catalog

```yaml
id: fpga-timing-analysis
name: FPGA Timing Analysis
version: 1.2.0
description: Analyze FPGA timing violations and identify root causes.

capabilities:
  - fpga
  - timing-analysis
  - sta
  - sdc

required_tools:
  - shell
  - fs_read
  - fs_write

required_permissions:
  - fs_read
  - fs_write
  - shell

supported_agents:
  - timing-lead
  - verification-agent

dependencies:
  - id: sdc-analysis
    version: "^1.0.0"

runtime:
  provider: agent-cli

source: https://example.invalid/skills/fpga-timing-analysis
checksum: "sha256:..."
publisher: aquaxis
risk:
  level: medium
status: available
```

- **ATH-SKILL-005** (MUST) Catalog の各項目は id、name、version、description、capabilities、
  required_tools、required_permissions、supported_agents、dependencies、runtime、source、
  checksum、publisher、risk、status を持たなければならない。
- **ATH-SKILL-006** (MUST) Core Agent は Catalog を検索して Skill 候補を取得しなければならない。

#### 12.4 Skill Registry

```text
Skill Package → Validation → Registry 登録 → Catalog 更新 → Core Agent が利用可能
```

Registry は以下の操作を提供する。

```text
register(skill)
unregister(skill)
update(skill)
enable(skill)
disable(skill)
list()
get(skill_id)
search(capability)
refresh()
```

- **ATH-SKILL-007** (MUST) Registry は上記 9 操作を提供しなければならない。
- **ATH-SKILL-008** (MUST) Skill は ATHENA の稼働中に追加できなければならない。Skill 追加のために
  Core または Runtime 全体を再起動してはならない。
- **ATH-SKILL-009** (MUST) 登録前に Validation を実施しなければならない。検証失敗の Skill は
  `FAILED` 状態とし、Core の選択候補から除外する。
- **ATH-SKILL-010** (MUST) 実行中の Agent Instance の Skill Set は、起動時に解決した内容で固定
  しなければならない。
- **ATH-SKILL-011** (MAY) 安全に Hot Reload 可能と宣言された Skill について、実行中 Agent への
  追加を許容する。

#### 12.5 Skill Manager

Runtime 内に Skill Manager を実装する。

```text
ATHENA Runtime
|
+-- Skill Manager
|   +-- Registry
|   +-- Catalog
|   +-- Resolver
|   +-- Loader
|   +-- Validator
|   +-- Policy
|   +-- Lifecycle Manager
|
+-- Agent Runtime
|   +-- agent-cli Adapter
|
+-- Process Manager
```

責務：Skill の検出、登録、検証、有効化・無効化、ロード、バージョン管理、依存解決、更新、削除、
Agent への提供、実行権限制御。

- **ATH-SKILL-012** (MUST) Skill Manager は Runtime の抽象層として実装し、`agent-cli` の内部実装へ
  直接依存してはならない。

#### 12.6 Skill Selection

```text
Human Request
     |
     v
Core Agent（Task 理解 → 必要能力抽出）
     |
     v
Skill Catalog 検索
     |
     +-- Candidate Skill A / B / C
     |
     v
Skill Selection
     +-- Capability Match
     +-- Agent Compatibility
     +-- Tool Availability
     +-- Version Compatibility
     +-- Risk / Permission
     +-- Cost / Performance
     |
     v
Selected Skill Set
```

例えば「Arria 10 のタイミング違反の原因を調査してください」という Task では、Core Agent は
必要能力を以下のように抽出し、Catalog を検索する。

```text
Required capabilities:
  FPGA
  Arria10
  Timing Analysis
  STA
  SDC

候補 Skill:
  fpga-timing-analysis
  arria10-sta
  sdc-analysis
```

その後、Agent の能力、利用可能 Tool、権限、Skill の互換性を評価して使用する Skill を決定する。

- **ATH-SKILL-013** (MUST) 初期実装の Skill 選択は Capability Matching を基本としなければならない。
- **ATH-SKILL-014** (MUST) 選択時に Agent 互換性、Tool 可用性、バージョン互換性、権限、リスクを
  検証しなければならない。
- **ATH-SKILL-015** (SHOULD) 将来的に以下の総合評価を行う。

```text
Skill Score = capability_score
            + compatibility_score
            + quality_score
            + reliability_score
            + availability_score
            - cost_score
            - risk_score
```

#### 12.7 Skill Resolution と Agent 起動

```text
Core: Agent = timing-lead, Skills = [fpga-timing-analysis, sdc-analysis]
  |
  v
Runtime → Skill Resolver
  +-- dependency check
  +-- version check
  +-- permission check
  +-- tool check
  |
  v
Skill Loader → Instance の skills/（絶対パス。提供手段は未確定）
  |
  v
Agent Process
```

- **ATH-SKILL-016** (MUST) Resolver は依存、バージョン、権限、Tool の 4 検証をすべて通過した
  Skill のみ Loader へ渡さなければならない。
- **ATH-SKILL-017** (MUST) Loader は Core Agent が選択した Skill のみを、対象 Agent Instance の
  Workspace 直下（`<ATHENA_HOME>/var/instances/<instance_id>/skills/`、第 4.3.4 節）へ配置しな
  ければならない。選択外の Skill を配置してはならない。`agent-cli` は Skill 配置の機構を持たない
  （第 2.2 節）ため、配置した実体を Agent へ到達させる手段を Skill Manager が定める。
  `.agent-cli/skills/` を配置先としてはならない（ATH-ACLI-015）。
- **ATH-SKILL-018** (MUST NOT) Agent は共有 Skill の実体を直接変更してはならない。更新は
  Registry 経由で行う。

Skill 解決の算法を以下に定める。

- **ATH-SKILL-028** (MUST) 依存関係のバージョン指定は Semantic Versioning の範囲表記
  （`1.2.0` / `^1.2.0` / `~1.2.0` / `>=1.2.0 <2.0.0`）に限る。
- **ATH-SKILL-029** (MUST) 解決は依存関係の推移閉包に対して行い、各 Skill は範囲を満たす最大版を
  選択しなければならない。
- **ATH-SKILL-030** (MUST) 推移閉包内で同一 Skill に両立しない範囲が要求された場合、解決を失敗させ
  `SkillError` を返さなければならない。暗黙に片方を採用してはならない。
- **ATH-SKILL-031** (MUST) 権限検査は推移閉包全体の `required_permissions` の和集合に対して
  行わなければならない（ATH-SEC-012）。
- **ATH-SKILL-032** (MUST) 推移閉包に含まれるすべての Skill が `ENABLED` でなければ、解決を失敗
  させなければならない。
- **ATH-SKILL-033** (MUST) Tool 検査は Agent の `allowed_tools` と Runtime の `capabilities()` の
  両方に対して行わなければならない。いずれかを満たさない場合は解決を失敗させる。
- **ATH-SKILL-034** (MUST) Hot Reload（ATH-SKILL-011）を実施する場合、ATH-SEC-012 の権限検査を
  再実行し、結果を当該 Instance の `skill_set` の新しい revision として記録しなければならない。
  再検査なしに実行中 Agent の能力を拡張してはならない。
- **ATH-SKILL-035** (MUST) **外部から取得する** Skill の登録は、設定で許可した publisher
  （`skills.allowed_publishers`）と取得元オリジン（`skills.allowed_sources`）に限らなければ
  ならない。チェックサムのみを信頼の根拠としてはならない。
- **ATH-SKILL-037** (MUST) `<ATHENA_HOME>/skills/` を**プロジェクト内の Skill Package の
  ソース**の所在としなければならない。ここに置かれた Package は外部取得ではないため
  ATH-SKILL-035 の許可リストの対象外とし、`skills.local_dir` として設定で参照する。ただし
  Validation（ATH-SKILL-009）と権限検査（ATH-SEC-012）は外部 Skill と同一に適用する。
- **ATH-SKILL-038** (MUST) 登録は `<ATHENA_HOME>/skills/`（ソース）または許可された外部取得元を
  入力とし、`skills.dir`（`<ATHENA_HOME>/var/skills/`）へ登録済みの実体を作る操作としなければ
  ならない。ソースと登録済みを同一ディレクトリとしてはならない。登録済みのみが Core Agent の
  選択候補となる（ATH-SKILL-006）。

#### 12.8 Skill Versioning

- **ATH-SKILL-019** (MUST) Skill は Semantic Versioning（`MAJOR.MINOR.PATCH`）に従わなければ
  ならない。
- **ATH-SKILL-020** (MUST) Agent 実行時に Skill のバージョンを固定しなければならない。
- **ATH-SKILL-021** (MUST) 実行履歴に Skill ID、Skill Version、Skill Checksum を保存しなければ
  ならない。

```json
{
  "agent_id": "timing-lead",
  "instance_id": "ai-01JA2B3C4D5E6F7G8H9J0K1L2P",
  "skills": [
    { "id": "fpga-timing-analysis", "version": "1.2.0", "checksum": "sha256:..." },
    { "id": "sdc-analysis", "version": "1.0.1", "checksum": "sha256:..." }
  ]
}
```

```yaml
skills:
  - id: fpga-timing-analysis
    version: "1.2.0"
```

#### 12.9 Skill Security

Skill ごとに以下を管理する。

```text
Source / Version / Checksum / Publisher
Required Tools / Required Permissions
Network Access / Filesystem Access / Process Access
Risk Level
```

- **ATH-SKILL-022** (MUST) 外部から取得した Skill は登録前に checksum と publisher を検証しなければ
  ならない。
- **ATH-SKILL-023** (MUST) Skill の要求権限に関する規範的定義は ATH-SEC-012 とする。

```text
permissions(Skill) ⊆ permissions(Agent)
```

- **ATH-SKILL-024** (MUST) リスク `high` の Skill 利用は人間の承認を要求しなければならない。
- **ATH-SKILL-036** (MUST) v1 の Skill 配布の検証方式は **checksum と publisher / source の
  許可リスト**とする（ATH-SKILL-022）。許可リストにない publisher または source からの Skill は
  登録を拒否しなければならない。公開鍵署名および Registry 限定配布を v1 の要件としてはならない。
  許可リストに依らず**外部公開された Skill を受け入れる段階で、公開鍵署名の検証を必須化**し、
  本書を改訂する。それまでの間、外部公開 Skill の受け入れを実装してはならない。

#### 12.10 Skill の再利用と共有

```text
                  Skill Registry
                       |
          +------------+------------+
          |            |            |
      Core Agent   Timing Lead   Worker Agent
```

- **ATH-SKILL-025** (MUST) Skill は複数 Agent から利用できる共有資産として管理しなければならない。
  Agent Workspace には解決済みバージョンの複製を提供し、再現性を確保する。

#### 12.11 Skill Lifecycle

状態と遷移は第 17.3 節の表で定義する。

#### 12.12 Skill と aiprj の優先順位

Skill に含まれる作業手順が Agent の `aiprj` 作業規約と矛盾する場合、以下の優先順位を適用する。

```text
ATHENA Security / System Policy
        ↓
Agent aiprj Project Rules
        ↓
Skill Instructions
        ↓
Task-specific Prompt
```

- **ATH-SKILL-026** (MUST) 実装は上記優先順位を強制しなければならない。
- **ATH-SKILL-027** (MUST NOT) Skill は `aiprj` のプロジェクト規約を無効化してはならない。

---

### 13. Memory

#### 13.1 階層

| 階層 | 内容 | スコープ | 既定保持期間 |
|---|---|---|---|
| Short Term Memory | 現在の会話、現在の Task 文脈 | Agent Instance | Instance 終了まで |
| Working Memory | Plan、Agent 報告、現在状態 | Task | Task 終端状態まで |
| Long Term Memory | プロジェクト知識、履歴結果、学習済み嗜好 | Project | 無期限（明示削除まで） |

- **ATH-MEM-001** (MUST) 3 階層を実装上分離しなければならない。
- **ATH-MEM-002** (MUST) 各階層はサイズ上限を持たなければならない。上限到達時の退避方針
  （Short Term は古い順に要約、Working Memory は完了 PlanStep を要約）を実装しなければならない。
- **ATH-MEM-003** (MUST) Long Term Memory への書き込みは、Core Agent の明示的判断または人間の
  指示に基づかなければならない。Agent が自動的に無制限へ蓄積してはならない。
- **ATH-MEM-004** (MUST NOT) 秘密情報（認証情報、トークン、鍵）を Memory のいずれの階層へも
  平文で保存してはならない。
- **ATH-MEM-005** (MUST) Long Term Memory の各記録は出典（Task ID、Event ID、作成日時）を保持
  しなければならない。
- **ATH-MEM-006** (MUST) Memory 記録は削除可能でなければならない。削除は Event として記録する。
- **ATH-MEM-007** (MUST) Working Memory は、Core Agent Instance の再起動時に文脈を再構成できる
  だけの情報を保持しなければならない（ATH-CORE-029、ATH-CORE-046）。最低限、以下を含む。

```text
現行 Plan（revision 付き）
未完了 PlanStep
未終端の委任（plan_step_id / instance_id / attempt / 状態）
各 Agent の最新報告
保留中の承認、および GRANTED かつ未消費の承認
直近の特権操作拒否（PrivilegedRequestDenied）
SUSPENDED Instance のスナップショット参照
```

- **ATH-MEM-008** (MUST) 退避時の要約は LLM 処理であるため、Core Host が要約専用の Agent
  （`memory.summarizer` で指定する Agent 定義）へ委任して実施しなければならない。Core Host 内の
  決定論的処理として要約を実装してはならない（ATH-CORE-022）。要約を利用できない場合は、古い
  記録を破棄して参照（Event ID / Result ID）のみを残す。
- **ATH-MEM-009** (MUST) `short_term_max_tokens` の計数に用いるトークナイザを設定で指定しなければ
  ならない（`memory.tokenizer`）。Provider 既定に暗黙に依存してはならない。
- **ATH-MEM-010** (MUST) 短期記憶は Agent Process 側に存在するため、ATHENA は上限管理と
  スナップショット取得のみを行い、内容の逐次保存を要求しない。`SUSPENDED` 時は要約を
  Working Memory へ保存する（ATH-STATE-019）。
- **ATH-MEM-011** (MUST) Memory の参照・削除のため `GET /api/v1/memory` と
  `DELETE /api/v1/memory/{id}` を提供しなければならない（ATH-MEM-006）。
- **ATH-MEM-012** (MUST) v1 の Long Term Memory の保存先は、第 27 章の永続ストア（`redb`）の
  **同一 DB** とする。
  別ファイルおよび外部ベクタストアを v1 の要件としてはならない。Memory へのアクセスは
  Repository インターフェース（ATH-PERS-007）経由に限り、保存先の実装詳細を Core の他モジュールへ
  露出させてはならない。類似検索の要求が発生した時点で、このインターフェースを保ったまま外部
  ベクタストアを追加する。

---

### 14. Runtime と agent-cli Adapter

#### 14.1 Runtime の役割

Runtime は Agent の実行環境を抽象化する。Core は Runtime Interface のみを利用する。

```text
ATHENA Core
    |
Runtime Interface
    |
agent-cli Adapter
    |
agent-cli
    |
Agent Process
```

#### 14.2 Runtime Interface

```rust
pub struct Runtime {
  readonly type: RuntimeType;

  start(agent: AgentDefinition, context: StartContext): Promise<RuntimeInstanceRef>;
  send(instance: RuntimeInstanceRef, message: Message): Promise<void>;
  receive(instance: RuntimeInstanceRef): AsyncIterable<Message>;
  status(instance: RuntimeInstanceRef): Promise<RuntimeInstanceStatus>;
  stop(instance: RuntimeInstanceRef, reason: StopReason): Promise<void>;
  collectResult(instance: RuntimeInstanceRef): Promise<Result>;
  capabilities(): RuntimeCapabilities;
}
```

- **ATH-RT-001** (MUST) Runtime Interface は start / send / receive / status / stop /
  collect_result / capabilities を提供しなければならない。
- **ATH-RT-002** (MUST) Core は Runtime Interface 経由でのみ Agent を操作しなければならない。
- **ATH-RT-003** (MUST) Runtime Interface は Adapter 実装より先に定義しなければならない
  （ATH-DEV-002）。
- **ATH-RT-004** (MUST) すべての Adapter は Interface の契約テスト（第 34 章）に合格しなければ
  ならない。
- **ATH-RT-005** (MUST) `capabilities()` は、その Runtime が提供する分離境界（プロセス、
  コンテナ、リモート）とサポートする Tool を申告しなければならない。
- **ATH-RT-006** (MUST) Runtime は起動タイムアウトと実行タイムアウトを強制しなければならない
  （第 20 章）。

#### 14.3 将来の Runtime

```text
Runtime
|
+-- AgentCLI Runtime      初期実装
+-- LocalProcess Runtime
+-- Docker Runtime
+-- SSH Runtime
+-- Kubernetes Runtime
+-- Cloud Runtime
```

- **ATH-RT-007** (MUST NOT) Core は Runtime の種別に依存した分岐を持ってはならない。
- **ATH-RT-008** (MUST) Runtime 追加時に Core の変更を必要としない構造でなければならない。

#### 14.4 agent-cli Adapter

`agent-cli` は Agent を実際に実行するための低レベル実行基盤である。`agent-cli` は ATHENA Core
そのものではない。

```text
ATHENA Core Agent（agent-cli #0）
       | 委任要求
       v
Core Host（検査：第 7.5.2 節）
       |
Agent Manager
       |
ATHENA Runtime
       |
agent-cli Adapter
       |
       |  Instance ごとのディレクトリを cwd として起動（ATH-OPS-009）
       |  .athena/var/instances/<instance_id>/
       +-- .aiprj/          cwd 相対で解決
       +-- .agent-cli/      Instance ごと。原本から生成（ATH-ACLI-012、ATH-ACLI-013）
       |     +-- persona.md  persona_file で指す
       +-- skills/          agent-cli に配置機構なし
       |
       v
agent-cli（cwd = Instance のディレクトリ）
       |
       v
Agent Process
       |
       +-- 作業対象: <PROJECT_ROOT> の source / artifacts（絶対パス：ATH-OPS-010）
       +-- 読み取り: <PROJECT_ROOT>/.aiprj/ の作業規約と 3 文書
       +-- 書き込み: 自身の .aiprj/（AI_PRJ_TASKS.md、AI_LOG/）
```

- **ATH-ACLI-003** (MUST) Adapter は `agent-cli` の設定・コマンド定義・Persona・Skill を
  **Agent Instance 単位**で構成し、当該 Instance の cwd 配下へ配置しなければならない
  （ATH-OPS-009、ATH-ACLI-007）。コマンド定義は Instance ごとに生成し、作業対象への参照は
  絶対パスで埋め込む（ATH-OPS-010）。コマンド定義は手順ファイルであり、Adapter は Persona と
  `instruction` にその絶対パスを明示しなければならない（第 2.2.3 節、ATH-ACLI-017、
  ATH-AIPRJ-028）。
- **ATH-ACLI-004** (MUST) Adapter は Agent 起動前に Workspace の `aiprj` 初期化状態を確認しなければ
  ならない（第 36 章）。
- **ATH-ACLI-005** (MUST) Adapter は `agent-cli` の異常終了を検出し、`RuntimeError` として Core へ
  報告しなければならない。
- **ATH-ACLI-006** (MUST NOT) Adapter は Core の判断（Agent 選択、Skill 選択、再計画）を
  代行してはならない。

#### 14.5 Runtime Instance の資源管理

- **ATH-RT-012** (MUST) Agent Instance と Runtime Instance は 1 対 1 としなければならない。
  複数の Agent Instance を 1 つの Runtime Instance（`agent-cli` プロセス）へ多重化しては
  ならない。これにより Agent 間の文脈混在と障害の波及を防ぐ。
- **ATH-RT-013** (MUST) Core Agent の Runtime Instance は下位 Agent の Runtime Instance と
  分離しなければならない。一方の異常終了が他方へ波及してはならない。
- **ATH-RT-009** (MUST) Runtime は同時実行 Instance 数の上限を持たなければならない（第 18 章）。
- **ATH-RT-010** (MUST) 終端状態に達した Instance の資源を解放しなければならない。
- **ATH-RT-011** (MUST) Core の再起動後、Runtime Manager は既存 Instance の再接続または安全な
  停止を実施しなければならない（第 31 章 ATH-NFR-003）。

---

### 15. Human-in-the-Loop

ATHENA は完全自律ではなく、人間による承認点を実行経路へ組み込む。

#### 15.1 承認対象

| 分類 | 例 | 既定リスク |
|---|---|---|
| 本番環境変更 | 本番設定変更、デプロイ | high |
| 公開操作 | `git push`、リリース公開 | high |
| 破壊的操作 | ファイル大量削除、データ削除 | high |
| 外部サービス変更 | 外部 API への書き込み | high |
| 課金操作 | 有償リソースの確保 | high |
| 機密情報アクセス | 認証情報の読み取り | high |
| 高リスク Skill 利用 | `production-deploy` | high |
| 権限昇格 | allowed_tools 外の Tool 要求 | high |

- **ATH-HITL-001** (MUST) リスク `high` に分類される操作は、実行前に人間の承認を取得しなければ
  ならない。
- **ATH-HITL-002** (MUST) リスク分類は実装の設定として定義し、実行時に参照しなければならない。
  Agent の自己申告のみに依存してはならない。

#### 15.2 承認フロー

```text
Agent / Core Agent
   |
   | approval_request
   v
Approval Manager → Event(ApprovalRequested) → CLI / Studio
   |
   | Human: approve / reject
   v
Approval Manager → Event(ApprovalGranted | ApprovalRejected)
   |
   v
実行 / 中止
```

```text
ATHENA:
本番環境へ変更を適用する必要があります。
実行しますか？

Human:
Yes

ATHENA:
実行します。
```

- **ATH-HITL-003** (MUST) 承認要求は要求元 Agent、対象操作、理由、影響範囲、リスク分類を含まなければ
  ならない。
- **ATH-HITL-004** (MUST) 承認要求には期限を設定しなければならない。既定値は 1 時間とする。
- **ATH-HITL-005** (MUST) 期限内に応答がない場合、既定動作は**拒否**としなければならない。
  無応答を承認とみなしてはならない。
- **ATH-HITL-006** (MUST) 承認・拒否・期限切れを Event として記録し、承認者の識別子、日時、
  対象操作を監査記録として保存しなければならない。
- **ATH-HITL-007** (MUST) 1 回の承認は要求された単一操作に対してのみ有効としなければならない。
  同種操作への包括的承認は、明示的に範囲と期限を指定した場合に限る。
- **ATH-HITL-008** (MUST) 承認待ちの Task は `WAITING_APPROVAL` 状態とし、その間 Runtime 資源を
  占有し続けない構造とする（第 17.1 節）。
- **ATH-HITL-009** (MUST) 第 7.2 節の反復上限到達、適合 Agent 不在（ATH-AGENT-005）、完了条件
  抽出不能（ATH-CORE-014）の各場合、Core Agent は人間へエスカレーションしなければならない。

#### 15.3 承認要求の信頼性

承認要求の文面は信頼境界の外側にある Core Agent が生成する（ATH-SEC-021）。したがって、人間が
読んで判断する内容を Agent の記述だけに依存させてはならない。

- **ATH-HITL-010** (MUST) Core Host は承認対象操作の**正規記述子**を自ら生成しなければならない。
  記述子には Tool 名、引数、対象パスまたはホスト、設定から決定したリスク分類、要求元 Instance を
  含める。
- **ATH-HITL-011** (MUST) CLI および Studio は正規記述子を主たる表示内容としなければならない。
  Agent が記述した `reason` / `impact` は、信頼できない情報であることを明示して併記する。
- **ATH-HITL-012** (MUST) `scope.pattern` は設定で定義した列挙（`policy.approval_scopes`）から
  選択しなければならない。Agent が指定した任意パターンを受理してはならない。
- **ATH-HITL-013** (MUST) `scope.expires_at` は設定の上限 TTL（`policy.max_approval_ttl`、
  既定 24 時間）を超えてはならない。
- **ATH-HITL-014** (MUST) 承認の消費時に、Core Host は対象操作のリスク分類を再判定しなければ
  ならない。承認時に提示した分類を超える操作は、新たな承認を要求しなければならない。
- **ATH-HITL-015** (MUST) `ApprovalConsumed` Event の payload に、消費した操作の正規記述子を
  記録しなければならない。
- **ATH-HITL-016** (MUST) 承認待ちの Agent Instance 数の上限（既定: `max_concurrent_instances` の
  25%）を設けなければならない。上限到達時は新規の承認要求を発生させる委任を保留する
  （ATH-STATE-019 の `SUSPENDED` と併せて配送の枯渇を防ぐ）。

#### 15.4 人間への確認（Clarification）

承認（高リスク操作の可否）とは別に、Core Agent が判断材料を人間へ問う経路を定義する。

- **ATH-HITL-017** (MUST) Core Agent は `ask_human` 要求（第 7.5.5 節）により人間へ確認できなければ
  ならない。対象 Task は `WAITING_INPUT` へ遷移する（第 17.1 節）。
- **ATH-HITL-018** (MUST) 確認要求と回答を `ClarificationRequested` / `ClarificationAnswered`
  Event として記録しなければならない。
- **ATH-HITL-019** (MUST) 確認には期限（`timeout.clarification`、既定 24 時間）を設け、
  期限切れ時は Task を `FAILED` としなければならない。無応答を任意の回答とみなしてはならない。
- **ATH-HITL-020** (MUST) 回答は `POST /api/v1/tasks/{id}/answer` および `athena chat` から
  投入できなければならない（第 22.3 節、第 24.2 節）。

---

## Part III 実行モデル

### 16. Task と Plan

#### 16.1 Task

Task は利用者から要求された仕事である。

```text
「このFPGA RTLのタイミング問題を解析して修正案を作成する」
```

Task は SubTask へ分解できる。

```text
Task
|
+-- SubTask A: RTL 解析
+-- SubTask B: Timing 解析
+-- SubTask C: 修正案作成
+-- SubTask D: 検証
```

- **ATH-TASK-001** (MUST) Task は木構造を成し、SubTask は親 Task への参照を持たなければならない。
- **ATH-TASK-002** (MUST) Task は目的、制約、完了条件を保持しなければならない。
- **ATH-TASK-003** (MUST) Task の状態は第 17.1 節の状態機械に従わなければならない。
- **ATH-TASK-004** (MUST) 親 Task は、すべての SubTask が終端状態に達するまで `SUCCESS` へ
  遷移してはならない。
- **ATH-TASK-005** (MUST) Task の分解・再分解は Core Agent の判断として記録しなければならない。
- **ATH-TASK-006** (MUST) Task 分解の粒度と Agent 階層を対応付けなければならない。`SubTask` は
  Lead の担当単位、`PlanStep` は Worker の担当単位とする（第 8.2.1 節）。
- **ATH-TASK-007** (MUST) `SubTask` は、単一の Lead が完了責任を負える粒度へ分解しなければ
  ならない。1 つの `SubTask` が複数領域にまたがる場合は、Director 単位へ再分解する。

#### 16.2 Plan が主、Workflow は表現

ATHENA は以下の固定 Workflow を基本モデルとしない。

```text
A → B → C → D
```

代わりに以下を基本モデルとする。

```text
目的
 ↓
現在の状況
 ↓
次に必要な行動を判断
 ↓
Agent / Skill / Tool を選択
 ↓
実行
 ↓
結果を観測
 ↓
次の行動を再判断
```

Workflow は、Core Agent が生成した Plan の**依存関係表現**として扱う。

- **ATH-PLAN-001** (MUST) 実行の主体モデルは Plan とし、Plan は実行中に変更可能でなければならない。
- **ATH-PLAN-002** (MUST NOT) 固定 Workflow 定義を実行の前提条件としてはならない。Workflow 定義を
  持たない Task も実行できなければならない。
- **ATH-PLAN-003** (MAY) Plan の依存関係を DAG として表現し、並列実行・順次実行・条件分岐・
  Retry・失敗処理の解析に利用する。

```text
Node = PlanStep
Edge = Dependency
```

DAG は Plan の**表現**であり、実行モデルの主役ではない。依存関係と並列性を解析するための
表現形式として扱い、固定 Workflow を一次モデルとしてはならない（ATH-PLAN-001）。

#### 16.3 Plan 構造

```rust
pub struct Plan {
  id: PlanId,
  task_id: TaskId,
  revision: u64,             // 変更ごとに加算
  goal: String,
  steps: Vec<PlanStep>,
  created_at: String,
  superseded_by: Option<PlanId>,
}

pub struct PlanStep {
  id: PlanStepId,
  plan_id: PlanId,
  order: u64,
  intent: String,               // この step で達成すること
  depends_on: Vec<PlanStepId>,
  assigned_agent: Option<AgentId>,
  skill_set: Vec<SkillRef>,
  state: PlanStepState,
  result_id: Option<ResultId>,
}
```

- **ATH-PLAN-004** (MUST) Plan 変更は新しい revision として記録し、旧 revision を保存しなければ
  ならない。Plan を破壊的に上書きしてはならない。
- **ATH-PLAN-005** (MUST) Plan の新 revision の確定を `PlanRevised` Event として記録しなければ
  ならない。Task 状態の遷移は `TaskReplanning` で表す（第 17.1 節）。
- **ATH-PLAN-006** (MUST NOT) `depends_on` に循環を含んではならない。生成時に検証する。

#### 16.4 Dynamic Planning の例

初期 Plan：

```text
+-- RTL Analysis
+-- Timing Analysis
+-- Fix
+-- Verify
```

Timing Agent の報告：

```text
「RTLではなくSDC制約が原因」
```

Core Agent による変更後：

```text
+-- Constraint Analysis
+-- SDC Fix
+-- Re-run Timing
+-- Verify
```

- **ATH-PLAN-007** (MUST) Core Agent は Agent の報告内容に基づき、未実行 PlanStep を追加・削除・
  置換できなければならない。
- **ATH-PLAN-008** (MUST) 実行済み PlanStep の記録を変更してはならない。

---

### 17. 状態遷移

本章は Task、PlanStep、Agent Instance、Skill、Approval の状態を規範的に定義する。状態と遷移は
図ではなく以下の遷移表で表現する（ATH-STATE-002）。

各遷移表の **要求元** 列は、その遷移を要求する主体を示す。遷移を実行し記録する主体は常に
Core Host の State Manager である（ATH-STATE-001）。Agent、Core Agent、Runtime は要求元には
なれるが、状態を直接書き換えることはできない（ATH-CORE-027、ATH-SEC-022）。

- **ATH-STATE-001** (MUST) 状態遷移は Core Host の State Manager が一元管理しなければならない。
  Agent、Core Agent Process、Runtime が状態を直接書き換えてはならない。
- **ATH-STATE-014** (MUST) 遷移要求には遷移前状態（`expected_state`）を含めなければならない。
  実際の状態と一致しない場合は `ValidationError` として拒否する（楽観的並行制御）。
- **ATH-STATE-002** (MUST) 遷移表に定義されていない遷移要求は `ValidationError` として拒否しなければ
  ならない。
- **ATH-STATE-003** (MUST) すべての遷移は対応する Event を発行しなければならない。

#### 17.1 Task State

状態集合：

```text
CREATED
QUEUED
RUNNING
WAITING_APPROVAL
WAITING_INPUT
EVALUATING
REPLANNING
RETRYING
CANCELLING
SUCCESS
FAILED
CANCELLED
```

終端状態：`SUCCESS` / `FAILED` / `CANCELLED`

Task 状態は、その Task に属する PlanStep の状態（第 17.5 節）から導出される集約状態である。
複数 PlanStep が並行する場合、以下の優先順位で Task 状態を決定する。

```text
CANCELLING > WAITING_APPROVAL > WAITING_INPUT > RETRYING > REPLANNING
           > EVALUATING > RUNNING > QUEUED
```

| From | To | Trigger | 要求元 | Event |
|---|---|---|---|---|
| CREATED | QUEUED | Task 投入 | Core Agent / API | `TaskQueued` |
| CREATED | CANCELLED | 実行前の取消 | User | `TaskCancelled` |
| QUEUED | RUNNING | 実行枠確保 | Scheduler | `TaskStarted` |
| QUEUED | CANCELLING | 取消要求 | User | `TaskCancelling` |
| RUNNING | WAITING_APPROVAL | 承認要求発生 | Approval Manager | `ApprovalRequested` |
| RUNNING | WAITING_INPUT | 人間への確認が必要 | Core Agent | `ClarificationRequested` |
| RUNNING | EVALUATING | Result 受領 | Core Agent | `TaskResultReceived` |
| RUNNING | CANCELLING | 取消要求 | User | `TaskCancelling` |
| RUNNING | FAILED | 回復不能エラー | Core Agent | `TaskFailed` |
| WAITING_APPROVAL | RUNNING | 承認取得 | Human | `ApprovalGranted` |
| WAITING_APPROVAL | EVALUATING | 評価中に要求した承認の取得 | Human | `ApprovalGranted` |
| WAITING_APPROVAL | REPLANNING | 承認拒否（代替手段を検討） | Core Agent | `TaskReplanning` |
| WAITING_APPROVAL | FAILED | 期限切れ | Approval Manager | `ApprovalExpired` |
| WAITING_APPROVAL | CANCELLING | 取消要求 | User | `TaskCancelling` |
| WAITING_INPUT | RUNNING | 回答受領 | Human | `ClarificationAnswered` |
| WAITING_INPUT | REPLANNING | 回答により計画変更が必要 | Core Agent | `TaskReplanning` |
| WAITING_INPUT | CANCELLING | 取消要求 | User | `TaskCancelling` |
| WAITING_INPUT | FAILED | 回答期限切れ | Core Host | `TaskFailed` |
| EVALUATING | SUCCESS | 完了条件充足 | Core Agent | `TaskCompleted` |
| EVALUATING | WAITING_APPROVAL | 適用に承認が必要 | Approval Manager | `ApprovalRequested` |
| EVALUATING | REPLANNING | 不充足かつ予算残あり | Core Agent | `TaskReplanning` |
| EVALUATING | RETRYING | 再試行可能な失敗 | Core Agent | `TaskRetrying` |
| EVALUATING | FAILED | 予算消尽 | Core Agent | `TaskFailed` |
| EVALUATING | CANCELLING | 取消要求 | User | `TaskCancelling` |
| REPLANNING | RUNNING | 新 Plan 確定 | Core Agent | `TaskStarted` |
| REPLANNING | WAITING_INPUT | 代替案の選択を人間へ確認 | Core Agent | `ClarificationRequested` |
| REPLANNING | FAILED | 代替計画なし | Core Agent | `TaskFailed` |
| REPLANNING | CANCELLING | 取消要求 | User | `TaskCancelling` |
| RETRYING | RUNNING | 再実行開始 | Scheduler | `TaskStarted` |
| RETRYING | FAILED | 試行上限到達 | Core Agent | `TaskFailed` |
| RETRYING | CANCELLING | 取消要求 | User | `TaskCancelling` |
| CANCELLING | CANCELLED | 対象 Instance の停止完了 | Runtime Manager | `TaskCancelled` |
| CANCELLING | CANCELLED | 停止タイムアウト後の強制終了 | Core Host | `TaskCancelled` |

- **ATH-STATE-004** (MUST) `EVALUATING` を経由せずに `RUNNING` から `SUCCESS` へ遷移しては
  ならない。評価されていない Result を成功として扱ってはならない。
- **ATH-STATE-005** (MUST) `CANCELLING` 中は、**当該 Task の部分木に対して**新規 Agent Instance を
  起動してはならない。他 Task の実行を妨げてはならない。
- **ATH-STATE-015** (MUST) 承認拒否は `REPLANNING` へ遷移させ、代替手段の検討を可能にしなければ
  ならない（ATH-FAIL-011）。`FAILED` へ直行させてはならない。期限切れは `FAILED` とする
  （ATH-HITL-005 の既定拒否を維持するため）。
- **ATH-STATE-016** (MUST) `CANCELLING` には停止タイムアウト（`timeout.stop`、既定 60 秒）を
  適用し、超過時は強制終了して `CANCELLED` へ遷移しなければならない。`CANCELLING` に留まり続けては
  ならない。強制終了した場合、Task の Result に「副作用の確定状態が不明」である旨を記録する。
- **ATH-STATE-017** (MUST) 取消対象の停止集合に Core Agent Instance（instance #0）を含めては
  ならない。instance #0 は ATHENA 停止時にのみ停止する。
- **ATH-STATE-018** (MUST) 取消の発生を Core Agent へ制御 Message（`cancel`）として通知しなければ
  ならない（第 10.1 節）。Event の発行のみで済ませてはならない。

#### 17.2 Agent Instance State

状態集合：

```text
CREATED
STARTING
IDLE
BUSY
WAITING
SUSPENDED
STOPPING
STOPPED
FAILED
```

終端状態：`STOPPED`
準終端状態：`FAILED`（資源回収のため `STOPPING` への遷移を持つ）

| From | To | Trigger | 要求元 | Event |
|---|---|---|---|---|
| CREATED | STARTING | 起動要求 | Runtime Manager | `AgentStarting` |
| STARTING | IDLE | 起動完了 | Runtime | `AgentStarted` |
| STARTING | FAILED | 起動タイムアウト / 起動失敗 | Runtime | `AgentFailed` |
| IDLE | BUSY | instruction 受領 | Message Broker | `AgentMessageReceived` |
| IDLE | STOPPING | 停止要求 | Runtime Manager | `AgentStopping` |
| IDLE | FAILED | プロセス異常終了 | Runtime | `AgentFailed` |
| BUSY | WAITING | 下位 Agent の応答待ち | Agent | `AgentWaiting` |
| BUSY | IDLE | report 送信完了 | Agent | `AgentMessageSent` |
| BUSY | STOPPING | 停止要求（取消・強制停止） | Runtime Manager | `AgentStopping` |
| BUSY | FAILED | 実行エラー / 実行タイムアウト | Runtime | `AgentFailed` |
| WAITING | BUSY | 応答受領 | Message Broker | `AgentMessageReceived` |
| WAITING | SUSPENDED | 承認待ちへの移行 | Approval Manager | `AgentSuspended` |
| WAITING | STOPPING | 停止要求（取消・強制停止） | Runtime Manager | `AgentStopping` |
| WAITING | FAILED | 応答期限切れ | Runtime Manager | `AgentFailed` |
| SUSPENDED | BUSY | 承認取得後の再開 | Approval Manager | `AgentResumed` |
| SUSPENDED | STOPPING | 承認拒否 / 期限切れ / 取消 | Runtime Manager | `AgentStopping` |
| STOPPING | STOPPED | プロセス終了確認 | Runtime | `AgentStopped` |
| STOPPING | STOPPED | 停止タイムアウト後の強制終了 | Core Host | `AgentStopped` |
| FAILED | STOPPING | 資源回収 | Runtime Manager | `AgentStopping` |

- **ATH-STATE-006** (MUST) `BUSY` の Agent Instance へ新たな `instruction` を送信してはならない。
  Broker は滞留させるか `TransientError` を返す。`report` / `complete` / `error` / 制御 Message は
  状態にかかわらず受理しなければならない（ATH-MSG-012）。
- **ATH-STATE-007** (MUST) `FAILED` の Instance は資源回収後に `STOPPED` へ到達しなければならない。
- **ATH-STATE-019** (MUST) `SUSPENDED` では Agent Process を停止し、Runtime 資源を解放しなければ
  ならない（ATH-HITL-008）。再開のため、短期記憶の要約と凍結済み Skill Set を Working Memory の
  スナップショットとして `instance_id` 単位で保存する。
- **ATH-STATE-020** (MUST) `SUSPENDED` の Instance を同時実行上限の計上対象から除外しなければ
  ならない（第 18.3 節）。
- **ATH-STATE-021** (MUST) `SUSPENDED` から再開する Instance は、凍結時と同一の Skill Set
  （同一 ID と版）で起動しなければならない（ATH-SKILL-010）。

##### 17.2.1 Core Agent Instance の特則

Core Agent Instance（instance #0）は常駐かつ複数 Task を横断するため、下位 Agent の状態機械を
そのまま適用できない。以下を特則とする。

| 項目 | 下位 Agent Instance | Core Agent Instance |
|---|---|---|
| 生存期間 | PlanStep 単位 | セッション単位（常駐） |
| `task_id` の束縛 | 必須 | なし（複数 Task を横断） |
| `BUSY` の粒度 | Instance 単位 | `thread_id` 単位（並行受理） |
| 応答期限切れ | `WAITING → FAILED` | 観測として配送（`FAILED` にしない） |
| `FAILED` 時 | Core Agent が原因分析し再計画 | Core Host が決定論的に再起動（第 7.5.3 節） |
| `SUSPENDED` | 承認待ちで適用 | 適用しない |
| `STOPPED` 到達 | 通常の完了 | ATHENA 停止時のみ |
| 取消時の停止 | 対象部分木を停止 | 停止しない（ATH-STATE-017） |
| 資源枠 | Scheduler の上限内 | 専用枠を確保（ATH-SCHED-008） |

- **ATH-STATE-012** (MUST) Core Agent Instance は `task_id` へ束縛せずに管理しなければならない。
- **ATH-STATE-013** (MUST NOT) Core Agent Instance の `FAILED` を、通常の Agent 失敗として
  再計画の入力に用いてはならない。第 7.5.3 節の手続きを適用する。
- **ATH-STATE-022** (MUST) Core Agent Instance の `BUSY` は `thread_id` 単位で管理しなければ
  ならない。ある Task の処理中に、他 Task の `report` / `complete` / `error` を受理できなければ
  ならない。Instance 単位の `BUSY` によって Task を直列化してはならない。
- **ATH-STATE-023** (MUST NOT) Core Agent Instance を応答期限切れ（`WAITING → FAILED`）で
  失敗させてはならない。下位 Agent の応答期限切れは `AgentMessageTimeout` として Core Agent へ
  配送し、再計画の入力としなければならない（ATH-FAIL-011）。
- **ATH-STATE-024** (MUST) Core Agent Instance の同時処理 thread 数の上限を設けなければならない
  （既定値は `scheduler.max_concurrent_tasks` と同一）。

#### 17.3 Skill State

状態集合：

```text
DISCOVERED
VALIDATING
AVAILABLE
ENABLED
DISABLED
UPDATING
FAILED
REMOVED
```

終端状態：`REMOVED`

| From | To | Trigger | Actor | Event |
|---|---|---|---|---|
| DISCOVERED | VALIDATING | 検証開始 | Skill Validator | `SkillValidating` |
| VALIDATING | AVAILABLE | 検証成功 | Skill Validator | `SkillValidated` |
| VALIDATING | FAILED | 検証失敗 | Skill Validator | `SkillValidationFailed` |
| AVAILABLE | ENABLED | 有効化 | Admin / API | `SkillEnabled` |
| ENABLED | DISABLED | 無効化 | Admin / API | `SkillDisabled` |
| DISABLED | ENABLED | 再有効化 | Admin / API | `SkillEnabled` |
| ENABLED | UPDATING | 更新要求 | Skill Manager | `SkillUpdating` |
| DISABLED | UPDATING | 更新要求 | Skill Manager | `SkillUpdating` |
| AVAILABLE | UPDATING | 更新要求 | Admin / API | `SkillUpdating` |
| UPDATING | VALIDATING | 新版取得完了 | Skill Manager | `SkillValidating` |
| UPDATING | FAILED | 取得失敗 / 検証失敗 | Skill Manager | `SkillValidationFailed` |
| UPDATING | AVAILABLE | 更新完了 | Skill Manager | `SkillUpdated` |
| FAILED | VALIDATING | 再検証 | Admin / API | `SkillValidating` |
| AVAILABLE | REMOVED | 削除 | Admin / API | `SkillRemoved` |
| ENABLED | REMOVED | 削除（無効化を経て） | Admin / API | `SkillRemoved` |
| DISABLED | REMOVED | 削除 | Admin / API | `SkillRemoved` |
| FAILED | REMOVED | 削除 | Admin / API | `SkillRemoved` |

- **ATH-STATE-008** (MUST) `ENABLED` 以外の Skill を Core Agent の選択候補に含めてはならない。
- **ATH-STATE-009** (MUST) 実行中 Agent Instance が使用中の Skill 版は、`UPDATING` や `REMOVED` の
  影響を受けてはならない（ATH-SKILL-010）。
- **ATH-STATE-025** (MUST NOT) Skill のライフサイクル遷移の要求元に Core Agent を含めてはならない。
  Core Agent は `ENABLED` の Skill を**選択**できるのみとする（ATH-STATE-008、ATH-SKILL-006）。
  有効化・無効化・更新・削除は `admin` ロールの操作とする（ATH-API-015）。
- **ATH-STATE-026** (MUST) Skill 状態は `(id, version)` 単位で管理しなければならない。第 12.3 節の
  Catalog の `status` は本節の状態集合と同一の値域とする。

#### 17.4 Approval State

| From | To | Trigger | 要求元 | Event |
|---|---|---|---|---|
| （なし） | REQUESTED | 承認要求の生成 | Core Agent / Policy Manager | `ApprovalRequested` |
| REQUESTED | GRANTED | 承認 | Human | `ApprovalGranted` |
| REQUESTED | REJECTED | 拒否 | Human | `ApprovalRejected` |
| REQUESTED | EXPIRED | 期限到達 | Approval Manager | `ApprovalExpired` |
| GRANTED | CONSUMED | 対象操作の実行（残回数 0） | Core Host | `ApprovalConsumed` |
| GRANTED | EXPIRED | 消費前に `scope.expires_at` 到達 | Approval Manager | `ApprovalExpired` |

終端状態：`REJECTED` / `EXPIRED` / `CONSUMED`

- **ATH-STATE-010** (MUST) `scope` が `operation_only` の Approval は 1 回のみ消費可能としなければ
  ならない。`pattern` スコープの Approval は `uses_remaining` を持ち、残回数が 0 になった時点で
  `CONSUMED` へ遷移する（第 15.3 節）。
- **ATH-STATE-011** (MUST) `EXPIRED` は拒否と同等に扱わなければならない（ATH-HITL-005）。
- **ATH-STATE-027** (MUST) Approval の消費は Core Host が実行しなければならない。Core Agent は
  消費を**要求**できるのみとする（ATH-CORE-026）。

#### 17.5 PlanStep State

PlanStep は Scheduler の配送単位であり、Worker の担当単位である（第 8.2.1 節）。

状態集合：

```text
PENDING
READY
RUNNING
SUSPENDED
ORPHANED
SUCCESS
FAILED
SKIPPED
```

終端状態：`SUCCESS` / `FAILED` / `SKIPPED`

| From | To | Trigger | 要求元 | Event |
|---|---|---|---|---|
| PENDING | READY | 依存 PlanStep がすべて終端かつ成功 | Scheduler | `PlanStepReady` |
| PENDING | SKIPPED | 再計画で不要化 | Core Agent | `PlanStepSkipped` |
| READY | RUNNING | Agent Instance 起動完了 | Scheduler | `PlanStepStarted` |
| READY | SKIPPED | 再計画で不要化 | Core Agent | `PlanStepSkipped` |
| RUNNING | SUSPENDED | 承認待ちで Instance を解放 | Approval Manager | `AgentSuspended` |
| RUNNING | SUCCESS | Result 受領かつ評価合格 | Core Agent | `PlanStepCompleted` |
| RUNNING | FAILED | Result 失敗 / タイムアウト / Instance 失敗 | Core Host | `PlanStepFailed` |
| RUNNING | ORPHANED | 担当 Instance が消失（再起動・heartbeat 途絶） | Core Host | `PlanStepOrphaned` |
| SUSPENDED | RUNNING | 承認取得後の再開 | Approval Manager | `AgentResumed` |
| SUSPENDED | FAILED | 承認拒否 / 期限切れ | Approval Manager | `PlanStepFailed` |
| ORPHANED | READY | Verification 完了後の再試行許可 | Core Host | `PlanStepReady` |
| ORPHANED | FAILED | 再試行不可 | Core Agent | `PlanStepFailed` |
| FAILED | READY | 再試行（新 attempt） | Scheduler | `PlanStepReady` |

- **ATH-STATE-028** (MUST) `RUNNING` の PlanStep を再計画で削除してはならない。削除できるのは
  `PENDING` と `READY` のみとする（ATH-PLAN-007）。`RUNNING` の中止は取消手続きに従う。
- **ATH-STATE-029** (MUST) `ORPHANED` の PlanStep は、副作用の確定状態が不明であるため、
  Verification（ATH-EVAL-002）を経てからのみ再試行してよい（ATH-CORE-048）。
- **ATH-STATE-030** (MUST) PlanStep の各 attempt は独立した Agent Instance に対応させなければ
  ならない（ATH-FAIL-007）。

---

### 18. Scheduler と並行制御

Scheduler は Task / PlanStep / Agent Instance の実行順序と資源を管理する。Scheduler は実行順序の
管理者であり、**何を実行すべきかの判断者ではない**。

#### 18.1 責務

```text
Queue 管理
優先度
依存関係の充足判定
並列実行
資源上限
Retry の投入
Timeout の検出
```

#### 18.2 依存関係と並列実行

```text
Task A
  |
  +-- Task B
  |
  +-- Task C
          |
          +-- Task D
```

B と C は並列実行できる。

```text
        +-- B --+
A ------|       |------ D
        +-- C --+
```

- **ATH-SCHED-001** (MUST) Scheduler は依存関係が充足した PlanStep のみを実行対象としなければ
  ならない。
- **ATH-SCHED-002** (MUST) 依存関係のない PlanStep は並列実行可能としなければならない。

#### 18.3 優先度と並行上限

| 優先度 | 用途 |
|---|---|
| `high` | 人間が明示的に優先指定した Task、障害対応 |
| `normal` | 既定 |
| `low` | 背景調査、将来のための情報収集 |

| 上限項目 | 既定値 | 設定キー |
|---|---|---|
| 同時実行 Task 数 | 8 | `scheduler.max_concurrent_tasks` |
| 同時実行 Agent Instance 数（全体） | 16 | `scheduler.max_concurrent_instances` |
| Runtime 単位の同時 Instance 数 | 16 | `runtime.<type>.max_instances` |
| Task 単位の同時 Instance 数 | 4 | `scheduler.max_instances_per_task` |
| Queue 長 | 1000 | `scheduler.max_queue_length` |
| Core Agent 用の予約枠 | 2（常時確保。終了中と後継の重複を許容） | `scheduler.reserved_core_agent_slots` |
| 承認待ち Instance の上限 | `max_concurrent_instances` の 25% | `scheduler.max_waiting_approval_ratio` |

- **ATH-SCHED-003** (MUST) Scheduler は上表の上限を強制しなければならない。
- **ATH-SCHED-015** (MUST) 全項目は設定で変更可能としなければならない。既定値は Phase 1
  （第 33.1 節）の計測結果に基づき本書の改訂として確定する。計測前の値を根拠に上限を固定実装して
  はならない。
  **確定（2026-10-18）**: 上表の値を v1 の確定値とする。根拠: (i) Phase 1 の飽和計測
  (`ATH-SCHED-009` の queue 保持・16 Instance 飽和・Task 齢 0 — AI_LOG 017、P1-46);
  (ii) NFR-01 の 16 並行 Instance ラン(状態混入なし); (iii) 当該改訂セッションの実ランナー計測
  (NFR-02 p95 7.2 ms @10k Task、NFR-03 0.12 s、NFR-13 0.06 s、NFR-14 約 7 s、NFR-16 の 8 並行
  往復) — どの上限も MVP の動作点で不足を示さなかった。再確定は新たな計測記録と本書の改訂による
  （計測なしでの変更は禁止）。
- **ATH-SCHED-004** (MUST) 上限により実行できない要求は Queue へ保持し、却下してはならない。
  ただし Queue 長上限を超える場合は `TransientError` を返す。
- **ATH-SCHED-005** (MUST) 同一優先度内は投入順（FIFO）で実行しなければならない。
- **ATH-SCHED-006** (MUST) 低優先度 Task の飢餓を防ぐため、実装は待機時間に応じた優先度昇格
  （既定: 30 分ごとに 1 段階、`normal` を上限）を行わなければならない。
- **ATH-SCHED-007** (MUST) Scheduler は Core Agent の判断を代替してはならない。Agent / Skill の
  選択を Scheduler 内に実装してはならない。
- **ATH-SCHED-008** (MUST) Core Agent Instance の資源枠を Task 用の上限とは別に常時確保しなければ
  ならない。予約枠は `max_concurrent_instances` の**外側**に確保し、下位 Agent の並行実行や
  終了処理中の旧 Instance によって Core Agent が起動できない状態を作ってはならない。
- **ATH-SCHED-009** (MUST) Queue の要素は **PlanStep の実行要求**としなければならない。
  `max_concurrent_tasks` は `RUNNING` 以降の Task 数、`max_concurrent_instances` は Agent Instance
  数、`max_instances_per_task` は 1 Task 配下の Instance 数を制限する。
- **ATH-SCHED-010** (MUST) `WAITING` および `SUSPENDED` の Instance を同時実行上限の計上対象から
  除外しなければならない。上位 Agent が下位 Agent の応答を待つ構造（ATH-HIER-015）で、
  親が枠を占有して子が起動できない hold-and-wait を発生させてはならない。
- **ATH-SCHED-011** (MUST) 階層構成の確定時（`compose_hierarchy`）に、その部分木が必要とする
  **同時実行 Instance 数の最大値**が `max_instances_per_task` を超える構成を `PolicyError` として
  拒否しなければならない（受付制御）。この計数は ATH-SCHED-010 と同一の基準とし、下位の応答を
  待つ上位 Agent（`WAITING`）を計上してはならない。すなわち計上対象は、同時に実作業を行う
  Instance（最も深い階層で並列実行される Worker 群など）に限る。上位階層を計上すると、
  ATH-HIER-013 が要求する 1 : N の階層が既定値で構成不能になる。
- **ATH-SCHED-012** (MUST) 1 Task の非終端 Instance すべてが `WAITING` / `SUSPENDED` であり、かつ
  当該 Task の Queue が空でない状態を検出した場合、`SchedulerBlocked` Event を発行しなければ
  ならない。タイムアウトによる失敗を待ってはならない。
- **ATH-SCHED-013** (MUST) 再試行のバックオフ時刻とタイムアウト期限を永続化し、Core の再起動時に
  再設定しなければならない（ATH-NFR-003）。
- **ATH-SCHED-014** (MUST) 依存が充足しているにもかかわらず資源上限で起動できない PlanStep を
  `SchedulerBlocked` Event として観測可能にしなければならない。

---

### 19. Result と Evaluation

#### 19.1 Result

```text
Agent → Result → ATHENA Core → Result Store（<ATHENA_HOME>/var/artifacts/）
```

```rust
pub struct Result {
  id: ResultId,
  task_id: TaskId,
  plan_step_id: Option<PlanStepId>,
  agent_id: AgentId,
  instance_id: InstanceId,
  status: "success" | "failure" | "partial",
  output: String,
  artifacts: Vec<Artifact>,
  logs_ref: Option<String>,
  skills_used: Vec<SkillRef>,
  model: Option<ModelRef>,
  started_at: String,
  finished_at: String,
  execution_time_ms: u64,
  metadata: serde_json::Value,
}
```

```json
{
  "task_id": "task-001",
  "status": "success",
  "output": "Timing issue was caused by an under-constrained SDC entry.",
  "artifacts": [
    { "kind": "file",  "base": "work_target", "path": "docs/analysis.md" },
    { "kind": "patch", "base": "store",       "path": "patch.diff" }
  ]
}
```

- **ATH-RESULT-001** (MUST) Result は status、output、artifacts、skills_used、実行時間、
  Agent / Instance 識別子を保持しなければならない。
- **ATH-RESULT-002** (MUST) Artifact は `base` と、その基準からの相対パスで記録しなければ
  ならない。`base` は以下の 2 種のみとする。

| `base` | 基準 | 用途 |
|---|---|---|
| `work_target` | `<PROJECT_ROOT>`（ATH-OPS-010） | プロジェクトの成果物（ソース、差分、ドキュメント） |
| `store` | `<ATHENA_HOME>/var/artifacts/<result_id>/`（Result Store） | ATHENA が保管する成果物（レポート、抽出データ） |

  Agent Workspace（Instance の cwd）相対のパスを Artifact として記録してはならない。Instance
  領域は保持期間の経過後に削除されるため（ATH-AIPRJ-029）、参照が失われる。
- **ATH-RESULT-004** (MUST) Instance 領域に生成された成果物のうち Result へ紐づけるものは、
  Result の受領時に Result Store へ複製しなければならない。これにより Instance 領域の削除
  （ATH-AIPRJ-029）が Result の保持（ATH-EVENT-014）を損なわない。複製後の `base` は `store`
  とする。
- **ATH-RESULT-003** (MUST) Result は不変としなければならない。訂正は新しい Result として記録する。

#### 19.2 Evaluation

Agent が返した結果をそのまま最終結果として扱わない。

```text
Coding Agent
     |
     v
  修正コード
     |
     v
Verification Agent
     |
     +-- PASS → 受理
     |
     +-- FAIL → Retry / Re-plan
```

検証 Agent の例：Test Agent、Lint Agent、Simulation Agent、Review Agent、Build Agent。

- **ATH-EVAL-001** (MUST) Core Agent は Result を完了条件（ATH-TASK-002）と照合しなければならない。
- **ATH-EVAL-002** (MUST) 検証可能な成果物（コード、設定、ドキュメント）を含む Result は、
  客観的手段による Verification を経なければ `SUCCESS` として受理してはならない。
- **ATH-EVAL-003** (MUST) Verification の実施者（Agent / Tool）、方法、結果を記録しなければ
  ならない。
- **ATH-EVAL-004** (MUST) 評価結果は受理 / 再試行 / 再計画 / 失敗 / 人間へのエスカレーションの
  いずれかでなければならない。
- **ATH-EVAL-005** (MUST NOT) Agent の自己申告 `status: success` のみを受理根拠としてはならない。

---

### 20. Failure / Retry / Timeout / Recovery

#### 20.1 エラー分類

| Class | 意味 | 再試行 | 再計画 | 承認 |
|---|---|---|---|---|
| `ValidationError` | 入力・状態遷移が不正 | 不可 | 不要 | 不要 |
| `PermissionError` | 権限不足 | 不可 | 不要 | エスカレーション |
| `PolicyError` | ポリシー違反（委任深さ、禁止操作） | 不可 | 不要 | エスカレーション |
| `TransientError` | 一時的資源不足・滞留 | 可（指数バックオフ） | 不要 | 不要 |
| `TimeoutError` | 期限超過 | 可（上限付き） | 上限到達時に実施 | 不要 |
| `AgentError` | Agent の作業失敗 | 可（代替 Agent） | 実施 | 不要 |
| `SkillError` | Skill の解決・実行失敗 | 条件付き（代替 Skill） | 実施 | 不要 |
| `RuntimeError` | 実行基盤の異常 | 可（Instance 再作成） | 上限到達時に実施 | 不要 |
| `InternalError` | ATHENA 自身の不整合 | 不可 | 不要 | 不要 |

- **ATH-FAIL-001** (MUST) すべてのエラーは上記 9 分類のいずれかへ分類しなければならない。
- **ATH-FAIL-002** (MUST) 再試行の可否は分類に基づいて決定しなければならない。分類不能な
  エラーは `InternalError` として扱い、再試行してはならない。
- **ATH-FAIL-003** (MUST) エラーは分類、コード、メッセージ、文脈（Task / Agent / Instance /
  Skill）とともに記録しなければならない。

#### 20.2 Retry

```text
Agent A → FAILED → Retry → Agent A → FAILED → Agent B
```

```yaml
retry:
  max_attempts: 3
  strategy: alternate-agent     # same-agent | alternate-agent | replan
  backoff:
    initial_ms: 1000
    multiplier: 2
    max_ms: 60000
```

- **ATH-FAIL-004** (MUST) Retry Policy は Task 単位または PlanStep 単位で設定可能としなければ
  ならない。
- **ATH-FAIL-005** (MUST) 試行回数上限を強制しなければならない。既定値は 3 とする。
- **ATH-FAIL-006** (MUST) `TransientError` に対する再試行は指数バックオフを適用しなければならない。
- **ATH-FAIL-007** (MUST) 各試行を独立した Agent Instance として記録しなければならない。
  試行履歴を上書きしてはならない。

#### 20.3 Timeout

```yaml
timeout:
  startup: 30s
  execution: 30m
  message_reply: 45m        # startup + execution より長いこと（ATH-CFG-008）
  approval: 1h
  stop: 60s
  clarification: 24h
```

本節は第 28.1 節の `timeout` セクションの再掲であり、重複定義ではない。規範的な既定値は
第 28.1 節を正とする。`startup` と `execution` は `runtime.<type>.startup_timeout` /
`runtime.<type>.execution_timeout` によって Runtime 単位で上書きできる（ATH-CFG-008）。
承認待ちおよび人間への確認の待ち時間は `execution` / `message_reply` の計時から除外する。

```text
RUNNING → TIMEOUT 検出 → Instance 停止 → RETRY または FAILED
```

- **ATH-FAIL-008** (MUST) 起動、実行、Message 応答、承認の各タイムアウトを実装しなければならない。
- **ATH-FAIL-009** (MUST) タイムアウト検出時、対象 Agent Instance を停止しなければならない。
  放置してはならない。
- **ATH-FAIL-010** (MUST) タイムアウト値は設定で上書き可能としなければならない（第 28 章）。
- **ATH-FAIL-014** (MUST) 承認待ち（`SUSPENDED`）および人間への確認（`WAITING_INPUT`）の経過時間を
  `execution` および `message_reply` の計時に含めてはならない。人間の応答遅延で作業を失っては
  ならない。
- **ATH-FAIL-015** (MUST) 停止タイムアウト（`timeout.stop`）を実装し、超過時は強制終了しなければ
  ならない（ATH-STATE-016）。

#### 20.4 Failure Recovery

Failure は単純なエラーではなく、次の意思決定の入力である。

```text
Agent
 |
FAILED
 |
ATHENA Core Agent
 |
原因分析
 |
+-- Retry（同一 Agent）
+-- 代替 Agent
+-- 代替 Skill
+-- Task 再分解
+-- Plan 変更
+-- Human 確認
```

- **ATH-FAIL-011** (MUST) Core Agent は失敗の原因分析結果に基づいて回復手段を選択しなければ
  ならない。無条件の同一再実行のみを回復手段としてはならない。
- **ATH-FAIL-012** (MUST) 回復手段の選択とその理由を記録しなければならない。
- **ATH-FAIL-013** (MUST) すべての回復手段が尽きた場合、Task を `FAILED` とし、失敗理由と
  実施した試行の一覧を利用者へ返さなければならない。

---

### 21. 実行フロー

本章は代表シナリオのエンドツーエンド動作を示す。

#### 21.1 シナリオ

```text
「FPGAのRTLを解析してタイミング問題の修正案を作成する」
```

#### 21.2 フロー

```text
 1. CLI (athena chat) / Studio が自然言語指示を受理
        |
        v
 2. Core API が認証・検証し Task を作成          → TaskCreated
        |
        v
 3. Core Agent（agent-cli #0）: Understand
      goal / constraints / completion_criteria / risk_level を抽出
      ※ Core Agent の判断要求は Core Host の検査を経る（第 7.5.2 節）
        |
        v
 4. Core Agent: Plan
      PlanStep: RTL解析 / Timing解析 / 原因特定 / 修正案 / 検証   → PlanCreated
        |
        v
 5. Core Agent: Decide
      Agent 階層構成（FPGA Director → RTL Lead / Timing Lead）  → HierarchyComposed
      Agent 選択（capability matching）                          → AgentSelected
      Skill 選択（Skill Catalog 検索）                           → SkillSelected
        |
        v
 6. Scheduler が依存充足済み PlanStep を投入      → TaskQueued / TaskStarted
        |
        v
 7. Runtime Manager → agent-cli Adapter
      Workspace の aiprj 初期化確認
      Skill Resolver / Loader が Instance の skills/ へ配置       → SkillLoaded
      Agent Process 起動                                          → AgentStarted
        |
        v
 8. Message Broker が instruction を送信          → AgentMessageSent
        |
        v
 9. Agent が作業を実行（Tool 使用、下位 Agent へ委任）
      aiprj 規約に従い AI_PRJ_TASKS.md を更新、AI_LOG を保存
        |
        v
10. Agent が report を返す                        → AgentMessageReceived / AgentCompleted
        |
        v
11. Core Agent: Observe → Evaluate               → TaskResultReceived
      Verification Agent による検証
        |
    +---+------------------+
    |                      |
  不充足                 充足
    |                      |
    v                      v
12. Re-plan（SDC 原因へ   13. 最終結果を統合      → TaskCompleted
    計画変更）                   |
    → TaskReplanning             v
    |                      14. CLI / Studio へ返却
    +--> 5 へ戻る
```

高リスク操作（修正の適用、`git push`）が必要な場合、11 と 13 の間で承認を取得する
（`ApprovalRequested` → `ApprovalGranted`）。

- **ATH-CORE-017** (MUST) 上記フローの各段階は、対応する Event の発行によって外部から観測可能
  でなければならない（第 23 章）。

---

## Part IV インターフェース

### 22. Core API

#### 22.1 共通仕様

| 項目 | 規定 |
|---|---|
| プロトコル | HTTP/1.1 以上、JSON（`application/json; charset=utf-8`） |
| ベースパス | `/api/v1` |
| バージョニング | パスによる（`/api/v1`）。互換性を壊す変更は `/api/v2` を新設する |
| 認証 | `Authorization: Bearer <token>` |
| 時刻表現 | RFC 3339 / UTC |
| 識別子 | 文字列（接頭辞 + ULID） |
| リアルタイム | SSE（既定）、WebSocket（任意） |

- **ATH-API-002** (MUST) すべてのエンドポイントは認証を要求しなければならない。認証なしで
  状態を変更できるエンドポイントを設けてはならない。
- **ATH-API-003** (MUST) 互換性を壊す変更はバージョンを上げなければならない。既存バージョンの
  応答形式を変更してはならない。
- **ATH-API-004** (MUST) 状態変更系（POST / DELETE）は `Idempotency-Key` ヘッダを受理し、同一
  キーの再送に対して同一結果を返さなければならない。
- **ATH-API-005** (MUST) 一覧系はカーソル方式のページネーションを提供しなければならない
  （`?limit=&cursor=`、応答に `next_cursor`）。
- **ATH-API-006** (MUST) Core 内部モジュールは HTTP API を経由せず、内部インターフェースを
  直接利用しなければならない。
- **ATH-API-019** (MUST) すべてのエンドポイントに必要ロール（第 29.2 節）を定義しなければならない。
  既定は `admin` とし、明示的に緩める場合のみ `operator` / `viewer` とする。
- **ATH-API-020** (MUST) `GET /api/v1/metrics` は Prometheus テキスト形式
  （`text/plain; version=0.0.4`）で応答しなければならない。必要ロールは `viewer` とする。

#### 22.2 エラー応答

```json
{
  "error": {
    "class": "PermissionError",
    "code": "SKILL_PERMISSION_EXCEEDS_AGENT",
    "message": "Skill requires 'shell' which is not granted to the agent.",
    "details": {
      "skill_id": "production-deploy",
      "agent_id": "worker-1"
    },
    "correlation_id": "cor-01JA2B3C4D5E6F7G8H9J0K1L2M"
  }
}
```

| HTTP | 用途 | 対応 class |
|---|---|---|
| 400 | 要求が不正 | `ValidationError` |
| 401 | 未認証 | — |
| 403 | 権限不足・ポリシー違反 | `PermissionError` / `PolicyError` |
| 404 | 資源が存在しない | `ValidationError` |
| 409 | 状態が遷移を許可しない | `ValidationError` |
| 429 | 資源上限 | `TransientError` |
| 500 | 内部不整合 | `InternalError` |
| 503 | 一時的に実行不能 | `TransientError` / `RuntimeError` |
| 504 | 上流タイムアウト | `TimeoutError` |

- **ATH-API-007** (MUST) すべてのエラー応答は上記 envelope 形式に従わなければならない。
- **ATH-API-008** (MUST) `class` は第 20.1 節の分類と一致しなければならない。
- **ATH-API-009** (MUST) `correlation_id` を返し、ログ・Event と対応付け可能にしなければならない。

#### 22.3 Task API

```http
GET    /api/v1/tasks
POST   /api/v1/tasks
GET    /api/v1/tasks/{id}
POST   /api/v1/tasks/{id}/run
POST   /api/v1/tasks/{id}/cancel
GET    /api/v1/tasks/{id}/results
GET    /api/v1/tasks/{id}/events
GET    /api/v1/tasks/{id}/plan
```

`POST /api/v1/tasks` 要求：

```json
{
  "title": "FPGA timing fix",
  "instruction": "このFPGA RTLのタイミング問題を解析して修正案を作成してください。",
  "workspace": "/work/fpga-project",
  "priority": "high",
  "constraints": ["本番ビットストリームを更新しない"],
  "auto_run": true
}
```

応答（201）：

```json
{
  "id": "task-01JA2B3C4D5E6F7G8H9J0K1L2M",
  "state": "QUEUED",
  "created_at": "2026-10-12T10:00:00Z"
}
```

| エンドポイント | 必須パラメータ | 主な応答 |
|---|---|---|
| `GET /tasks` | — | `{ items: Task[], next_cursor }`。`?state=&priority=` で絞り込み |
| `POST /tasks` | `instruction`, `workspace` | 201 + Task |
| `GET /tasks/{id}` | — | Task（Plan 概要と状態を含む） |
| `POST /tasks/{id}/run` | — | 202。`CREATED` 以外は 409 |
| `POST /tasks/{id}/cancel` | — | 202。終端状態は 409 |
| `GET /tasks/{id}/results` | — | `Result[]` |
| `GET /tasks/{id}/plan` | — | 最新 Plan（`?revision=` で指定可） |
| `GET /tasks/{id}/messages` | — | `Utterance[]`（人間と Core Agent の対話） |
| `POST /tasks/{id}/messages` | `body` | 人間の発話を投入。202 |
| `POST /tasks/{id}/answer` | `clarification_id`, `answer` | 確認への回答（ATH-HITL-020）。202 |

- **ATH-API-010** (MUST) `instruction` と `workspace` を必須としなければならない。
- **ATH-API-011** (MUST) 状態が遷移を許可しない操作は 409 を返さなければならない。
- **ATH-API-021** (MUST) `POST /api/v1/conversations` と `POST /api/v1/conversations/{id}/messages`
  を提供し、Task 作成前の自然言語対話（`athena chat`）を成立させなければならない。Task が
  作成された時点で `Conversation.task_id` を関連付ける。
- **ATH-API-022** (MUST) `auto_run: true` の場合、Task は作成時に `QUEUED` へ遷移させなければ
  ならない。この場合 `POST /tasks/{id}/run` は 409 を返す。
- **ATH-API-023** (MUST) 同一 `workspace` を対象とする Task が非終端状態で存在する場合、
  作成要求に 409 を返さなければならない（ATH-DATA-009）。

#### 22.4 Plan API

```http
GET    /api/v1/plans/{id}
GET    /api/v1/plans/{id}/revisions
GET    /api/v1/plans/{id}/steps
```

- **ATH-API-012** (MUST) Plan は読み取り専用として公開しなければならない。外部から Plan を
  直接書き換える API を設けてはならない。Plan の内容を決める判断は Core Agent の責務であり、
  その永続化は Core Host が行う（ATH-CORE-027、第 7.5.5 節の `submit_plan`）。

#### 22.5 Agent API

```http
GET    /api/v1/agents                      viewer
POST   /api/v1/agents                      admin   Agent 定義の登録
GET    /api/v1/agents/{id}                 viewer
PUT    /api/v1/agents/{id}                 admin
GET    /api/v1/instances                   viewer
GET    /api/v1/instances/{id}              viewer
GET    /api/v1/instances/{id}/messages     viewer
POST   /api/v1/instances/{id}/stop         admin   緊急停止
GET    /api/v1/hierarchies/{task_id}       viewer
GET    /api/v1/tool-executions             viewer
GET    /api/v1/evaluations                 viewer
GET    /api/v1/memory                      operator
DELETE /api/v1/memory/{id}                 operator
```

- **ATH-API-013** (MUST) `agents` は Agent 定義、`instances` は Agent Instance を返し、両者を
  混在させてはならない。
- **ATH-API-024** (MUST NOT) Agent Instance を直接起動する API を提供してはならない。Instance の
  生成は Core Agent の判断と Core Host の検査を経た委任のみによる（ATH-HIER-011、ATH-CORE-026）。
  `POST /api/v1/instances/{id}/stop` は運用上の緊急停止手段として `admin` に限り提供する。
- **ATH-API-025** (MUST) 動的生成された Agent 定義も `POST /api/v1/agents` と同一の検証を経て
  永続化しなければならない（ATH-HIER-008、ATH-AGENT-001）。

#### 22.6 Runtime API

```http
GET    /api/v1/runtimes
GET    /api/v1/runtimes/{id}
GET    /api/v1/runtimes/{id}/capabilities
```

#### 22.7 Skill API

```http
GET    /api/v1/skills                                    viewer
GET    /api/v1/skills/{id}                               viewer   版一覧を含む
GET    /api/v1/skills/{id}/versions/{version}            viewer
POST   /api/v1/skills                                    admin    登録（source + checksum）
POST   /api/v1/skills/{id}/versions/{version}/enable     admin
POST   /api/v1/skills/{id}/versions/{version}/disable    admin
POST   /api/v1/skills/{id}/versions/{version}/update     admin
POST   /api/v1/skills/{id}/versions/{version}/validate   admin
DELETE /api/v1/skills/{id}/versions/{version}            admin
GET    /api/v1/skills/search?capability=fpga             viewer
```

- **ATH-API-026** (MUST) Skill 操作は `(id, version)` を指定しなければならない。版を省略した
  操作を提供してはならない（ATH-STATE-026）。

`POST /api/v1/skills` 要求：

```json
{
  "source": "https://example.invalid/skills/fpga-timing-analysis-1.2.0.tar.gz",
  "checksum": "sha256:...",
  "enable": false
}
```

- **ATH-API-014** (MUST) Skill 登録は検証完了まで `ENABLED` へ遷移させてはならない
  （ATH-SKILL-009）。
- **ATH-API-015** (MUST) Skill 操作系は管理権限（第 29 章の `admin` ロール）を要求しなければ
  ならない。

#### 22.8 Approval API

```http
GET    /api/v1/approvals
GET    /api/v1/approvals/{id}
POST   /api/v1/approvals/{id}/approve
POST   /api/v1/approvals/{id}/reject
```

- **ATH-API-016** (MUST) 承認・拒否の要求には承認者の識別情報を記録しなければならない。
- **ATH-API-017** (MUST) 終端状態の Approval への操作は 409 を返さなければならない。

#### 22.9 Event / System API

```http
GET    /api/v1/events                 一覧（?task_id=&type=&since=&limit=&cursor=）
GET    /api/v1/events/stream          SSE による購読（?task_id=&types=）
GET    /api/v1/status                 システム状態
GET    /api/v1/healthz                生存確認
GET    /api/v1/metrics                メトリクス（第 30 章）
```

`GET /api/v1/status` 応答：

```json
{
  "version": "0.2.0",
  "uptime_s": 86400,
  "tasks": { "running": 2, "queued": 1, "waiting_approval": 0 },
  "instances": { "running": 5, "max": 16 },
  "runtimes": [ { "type": "agent-cli", "healthy": true, "instances": 5 } ],
  "skills": { "enabled": 7, "failed": 0 }
}
```

- **ATH-API-018** (MUST) `GET /api/v1/healthz` は認証なしで応答してよい唯一のエンドポイントと
  し、内部状態を含めてはならない。

---

### 23. Event Architecture

ATHENA は状態変化を Event として扱う。Event は不変の事実であり、Studio のリアルタイム表示、
CLI の状態表示、Logging、Audit、Debugging、Replay の基盤となる。

#### 23.1 Event Envelope

```rust
pub struct Event {
  event_id: String,            // "evt-" + ULID
  schema_version: u64,      // 本書では 1
  type: EventType,
  occurred_at: String,         // RFC 3339
  sequence: u64,            // Core 単位の単調増加
  task_id: Option<TaskId>,
  plan_step_id: Option<PlanStepId>,
  agent_id: Option<AgentId>,
  instance_id: Option<InstanceId>,
  skill_id: Option<String>,
  correlation_id: String,      // 一連の処理の識別子
  causation_id: Option<String>, // 直接の原因 Event の event_id
  actor: "human" | "core-agent" | "agent" | "scheduler" | "runtime" | "system",
  payload: serde_json::Value,
}
```

```json
{
  "event_id": "evt-01JA2B3C4D5E6F7G8H9J0K1L2M",
  "schema_version": 1,
  "type": "AgentCompleted",
  "occurred_at": "2026-10-12T10:00:00Z",
  "sequence": 1041,
  "task_id": "task-001",
  "agent_id": "rtl-agent",
  "instance_id": "ai-01JA2B3C4D5E6F7G8H9J0K1L2N",
  "correlation_id": "cor-01JA2B3C4D5E6F7G8H9J0K1L2M",
  "causation_id": "evt-01JA2B3C4D5E6F7G8H9J0K1L2K",
  "actor": "agent",
  "payload": { "result_id": "res-0001", "status": "success" }
}
```

- **ATH-EVENT-001** (MUST) すべての Event は上記 envelope を満たさなければならない。
- **ATH-EVENT-002** (MUST) Event は不変としなければならない。訂正は新しい Event で表現する。
- **ATH-EVENT-003** (MUST) `sequence` は Core 単位で単調増加しなければならない。
- **ATH-EVENT-004** (MUST) 同一 `task_id` の Event 順序は保証しなければならない。
- **ATH-EVENT-005** (MUST) 配信は at-least-once としなければならない。購読側は `event_id` で
  重複排除する。
- **ATH-EVENT-006** (MUST) `schema_version` を持ち、payload 構造の変更時に加算しなければならない。
- **ATH-EVENT-007** (MUST NOT) payload に秘密情報、認証情報、Agent への入力全文を含めてはならない。
  大きな内容は参照（Result ID、ログ参照）で表現する。

#### 23.2 Event 型カタログ

| 分類 | Event 型 |
|---|---|
| Task | `TaskCreated` `TaskQueued` `TaskStarted` `TaskResultReceived` `TaskReplanning` `TaskRetrying` `TaskCancelling` `TaskCancelled` `TaskCompleted` `TaskFailed` |
| Plan | `PlanCreated` `PlanRevised` `PlanStepStarted` `PlanStepCompleted` `PlanStepFailed` |
| Hierarchy | `HierarchyComposed` `AgentDelegated` |
| Agent | `AgentSelected` `AgentStarting` `AgentStarted` `AgentWaiting` `AgentStopping` `AgentStopped` `AgentCompleted` `AgentFailed` |
| Core Agent | `CoreAgentStarted` `CoreAgentFailed` `CoreAgentRestarted` `CoreAgentEscalated` `CoreAgentHeartbeat` `PrivilegedRequestDenied` |
| PlanStep | `PlanStepReady` `PlanStepOrphaned` `PlanStepSkipped` |
| Instance | `AgentSuspended` `AgentResumed` |
| Scheduler | `SchedulerBlocked` `TaskStalled` |
| Clarification | `ClarificationRequested` `ClarificationAnswered` `ClarificationExpired` |
| Workspace | `WorkspaceInitialized` `WorkspaceLeased` `WorkspaceReleased` |
| Message | `AgentMessageSent` `AgentMessageReceived` `AgentMessageTimeout` |
| Tool | `ToolExecutionStarted` `ToolExecutionCompleted` `ToolExecutionFailed` |
| Skill | `SkillRegistered` `SkillValidating` `SkillValidated` `SkillValidationFailed` `SkillEnabled` `SkillDisabled` `SkillUpdating` `SkillUpdated` `SkillRemoved` `SkillSelected` `SkillLoaded` `SkillExecutionStarted` `SkillExecutionCompleted` `SkillExecutionFailed` |
| Runtime | `RuntimeInstanceCreated` `RuntimeInstanceStopped` `RuntimeUnhealthy` |
| Approval | `ApprovalRequested` `ApprovalGranted` `ApprovalRejected` `ApprovalExpired` `ApprovalConsumed` |
| Memory | `MemoryRecordCreated` `MemoryRecordDeleted` |
| Evaluation | `EvaluationStarted` `EvaluationPassed` `EvaluationFailed` |
| System | `SystemStarted` `SystemStopping` `ConfigurationLoaded` |

- **ATH-EVENT-008** (MUST) 第 17 章の遷移表が指定する Event をすべて実装しなければならない。
- **ATH-EVENT-009** (MUST) 本カタログにない Event 型を追加する場合、本書を改訂しなければならない。
- **ATH-EVENT-017** (MUST) Event ごとの必須 payload 項目を定義しなければならない。最低限、
  以下を満たす。

| Event | 必須 payload 項目 |
|---|---|
| `TaskCreated` | `instruction_digest`, `workspace`, `priority`, `created_by` |
| `TaskQueued` / `TaskStarted` | `plan_id`, `revision` |
| `TaskResultReceived` | `result_id`, `plan_step_id`, `status` |
| `TaskReplanning` | `from_revision`, `to_revision`, `reason` |
| `TaskRetrying` | `attempt`, `max_attempts`, `error_class` |
| `TaskCompleted` | `result_ids`, `completion_criteria_met` |
| `TaskFailed` | `error_class`, `error_code`, `attempts`, `recovery_attempted` |
| `TaskCancelled` | `forced` (boolean), `side_effects` |
| `PlanCreated` / `PlanRevised` | `plan_id`, `revision`, `step_count`, `rationale` |
| `PlanStepReady` / `PlanStepStarted` | `plan_step_id`, `attempt`, `assigned_agent` |
| `PlanStepOrphaned` | `plan_step_id`, `instance_id`, `side_effects` |
| `HierarchyComposed` | `hierarchy_id`, `revision`, `nodes`（level と scope_ref） |
| `AgentSelected` | `agent_id`, `candidates`, `rationale`, `capability_match` |
| `SkillSelected` | `skill_refs`, `rationale` |
| `AgentStarted` / `AgentStopped` | `instance_id`, `agent_id`, `runtime_type` |
| `AgentWaiting` | `waits_on`（instance_id / approval_id / message_id の配列）, `deadline` |
| `AgentFailed` | `error_class`, `attempt`, `side_effects` |
| `AgentMessageSent` / `AgentMessageReceived` | `message_id`, `type`, `thread_id` |
| `AgentMessageTimeout` | `message_id`, `thread_id`, `deadline` |
| `ToolExecutionStarted` / `Completed` / `Failed` | `tool_execution_id`, `tool`, `operation_descriptor`, `risk_level` |
| `ApprovalRequested` | `approval_id`, `operation_descriptor`, `risk_level`, `expires_at` |
| `ApprovalGranted` / `ApprovalRejected` | `approval_id`, `responded_by`, `scope` |
| `ApprovalConsumed` | `approval_id`, `operation_descriptor`, `uses_remaining` |
| `PrivilegedRequestDenied` | `request_kind`, `error_class`, `policy_id` |
| `CoreAgentHeartbeat` | `iteration`, `focus_task_id`, `pending_requests` |
| `CoreAgentRestarted` | `attempt`, `window_start`, `context_document_ref` |
| `SchedulerBlocked` | `plan_step_id`, `blocking_limit` |
| `TaskStalled` | `task_id`, `last_event_at`, `stall_seconds` |
| `ClarificationRequested` / `Answered` | `clarification_id`, `question_digest` |
| `EvaluationPassed` / `EvaluationFailed` | `evaluation_id`, `evaluator`, `method`, `verdict` |
| `Skill*` | `skill_id`, `version`, `checksum` |
| `WorkspaceInitialized` | `workspace`, `aiprj_version` |

- **ATH-EVENT-018** (MUST) `AgentWaiting` の `waits_on` を必須としなければならない。これにより
  待機グラフを Event から再構成し、循環と停滞を検出できる（第 30 章）。

#### 23.3 保持と Replay

- **ATH-EVENT-010** (MUST) Event は永続化しなければならない（第 27 章）。
- **ATH-EVENT-011** (MUST) 既定の保持期間は 90 日とし、設定で変更可能としなければならない。
- **ATH-EVENT-012** (MUST) Task 単位で Event を時系列順に再生でき、Task の実行経過を再構成
  できなければならない。
- **ATH-EVENT-013** (MUST NOT) Event の再生によって Agent の再実行や外部副作用を発生させては
  ならない。Replay は観測目的に限る。
- **ATH-EVENT-014** (MUST) 保持期間を超えて削除する場合、Task の最終結果と Result は保持しなければ
  ならない。

#### 23.4 aiprj AI_LOG との関係

```text
Agent
 |
 +-- aiprj AI_LOG     Agent 自身の作業記録（Agent の文脈）
 |
 +-- ATHENA Event     状態遷移・通信・監視・監査（システムの事実）
```

- **ATH-EVENT-015** (MUST) AI_LOG と Event を別レイヤーとして管理しなければならない。一方を
  他方の代替としてはならない。
- **ATH-EVENT-016** (MUST) Agent Instance の記録は、対応する AI_LOG ファイルへの参照を保持
  しなければならない（第 30.4 節）。

#### 23.5 外部配信

- **ATH-EVENT-019** (MUST) v1 の Event の外部配信手段は **SSE のみ**とする
  （`GET /api/v1/events/stream`、第 22.9 節）。WebSocket およびメッセージブローカー連携を
  v1 の要件としてはならない。Studio（第 25 章）が双方向通信を要求した時点で WebSocket を
  追加するが、SSE による購読を廃止してはならない。配信手段を追加する場合も、Event Envelope
  （第 23.1 節）、順序保証（ATH-EVENT-004）、配送保証（ATH-EVENT-005）を変更してはならない。
- **ATH-EVENT-020** (MUST) 配信は Event の永続化（ATH-EVENT-010）より後に行わなければならない。
  購読者が存在しない、または配信に失敗した Event を破棄してはならない。購読者は `sequence` を
  用いて再接続時に欠落分を取得できなければならない。

---

### 24. ATHENA CLI

#### 24.1 位置付け

ATHENA CLI は ATHENA システム全体を操作するコマンドライン UI であり、`agent-cli` のコマンド
ラッパーではない。

```text
athena CLI
    |
ATHENA Core API
    |
ATHENA Core
    |
Runtime
    |
agent-cli
```

- **ATH-CLI-001** (MUST) CLI は Core API 経由でのみ Core を操作しなければならない。
- **ATH-CLI-002** (MUST NOT) CLI は `agent-cli` を直接起動してはならない。

#### 24.2 自然言語対話

CLI は自然言語対話を第一級機能とする。CLI は Task CRUD のためだけの道具ではなく、
**Human → ATHENA Core Agent の直接インターフェース**である。

```bash
athena chat
```

```text
$ athena chat

You:
このプロジェクトのビルドが失敗する原因を調査して、
必要なら修正してください。

ATHENA:
プロジェクトを確認します。
まずビルドエラーと最近の変更を調査します。
```

- **ATH-CLI-003** (MUST) `athena chat` を提供しなければならない。
- **ATH-CLI-004** (MUST) 対話中に発生した承認要求を対話内で提示し、応答を受理できなければ
  ならない。
- **ATH-CLI-005** (MUST) 対話中に Task の進行状況（現在の PlanStep、実行中 Agent）を表示しなければ
  ならない。

#### 24.3 コマンド一覧

| コマンド | 用途 |
|---|---|
| `athena init` | カレントプロジェクトへ `.athena/` を生成（第 4.3.4 節、ATH-CLI-009） |
| `athena chat` | 自然言語対話 |
| `athena run <instruction>` | 単発指示の実行 |
| `athena status` | システム状態表示 |
| `athena task list` | Task 一覧 |
| `athena task create` | Task 作成 |
| `athena task show <task-id>` | Task 詳細（Plan、Result を含む） |
| `athena task cancel <task-id>` | Task 取消 |
| `athena plan show <task-id>` | Plan とその revision 表示 |
| `athena agent list` | Agent 定義一覧 |
| `athena agent show <agent>` | Agent 定義詳細 |
| `athena instance list` | Agent Instance 一覧 |
| `athena instance stop <id>` | Instance 停止 |
| `athena runtime list` | Runtime 一覧 |
| `athena event tail` | Event の追従表示 |
| `athena approval list` | 承認待ち一覧 |
| `athena approval approve <id>` | 承認 |
| `athena approval reject <id>` | 拒否 |
| `athena skill list` | Skill 一覧 |
| `athena skill search <capability>` | Skill 検索 |
| `athena skill show <skill>` | Skill 詳細 |
| `athena skill install <source>` | Skill 登録 |
| `athena skill enable <skill>` | Skill 有効化 |
| `athena skill disable <skill>` | Skill 無効化 |
| `athena skill update <skill>` | Skill 更新 |
| `athena skill remove <skill>` | Skill 削除 |
| `athena skill validate <skill>` | Skill 検証 |

- **ATH-CLI-006** (MUST) 上表のコマンドを提供しなければならない。
- **ATH-CLI-007** (MUST) 非対話実行のために `--json` 出力を提供しなければならない。
- **ATH-CLI-008** (MUST) 終了コードは成功 0、要求不正 2、権限エラー 3、実行失敗 4、内部エラー 70
  としなければならない。
- **ATH-CLI-009** (MUST) `athena init` は、カレントディレクトリをプロジェクトルートとして
  `.athena/` を生成しなければならない。生成物は `config.yaml`（既定値）、`agents/`、
  `personas/`、`skills/`、`var/` とする。既存の `.athena/` がある場合は上書きせず、不足する
  要素のみを補わなければならない（ATH-AIPRJ-006 と同じ方針）。あわせて Instance の
  `.agent-cli/` を生成する原本 `<ATHENA_HOME>/var/agent-cli-template/`（`config.toml` と
  `commands/`）を生成する（ATH-ACLI-013）。プロジェクト
  ルートの `aiprj` が未初期化の場合は、その初期化も併せて実施する（ATH-AIPRJ-005）。この初期化
  により `<PROJECT_ROOT>/.aiprj/` と `<PROJECT_ROOT>/.agent-cli/` が作られる。後者は利用者が
  直接 `agent-cli` を使う場合の設定であり、ATHENA は参照しない（ATH-ACLI-012）。
- **ATH-CLI-010** (MUST) `athena init` 以外のコマンドは、`.athena/` が解決できない場合
  （ATH-CFG-010）に終了コード 2 で失敗し、`athena init` を促さなければならない。

---

### 25. ATHENA Studio

#### 25.1 位置付け

```text
Browser
   |
ATHENA Studio
   |
Core API
   |
ATHENA Core
```

- **ATH-STUDIO-001** (MUST NOT) Studio は Core の内部実装を直接参照してはならない。
- **ATH-STUDIO-002** (MUST) Studio は Core API と Event Stream のみを情報源としなければならない。

#### 25.2 画面

| 画面 | 表示内容 |
|---|---|
| Dashboard | Running / Completed / Failed Tasks、Active Agents、System Status、承認待ち件数 |
| Task View | 状態、指示、目的、Plan、担当 Agent、Message、Logs、Results |
| Plan View | PlanStep の依存関係（DAG 表示）と revision 履歴 |
| Agent Hierarchy View | Task ごとの Producer / Director / Lead / Worker 構成 |
| Agent View | 状態、Runtime、Model、Capabilities、Skill Set、現在の Action、Logs |
| Message View | Agent間通信の時系列 |
| Tool Execution View | Tool 実行履歴と結果 |
| Skill View | Skill 一覧、状態、版、リスク、有効化操作 |
| Approval View | 承認要求の提示と承認・拒否操作 |
| Event View | Event の時系列とフィルタ |
| Result View | Result と Artifact |

特に以下を可視化する。

```text
現在の目的
現在実行中のAgent
現在のAction
実行結果
次のAction
```

- **ATH-STUDIO-003** (MUST) 上表の画面を提供しなければならない。
- **ATH-STUDIO-004** (MUST) Task の進行を Event Stream によってリアルタイム更新しなければならない。
- **ATH-STUDIO-005** (MUST NOT) Agent の内部推論過程（Chain-of-Thought）を無条件に表示しては
  ならない。利用者が理解できる実行状況と要約を表示する。
- **ATH-STUDIO-006** (MUST) 承認操作は Studio から実行可能でなければならない。
- **ATH-STUDIO-007** (MUST NOT) 秘密情報を画面へ表示してはならない（第 29 章）。

---

## Part V 基盤

### 26. データモデル

本章は ATHENA の中核エンティティを定義する。表現は Rust の型宣言とする（ATH-POLICY-002）。

型の表現には以下の写像を本章全体に適用する。

| 意味 | 本章の表現 |
|---|---|
| 省略可能・該当しない値 | `Option<T>` |
| 値の列挙 | `enum` |
| 件数・サイズ・時間（ミリ秒）・revision | `u64` |
| プロセス ID | `u32` |
| プロセス終了コード | `i32` |
| 比率・係数 | `f64` |
| 任意構造の付随データ | `serde_json::Value` |
| RFC 3339 の時刻（ATH-DATA-002） | `String` |

時刻を `String` とするのは、本書が規定するのは表現形式（RFC 3339 / UTC）であり、実装側の
日時型の選択を拘束しないためである。

#### 26.1 識別子

```rust
pub type TaskId = String;  // "task-" + ULID
pub type PlanId = String;  // "plan-" + ULID
pub type PlanStepId = String;  // "step-" + ULID
pub type AgentId = String;  // persona 由来の slug（例 "timing-lead", "athena-core"）
pub type InstanceId = String;  // "ai-"   + ULID
pub type ResultId = String;  // "res-"  + ULID
pub type EventId = String;  // "evt-"  + ULID
pub type MessageId = String;  // "msg-"  + ULID
pub type SkillId = String;  // slug（例 "fpga-timing-analysis"）
pub type ApprovalId = String;  // "apr-"  + ULID
pub type MemoryId = String;  // "mem-"  + ULID
pub type ProjectId = String;  // Workspace の正規化絶対パスの SHA-256（先頭 16 桁）

pub enum RuntimeType { AgentCli, LocalProcess, Docker, Ssh, Kubernetes, Cloud }
pub enum RiskLevel { Low, Medium, High }
pub enum Priority { Low, Normal, High }

pub enum ToolName {
    FsRead, FsWrite, Shell, Git, Search,
    SendTo, Process, Terminal, Network,
}

// Permission は ToolName の上位集合（Tool 以外の権限も含む）
pub enum Permission {
    Tool { tool: ToolName },
    Fs { mode: FsMode, scope: String },
    Network { mode: NetworkMode, hosts: Option<Vec<String>> },
    Process,
}
pub enum FsMode { Read, Write }
pub enum NetworkMode { Allow, Deny, Allowlist }

pub enum MessageType {
    Instruction, Report, Question, Answer, Status, Error,
    Complete, ApprovalRequest,
    ControlRequest, ControlResponse, Cancel, Interrupt,
}

pub enum TaskState {
    Created, Queued, Running, WaitingApproval, WaitingInput, Evaluating, Replanning,
    Retrying, Cancelling, Success, Failed, Cancelled,
}

pub enum PlanStepState {
    Pending, Ready, Running, Suspended, Orphaned, Success, Failed, Skipped,
}

pub enum AgentInstanceState {
    Created, Starting, Idle, Busy, Waiting, Suspended, Stopping, Stopped, Failed,
}

pub enum SkillState {
    Discovered, Validating, Available, Enabled, Disabled, Updating, Failed, Removed,
}

pub enum ApprovalState { Requested, Granted, Rejected, Expired, Consumed }

pub type EventType = String;  // 第 23.2 節のカタログに定義された値のみ

pub enum AgentLevel { Producer, Director, Lead, Worker }
pub enum Isolation { Process, Container, RemoteHost, Pod }
pub enum ArtifactKind { File, Patch, Report, Log, Data }
pub enum ArtifactBase { WorkTarget, Store }
pub enum MemoryTier { Short, Working, Long }
pub enum SideEffects { Applied, None, Unknown }
pub enum ToolExecutionStatus { Started, Succeeded, Failed, Denied }
pub enum Verdict { Accept, Retry, Replan, Fail, Escalate }
pub enum UtteranceRole { Human, CoreAgent }
pub enum UtteranceKind { Message, Question, Answer, Summary }
pub enum Evaluator { Agent(AgentId), Tool }

pub struct Deadlines { startup_ms: u64, execution_ms: u64 }
pub struct MemoryOrigin { task_id: Option<TaskId>, event_id: Option<EventId> }
```

Runtime 関連の型は以下とする（第 14.2 節の `Runtime` インターフェースが用いる）。

```rust
pub struct RuntimeInstanceRef {
  runtime_type: RuntimeType,
  runtime_instance_id: String,
  instance_id: InstanceId,
}

pub struct StartContext {
  task_id: Option<TaskId>,          // Core Agent Instance は null
  plan_step_id: Option<PlanStepId>,
  instance_id: InstanceId,
  workspace: String,               // Instance ごとのディレクトリ。agent-cli の cwd となる
                                   // 書き込み領域と Persona・Skill を含む（ATH-OPS-009）
  work_targets: Vec<String>,          // 作業対象の絶対パス（ATH-OPS-010）
  persona_ref: String,             // 読み取り専用で配置（ATH-CORE-043）
  persona_version: String,
  skill_set: Vec<SkillRef>,           // 解決済み（ATH-SKILL-016）
  allowed_tools: Vec<ToolName>,
  delegable_tools: Vec<ToolName>,
  filesystem_scope: Vec<String>,      // workspace と work_targets の和集合（ATH-SEC-010）
                                   // 制御面と他 Instance を含めない（ATH-OPS-008）
  model: ModelRef,
  deadlines: Deadlines,
  resume_snapshot_ref: Option<String>,    // SUSPENDED からの再開（ATH-STATE-021）
  context_document_ref: Option<String>,   // Core Agent 再起動（ATH-CORE-046）
}

pub enum StopReason { Completed, Cancelled, Timeout, Failed, Shutdown, Suspend }

pub struct RuntimeInstanceStatus {
  state: AgentInstanceState,
  pid: Option<u32>,
  last_heartbeat_at: Option<String>,
  exit_code: Option<i32>,
}

pub struct RuntimeCapabilities {
  isolation: Isolation,
  supported_tools: Vec<ToolName>,
  max_instances: u64,
  supports_suspend: bool,
}
```

- **ATH-DATA-001** (MUST) 識別子は接頭辞付きの文字列としなければならない。数値連番を外部公開
  識別子として使用してはならない。
- **ATH-DATA-002** (MUST) 時刻は RFC 3339 の UTC 文字列で保持しなければならない。

#### 26.2 Task / Plan

```rust
pub struct Task {
  id: TaskId,
  parent_id: Option<TaskId>,
  title: String,
  instruction: String,             // 人間の自然言語入力（原文）
  workspace: String,               // Agent Workspace の絶対パス
  goal: Option<String>,             // Core Agent の解釈
  constraints: Vec<String>,
  completion_criteria: Vec<String>,
  state: TaskState,
  priority: Priority,
  plan_id: Option<PlanId>,
  risk_level: RiskLevel,
  attempt_count: u64,
  max_attempts: u64,
  iteration_count: u64,         // Agent Loop 反復回数（ATH-CORE-013）
  max_iterations: u64,
  deadline: Option<String>,
  created_by: String,              // 利用者識別子
  created_at: String,
  updated_at: String,
  finished_at: Option<String>,
}
```

`Plan` と `PlanStep` は第 16.3 節で定義した構造とする。

| エンティティ | 変更権限を持つモジュール | 発行する Event |
|---|---|---|
| Task | State Manager（Core Agent の指示による） | `Task*` |
| Plan / PlanStep | State Manager（Core Agent の判断による） | `Plan*` |
| AgentInstance | Runtime Manager | `Agent*` |
| Message | Message Broker | `AgentMessage*` |
| Result | Result Manager | `TaskResultReceived` |
| Skill | Skill Manager | `Skill*` |
| Hierarchy | Hierarchy Manager（Core Agent の判断による） | `HierarchyComposed` |
| RuntimeInstance | Runtime Manager | `RuntimeInstance*` |
| ToolExecution | Tool Manager | `ToolExecution*` |
| Evaluation | Result Manager（Core Agent の判断による） | `Evaluation*` |
| Conversation / Utterance | Core Host | `Clarification*` |
| Approval | Approval Manager | `Approval*` |
| MemoryRecord | Memory Manager | `MemoryRecord*` |

- **ATH-DATA-003** (MUST) 各エンティティの変更権限は上表のモジュールに限定しなければならない。
- **ATH-DATA-004** (MUST) `instruction` の原文を保持しなければならない。要約で置き換えてはならない。
- **ATH-DATA-009** (MUST) Workspace は Task 単位で排他的にリースしなければならない。同一
  Workspace を対象とする別 Task の作成要求には 409 を返す。1 Task 配下の Agent 群（Director /
  Lead / Worker）は同一 Workspace を共有してよい（ATH-AIPRJ-003）。
- **ATH-DATA-010** (MUST) 同一 PlanStep に対して非終端の Agent Instance が既に存在する場合、
  新たな委任要求を `ValidationError` として拒否しなければならない（冪等キー: `plan_step_id` と
  `attempt`、ATH-CORE-035）。

#### 26.3 Agent / Instance

```rust
pub struct AgentDefinition {
  id: AgentId,
  name: String,
  level: AgentLevel,
  description: String,
  capabilities: Vec<String>,
  persona_ref: String,             // Persona ファイルへの参照
  persona_version: String,
  allowed_tools: Vec<ToolName>,
  delegable_tools: Vec<ToolName>,     // 下位へ付与できる Tool（ATH-SEC-009）
  default_skills: Vec<SkillRef>,
  runtime: { type: RuntimeType },
  model: ModelRef,                 // 必須。既定値を持たない（ATH-AGENT-008）
  max_delegation_depth: u64,
  dynamic: bool,                // 動的生成された定義か
}

pub struct AgentInstance {
  id: InstanceId,
  agent_id: AgentId,
  task_id: Option<TaskId>,          // Core Agent Instance は null（ATH-STATE-012）
  plan_step_id: Option<PlanStepId>,
  superior_instance_id: Option<InstanceId>,
  delegation_depth: u64,
  state: AgentInstanceState,
  runtime_instance_id: Option<String>,
  workspace: String,
  skill_set: Vec<SkillRef>,           // 起動時に固定（ATH-SKILL-010）
  aiprj_log_ref: Option<String>,    // AI_LOG ファイルへの参照
  attempt: u64,
  started_at: Option<String>,
  finished_at: Option<String>,
}

pub struct SkillRef { id: SkillId, version: String, checksum: Option<String> }
pub struct ModelRef { provider: String; name: String; }
```

- **ATH-DATA-005** (MUST) `AgentInstance` は `delegation_depth` を保持し、ATH-HIER-005 の検査に
  使用しなければならない。
- **ATH-DATA-006** (MUST) `skill_set` は起動時に確定した内容を保持し、事後変更してはならない。

#### 26.4 Result / Artifact

```rust
pub struct Artifact {
  result_id: ResultId,
  kind: ArtifactKind,
  base: ArtifactBase,   // path の基準（ATH-RESULT-002）
  path: String,                    // base からの相対パス
  bytes: Option<u64>,
  checksum: Option<String>,
  created_at: String,
}
```

`Result` は第 19.1 節の構造とする。

#### 26.5 Skill / Approval / Memory

```rust
pub struct Skill {
  id: SkillId,
  name: String,
  version: String,                 // Semantic Versioning
  description: String,
  capabilities: Vec<String>,
  required_tools: Vec<ToolName>,
  required_permissions: Vec<Permission>,
  supported_agents: Vec<AgentId>,
  dependencies: Vec<SkillRef>,
  runtime: { provider: String },
  source: String,
  checksum: String,
  publisher: String,
  risk_level: RiskLevel,
  state: SkillState,
  hot_reload_safe: bool,
  registered_at: String,
  updated_at: String,
}

pub struct Approval {
  id: ApprovalId,
  task_id: TaskId,
  requested_by: AgentId,
  operation: String,               // 対象操作の識別
  reason: String,
  impact: String,
  risk_level: RiskLevel,
  state: ApprovalState,
  operation_descriptor: String,    // Core Host が生成（ATH-HITL-010）
  requested_by_instance_id: InstanceId,
  uses_remaining: u64,          // operation_only は 1
  expires_at: String,
  responded_by: Option<String>,     // 承認者識別子
  responded_at: Option<String>,
  scope: { operation_only: true } | { pattern: String; expires_at: String },
}

pub struct MemoryRecord {
  id: MemoryId,
  tier: MemoryTier,
  scope_id: String,                // Instance ID / Task ID / Project ID
  content: String,
  summary: Option<String>,
  origin: MemoryOrigin,
  created_at: String,
  expires_at: Option<String>,
}
```

#### 26.6 Hierarchy / RuntimeInstance / ToolExecution / Evaluation / Conversation

```rust
pub struct Hierarchy {
  id: String,                    // "hier-" + ULID
  task_id: TaskId,
  revision: u64,
  nodes: Vec<HierarchyNode>,
  created_at: String,
}

pub struct HierarchyNode {
  agent_id: AgentId,
  level: AgentLevel,
  superior_agent_id: Option<AgentId>,
  scope_ref: Option<TaskId | PlanStepId>,   // Director=SubTask 群, Lead=SubTask, Worker=PlanStep
  sibling_acl: Vec<AgentId>,                  // 兄弟間通信の許可（ATH-MSG-018）
}

pub struct RuntimeInstance {
  id: String,                    // "ri-" + ULID
  runtime_type: RuntimeType,
  instance_id: InstanceId,
  state: AgentInstanceState,
  pid: Option<u32>,
  last_heartbeat_at: Option<String>,
  started_at: String,
  stopped_at: Option<String>,
  stop_reason: Option<StopReason>,
  side_effects: SideEffects,   // ATH-CORE-048
}

pub struct ToolExecution {
  id: String,                    // "tx-" + ULID
  instance_id: InstanceId,
  task_id: Option<TaskId>,
  tool: ToolName,
  operation_descriptor: String,  // 正規記述子（ATH-HITL-010）
  risk_level: RiskLevel,
  approval_id: Option<ApprovalId>,
  status: ToolExecutionStatus,
  started_at: String,
  finished_at: Option<String>,
}

pub struct Evaluation {
  id: String,                    // "ev-" + ULID
  task_id: TaskId,
  result_id: ResultId,
  evaluator: Evaluator,
  method: String,                // test / lint / build / review / simulation
  verdict: Verdict,
  rationale: String,
  created_at: String,
}

pub struct Conversation {
  id: String,                    // "conv-" + ULID
  task_id: Option<TaskId>,        // chat 開始時は null、Task 作成後に関連付け
  created_at: String,
}

pub struct Utterance {
  id: String,                    // "utt-" + ULID
  conversation_id: String,
  role: UtteranceRole,
  kind: UtteranceKind,
  body: String,
  clarification_id: Option<String>,
  created_at: String,
}
```

`Event` と `Message` は第 23.1 節および第 10.2 節で定義した構造とする。

- **ATH-DATA-011** (MUST) `ProjectId` は Workspace の正規化絶対パスから決定論的に導出しなければ
  ならない。Long Term Memory のスコープはこの `ProjectId` とする（第 13.1 節）。
- **ATH-DATA-012** (MUST) `Artifact` は所属 `Result` への参照（`result_id`）を保持しなければ
  ならない。

- **ATH-DATA-007** (MUST) 上記すべてのエンティティを実装し、第 27 章の永続化対象としなければ
  ならない。
- **ATH-DATA-008** (MUST) エンティティ定義を変更する場合、本書と永続化スキーマを同時に更新しなければ
  ならない。

---

### 27. 永続化

#### 27.1 永続化対象

```text
Task / SubTask
Plan / PlanStep（revision 履歴を含む）
AgentDefinition
AgentInstance
Message
Result / Artifact
Skill / SkillVersion
Runtime / RuntimeInstance
Approval
MemoryRecord
Event
Log 参照
```

- **ATH-PERS-001** (MUST) 上記をすべて永続化しなければならない。
- **ATH-PERS-002** (MUST) 権威データは永続ストアとし、メモリ上の状態は派生とみなさなければならない。
- **ATH-PERS-003** (MUST) 状態遷移と Event の記録は、同一トランザクションで原子的に永続化しなければ
  ならない。遷移のみ成功して Event が失われる状態を許してはならない。

#### 27.2 初期実装のストア

- **ATH-PERS-004** (MUST) 永続化エンジンは**SQLite を使用してはならない**。v1 のエンジンは
  純 Rust の埋め込み型トランザクションストア（`redb`）とし、ファイルは
  `<ATHENA_HOME>/var/` 配下に配置しなければならない。
  （改訂 2026-10-22: 初版の「初期実装は SQLite を使用しなければならない」（`ATH-PERS-004` /
  `ATH-PERS-005`）は、SQLite 使用禁止の指摘（2026-10-18 指示ソース 最優先事項、O-6 で
  エンジン確定）により置き換えた。ID は空位にしない（`ATH-GEN-006`）— `ATH-PERS-005` は
  下記のとおり耐久性条項として残る。）
- **ATH-PERS-005** (MUST) エンジンは ACID トランザクションと障害時の耐久性（コミット済み
  書込みの喪失なし）を保証しなければならない。状態遷移と Event の記録は 1 つの書込み
  トランザクションで原子的に確定しなければならない（ATH-PERS-003）。
- **ATH-PERS-006** (MAY) 将来 PostgreSQL 等へ移行する。移行のため、永続化層はインターフェース
  として分離しなければならない（ATH-PERS-007）。
- **ATH-PERS-007** (MUST) 永続化実装は Repository インターフェース背後に隠蔽し、Core が SQL へ
  直接依存してはならない。

#### 27.3 論理スキーマ

| テーブル | 主キー | 主要索引 | 備考 |
|---|---|---|---|
| `tasks` | `id` | `state`, `parent_id`, `created_at` | — |
| `plans` | `id` | `task_id`, `revision` | `(task_id, revision)` は一意 |
| `plan_steps` | `id` | `plan_id`, `state` | `depends_on` は別表 |
| `plan_step_deps` | `(step_id, depends_on)` | — | 依存関係 |
| `agent_definitions` | `id` | `level` | 動的定義も記録 |
| `agent_instances` | `id` | `task_id`, `state`, `agent_id` | — |
| `messages` | `message_id` | `task_id`, `thread_id`, `created_at` | — |
| `results` | `id` | `task_id`, `plan_step_id` | 不変 |
| `artifacts` | `(result_id, base, path)` | `result_id` | `base` は `work_target` / `store` |
| `skills` | `(id, version)` | `state`, `capabilities` | 版ごとに 1 行 |
| `runtime_instances` | `id` | `state` | — |
| `approvals` | `id` | `task_id`, `state`, `expires_at` | — |
| `memory_records` | `id` | `tier`, `scope_id` | — |
| `plan_step_attempts` | `(step_id, attempt)` | `step_id` | 試行ごとの Instance 対応 |
| `hierarchies` | `id` | `task_id`, `revision` | 階層構成 |
| `hierarchy_nodes` | `(hierarchy_id, agent_id)` | `hierarchy_id` | 階層ノードと sibling ACL |
| `tool_executions` | `id` | `instance_id`, `task_id`, `status` | Tool 実行履歴 |
| `evaluations` | `id` | `task_id`, `result_id` | 評価記録 |
| `conversations` | `id` | `task_id` | chat セッション |
| `utterances` | `id` | `conversation_id`, `created_at` | 発話 |
| `clarifications` | `id` | `task_id`, `state` | 人間への確認 |
| `workspace_leases` | `workspace` | `task_id` | Workspace の排他（ATH-DATA-009） |
| `idempotency_keys` | `key` | `created_at` | 冪等キー（ATH-CORE-035、既定保持 7 日） |
| `message_deliveries` | `(to_instance_id, message_id)` | `message_id` | 配送・受理記録（ATH-MSG-014） |
| `events` | `event_id` | `task_id`, `sequence`, `type`, `occurred_at` | 追記のみ |

- **ATH-PERS-008** (MUST) `events` と `results` は追記専用とし、更新・削除は保持期間満了時の
  削除のみ許容しなければならない。
- **ATH-PERS-009** (MUST) スキーマはバージョン番号を持ち、前方移行スクリプトを提供しなければ
  ならない。
- **ATH-PERS-010** (MUST) 起動時にスキーマバージョンを検証し、不一致の場合は移行を実施するか
  起動を中止しなければならない。黙って動作してはならない。
- **ATH-PERS-011** (MUST) データ格納場所は設定可能とし、既定を
  `<PROJECT_ROOT>/.athena/var/athena.db` としなければならない（ATH-OPS-006、ATH-CFG-010）。

---

### 28. Configuration

#### 28.1 設定ファイル

設定は YAML を基本とする。

```yaml
athena:
  home: ./.athena            # ATHENA が使用するファイルの格納先。既定は cwd から上方へ探索
                             # 環境変数 ATHENA_HOME が対応（ATH-CFG-002、ATH-CFG-010）
  api:
    host: 127.0.0.1
    port: 8080
    auth:
      mode: token              # token | none（none は開発時のみ）
  data:
    dir: ${athena.home}/var
    database: ${athena.home}/var/athena.db   # プロジェクト単位

core:
  max_iterations: 50
  iteration_timeout: 30m
  evaluation:
    require_verification: true
  agent:                          # Core Agent（agent-cli instance #0）
    persona: ${athena.home}/personas/producer.md
    workspace: ${athena.home}/var/core-agent
    runtime: agent-cli
    provider: claude
    model: claude-opus-5
    skills:
      - id: athena-orchestration
        version: "1.0.0"
    restart:
      max_attempts: 3
      on_exhausted: escalate      # escalate のみ許容（ATH-CORE-030）
    allowed_tools:                  # Core Agent 自身が実行できる Tool（最小：ATH-SEC-025）
      - fs_read
      - fs_write
      - send_to
    delegable_tools:                # 下位 Agent へ付与できる Tool（ATH-SEC-023）
      - fs_read
      - fs_write
      - shell
      - git
      - search
      - send_to
      - network

scheduler:
  max_concurrent_tasks: 8
  max_concurrent_instances: 16        # WAITING / SUSPENDED は計上外（ATH-SCHED-010）
  max_instances_per_task: 4
  max_queue_length: 1000
  priority_promotion_interval: 30m
  reserved_core_agent_slots: 2        # 上限の外側に確保（ATH-SCHED-008）
  max_waiting_approval_ratio: 0.25
  stall_detection_factor: 2           # iteration_timeout の倍数で TaskStalled

messaging:
  max_queue_depth: 1000
  dedup_retention: 7d

timeout:                              # 全 Runtime 共通の既定値（第 20.3 節）
  startup: 30s
  execution: 30m
  message_reply: 45m                  # startup + execution より長いこと（ATH-CFG-008）
  approval: 1h
  stop: 60s
  clarification: 24h

policy:                               # リスク分類（ATH-HITL-002）
  rules:
    - match: { tool: shell, args_pattern: "^git\\s+push" }
      risk: high
      requires_approval: true
    - match: { tool: shell, args_pattern: "^rm\\s+-rf" }
      risk: high
      requires_approval: true
    - match: { tool: network, hosts: ["*"] }
      risk: high
      requires_approval: true
    - match: { tool: fs_write, path_scope: "outside_workspace" }
      risk: high
      requires_approval: true
    - match: { tool: shell }
      risk: medium
      requires_approval: false
    - match: { tool: fs_read }
      risk: low
      requires_approval: false
  approval_scopes:                    # 選択可能な scope（ATH-HITL-012）
    - operation_only
    - "tool:shell:same_argv"
    - "tool:fs_write:same_path"
  max_approval_ttl: 24h

runtime:
  default: agent-cli
  agent-cli:
    max_instances: 16               # Σ runtime.*.max_instances >= scheduler 上限（ATH-CFG-008）
    command: agent-cli
    startup_timeout: 30s            # 任意。timeout.startup の Runtime 単位の上書き
    execution_timeout: 30m          # 任意。timeout.execution の Runtime 単位の上書き

skills:
  local_dir: ${athena.home}/skills     # プロジェクト内 Package のソース（ATH-SKILL-037）
  dir: ${athena.home}/var/skills       # 登録済み（ATH-SKILL-038）
  auto_enable_on_register: false
  verify_checksum: true
  allowed_publishers: [aquaxis]        # ATH-SKILL-035
  allowed_sources: ["https://skills.example.invalid/"]

memory:
  tokenizer: provider-default-claude   # 計数に用いるトークナイザ（ATH-MEM-009）
  summarizer: memory-summarizer-agent  # 要約を担う Agent 定義（ATH-MEM-008）
  short_term_max_tokens: 32000
  working_max_records: 500
  long_term:
    enabled: true
    max_records: 10000

approval:
  on_timeout: reject                   # 期限は timeout.approval（ATH-HITL-005）

core_agent_restart:
  max_attempts: 3
  window: 1h                           # スライディングウィンドウ（ATH-CORE-045）
  exclude_error_classes: [TransientError]
  heartbeat_interval: 20s
  heartbeat_timeout: 60s               # ATH-CORE-047

retry:
  max_attempts: 3
  strategy: alternate-agent
  backoff:
    initial_ms: 1000
    multiplier: 2
    max_ms: 60000

events:
  retention_days: 90

observability:
  log_level: info
  log_format: json

agents:
  - id: fpga-director
    runtime: agent-cli
  - id: timing-lead
    runtime: agent-cli
  - id: research-agent
    runtime: agent-cli
```

#### 28.2 優先順位と検証

```text
既定値 < 設定ファイル < 環境変数 < CLI オプション
```

- **ATH-CFG-001** (MUST) 上記の優先順位で設定を解決しなければならない。
- **ATH-CFG-002** (MUST) 環境変数は `ATHENA_` 接頭辞と大文字スネークケースで対応させなければ
  ならない（例 `ATHENA_API_PORT` → `athena.api.port`）。
- **ATH-CFG-003** (MUST) 起動時に設定全体をスキーマ検証し、不正な設定では起動を中止しなければ
  ならない（fail fast）。部分的な既定値補完で続行してはならない。
- **ATH-CFG-004** (MUST) 解決後の有効設定を `ConfigurationLoaded` Event として記録しなければ
  ならない。ただし秘密情報は含めない。
- **ATH-CFG-005** (MUST NOT) 設定ファイルに API トークン、LLM Provider の鍵、その他の秘密情報を
  記述してはならない。秘密情報は環境変数または外部の秘密管理機構から取得する。
- **ATH-CFG-006** (MUST) `athena.api.auth.mode: none` は `athena.api.host` が 127.0.0.1 の場合に
  限り許容しなければならない。それ以外の設定との組み合わせは検証エラーとする。
- **ATH-CFG-007** (SHOULD) 設定の再読込は再起動によって行う。無停止の設定変更は Skill の登録・
  有効化に限る（ATH-SKILL-008）。
- **ATH-CFG-008** (MUST) 起動時に以下の相互制約を検証し、違反時は起動を中止しなければならない。

```text
Σ runtime.<type>.max_instances >= scheduler.max_concurrent_instances
timeout.message_reply          >  timeout.startup + timeout.execution
timeout.stop                   >  0
policy.max_approval_ttl        >= timeout.approval
scheduler.reserved_core_agent_slots >= 1
```

Runtime 単位の上書き（`runtime.<type>.startup_timeout` / `runtime.<type>.execution_timeout`）を
設定した場合、その Runtime についても同じ制約を満たさなければならない。

```text
timeout.message_reply > runtime.<type>.startup_timeout + runtime.<type>.execution_timeout
```

- **ATH-CFG-009** (MUST) `policy.rules` は上から順に評価し、最初に一致した規則を採用しなければ
  ならない。一致する規則が存在しない操作は `high` として扱う（既定拒否）。
- **ATH-CFG-010** (MUST) `athena.home` を `<PROJECT_ROOT>/.athena`（`ATHENA_HOME`、第 4.3.4 節）
  として解決しなければならない。解決順序は以下とし、最初に定まった値を採用する。

| 順 | 解決元 |
|---|---|
| 1 | CLI オプション（`--home`） |
| 2 | 環境変数 `ATHENA_HOME`（ATH-CFG-002 の対応規則による） |
| 3 | カレントディレクトリから上方へ `.athena/` を探索（最初に見つかったものを採用） |

  上記のいずれでも定まらない場合、起動を中止し、`athena init` によるプロジェクトへの導入を
  促すメッセージを返さなければならない。既定値による暗黙の作成を行ってはならない。
  `athena.home` をプロジェクト外へ向けることは可能だが、その場合も 1 インスタンスが担当する
  プロジェクトは 1 つである（ATH-OPS-007）。

  探索を解決手段としたのは、ATHENA がプロジェクト単位で構築されるためである。実行ファイルの
  位置は、パッケージマネージャによる導入やグローバル配置では担当プロジェクトを特定できない。
  設定ファイル自身の位置も導出元にしてはならない（`--config` で任意の位置を指定できるため、
  配置が一意に定まらない）。

  設定値中の `${athena.home}` は、他の設定の検証（ATH-CFG-003）より**前**に展開しなければ
  ならない。設定値に現れる相対パスは `athena.home` を基準に解決する。`${athena.home}` 以外の
  変数参照を導入してはならない。
- **ATH-CFG-011** (MUST) 設定ファイルの既定の位置は `<ATHENA_HOME>/config.yaml` としなければ
  ならない。`--config` による明示指定がない場合はこれを読み込む。

---

### 29. Security

#### 29.1 信頼境界

```text
[1] Human ──────────────► Core API        認証・認可が必要
[2] Core API ───────────► Core            プロセス内（同一信頼境界）
[3] Core ───────────────► Runtime         プロセス内または IPC
[4] Runtime ────────────► Agent Process   信頼境界（Agent は信頼しない）
                                          Core Agent Process も同様に外側
[5] Agent Process ──────► Tool / 外部     権限制御が必要
[6] 外部 Skill ─────────► Registry        検証が必要
```

- **ATH-SEC-001** (MUST) Agent Process を信頼境界の外側として扱わなければならない。Agent の出力・
  要求を検証なしに特権操作へ接続してはならない。
- **ATH-SEC-002** (MUST) 境界 [4] [5] [6] のそれぞれで権限検査を実施しなければならない。
- **ATH-SEC-021** (MUST) Core Agent Process を信頼境界の外側として扱わなければならない。
  Core Agent が最上位の権限を持つことと、その要求を無検証で実行することは別である
  （ATH-CORE-026）。
- **ATH-SEC-022** (MUST) Core Host を唯一の権限強制点（Policy Enforcement Point）として実装しなければ
  ならない。権限検査、承認判定、状態遷移の許可を Agent Process 側へ委ねてはならない。
- **ATH-SEC-023** (MUST) Core Agent の `delegable_tools` は ATHENA 全体の権限上限となるため、
  設定で明示しなければならない。既定で無制限としてはならない。
- **ATH-SEC-026** (MUST) Tool 実行とリスク分類の強制点は Core Host の Tool Manager および
  Policy Manager とする。Runtime 内の Skill Resolver による検査（ATH-SEC-012）は多層防御であり、
  独立した方針決定ではない（ATH-SEC-002）。

#### 29.2 API の認証・認可

| ロール | 権限 |
|---|---|
| `viewer` | Task / Agent / Event / Result の読み取り |
| `operator` | `viewer` + Task 作成・実行・取消、承認応答 |
| `admin` | `operator` + Skill 登録・有効化・削除、設定参照、Instance 強制停止 |

- **ATH-SEC-003** (MUST) 上記 3 ロールを実装しなければならない。
- **ATH-SEC-004** (MUST) トークンはロールを保持し、エンドポイントごとに必要ロールを検査しなければ
  ならない。
- **ATH-SEC-005** (MUST) トークンはハッシュ化して保存しなければならない。平文で保存してはならない。
- **ATH-SEC-006** (MUST) 認証失敗、認可拒否を Event およびログへ記録しなければならない。
- **ATH-SEC-007** (SHOULD) トークンに有効期限を設定する。
- **ATH-SEC-027** (MUST) v1 の認証方式は**静的に発行した Bearer トークン**とする。トークンは
  管理者が発行し、ロールを 1 つ割り当て、ハッシュのみを永続化する（ATH-SEC-005）。
  OIDC / OAuth2 および mTLS を v1 の要件としてはならない。これらは多利用者運用（ATH-OPS-004）が
  要求された時点で、第 22.1 節の `Authorization: Bearer` の形式を保ったまま追加する。
  追加時もロール検査（ATH-SEC-004）の位置は変更しない。

#### 29.3 Agent の権限

Agent の権限は以下の要素で構成する。

```text
allowed_tools          自身が実行できる Tool
delegable_tools        下位 Agent へ付与できる Tool（自身の実行権限とは独立）
filesystem_scope       読み書き可能なパス範囲（制御面を除く：ATH-CORE-043）
network_access         allow | deny | allowlist
process_access         プロセス操作の可否
delegation_depth       委任可能な深さ
risk_ceiling           承認なしで実行できる最大リスク
```

権限は「自身が使える権限」と「下位へ渡せる権限」を分離する。これにより、常駐かつ外部から
操作され得る Core Agent 自身の実行権限を最小に保ちながら、下位 Agent へ必要な権限を付与できる。

- **ATH-SEC-008** (MUST) Agent 権限は既定で最小としなければならない（既定: `fs_read` のみ、
  network `deny`）。
- **ATH-SEC-009** (MUST) 下位 Agent の `allowed_tools` は、上位 Agent の `delegable_tools` の
  部分集合でなければならない。

```text
allowed_tools(child) ⊆ delegable_tools(parent)
```

- **ATH-SEC-024** (MUST) `delegable_tools` は設定で明示しなければならない。既定は空集合とする。
- **ATH-SEC-025** (MUST) Core Agent の `allowed_tools` は最小（既定: `fs_read`、`fs_write`、
  `send_to`）としなければならない。`shell` / `process` / `network` を Core Agent 自身の
  `allowed_tools` へ既定で含めてはならない。これらは `delegable_tools` として付与する。
- **ATH-SEC-010** (MUST) `filesystem_scope` は Agent Workspace（cwd）と作業対象の絶対パス
  （ATH-OPS-010）の和集合とし、境界外アクセスを
  拒否しなければならない（ATH-TOOL-005）。
- **ATH-SEC-011** (MUST) Agent が自身の権限を変更する操作を提供してはならない。権限変更は
  ATHENA 側の設定と承認に基づく。

#### 29.4 Skill の権限

- **ATH-SEC-012** (MUST) 以下を満たさない Skill を実行してはならない。

```text
permissions(Skill) ⊆ permissions(Agent)
```

検査実施点：Skill Resolver（Agent 起動前）。検査失敗時は `PermissionError` を返し、Agent を
起動しない。

- **ATH-SEC-013** (MUST) 外部取得 Skill は checksum と publisher を検証してから登録しなければ
  ならない（ATH-SKILL-022）。
- **ATH-SEC-014** (MUST) リスク `high` の Skill 利用は承認を要求しなければならない
  （ATH-SKILL-024）。

#### 29.5 Runtime の分離

| Runtime | 分離境界 | 初期実装 |
|---|---|---|
| agent-cli | OS プロセス + Workspace パス境界 | 対象 |
| LocalProcess | OS プロセス | 将来 |
| Docker | コンテナ | 将来 |
| SSH | リモートホスト | 将来 |
| Kubernetes | Pod | 将来 |

- **ATH-SEC-015** (MUST) Runtime は申告した分離境界（ATH-RT-005）を実際に強制しなければならない。
- **ATH-SEC-016** (SHOULD) 外部から取得した Skill を使用する Agent は、コンテナ等のより強い
  分離境界を持つ Runtime で実行する。

#### 29.6 秘密情報

- **ATH-SEC-017** (MUST NOT) 秘密情報を設定ファイル、Event payload、Message、ログ、Memory、
  Artifact へ平文で保存してはならない。
- **ATH-SEC-018** (MUST) ログおよび Event の出力時に、既知の秘密情報パターンを遮蔽しなければ
  ならない。
- **ATH-SEC-019** (MUST) Agent へ渡す秘密情報は、必要最小限の範囲と期間に限定しなければならない。
- **ATH-SEC-020** (MUST) 機密情報へのアクセスは承認対象としなければならない（第 15.1 節）。

---

### 30. Observability

#### 30.1 必須記録項目

```text
Task ID / Plan ID / PlanStep ID
Agent ID / Instance ID / Runtime ID
Skill ID + Version
Start Time / End Time / Duration
State / Exit Code
Error Class / Error Code
Correlation ID / Causation ID
Logs 参照 / Events
```

- **ATH-OBS-001** (MUST) 上記項目を記録しなければならない。

#### 30.2 構造化ログ

```json
{
  "ts": "2026-10-12T10:00:00.123Z",
  "level": "info",
  "module": "runtime.agent-cli",
  "message": "agent instance started",
  "task_id": "task-001",
  "agent_id": "timing-lead",
  "instance_id": "ai-0001",
  "correlation_id": "cor-0001",
  "duration_ms": 842
}
```

- **ATH-OBS-002** (MUST) ログは JSON 構造化形式で出力しなければならない。
- **ATH-OBS-003** (MUST) すべてのログ行に `correlation_id` を含めなければならない。
- **ATH-OBS-004** (MUST) ログレベルは `debug` / `info` / `warn` / `error` としなければならない。
- **ATH-OBS-005** (MUST NOT) Agent への入力全文や Agent 出力全文をログへ常時出力してはならない。
  `debug` レベルかつ明示設定時に限る。

#### 30.3 メトリクス

| メトリクス名 | 型 | 内容 |
|---|---|---|
| `athena_tasks_total` | counter | 状態別 Task 数（label: `state`） |
| `athena_tasks_running` | gauge | 実行中 Task 数 |
| `athena_task_duration_seconds` | histogram | Task 実行時間 |
| `athena_agent_instances_running` | gauge | 実行中 Instance 数（label: `runtime`） |
| `athena_agent_instance_duration_seconds` | histogram | Instance 実行時間 |
| `athena_agent_failures_total` | counter | Agent 失敗数（label: `error_class`） |
| `athena_messages_total` | counter | Message 数（label: `type`） |
| `athena_message_queue_depth` | gauge | Broker 滞留数 |
| `athena_events_written_total` | counter | 書き込み Event 数 |
| `athena_replans_total` | counter | 再計画回数 |
| `athena_retries_total` | counter | 再試行回数（label: `error_class`） |
| `athena_approvals_total` | counter | 承認結果別件数（label: `outcome`） |
| `athena_approval_wait_seconds` | histogram | 承認待ち時間 |
| `athena_skill_loads_total` | counter | Skill ロード数（label: `skill_id`） |
| `athena_skill_validation_failures_total` | counter | Skill 検証失敗数 |
| `athena_api_requests_total` | counter | API 要求数（label: `route`, `status`） |
| `athena_api_request_duration_seconds` | histogram | API 応答時間 |
| `athena_core_agent_last_event_age_seconds` | gauge | Core Agent の最終 Event からの経過秒数 |
| `athena_instances_waiting` | gauge | `WAITING` / `SUSPENDED` の Instance 数（label: `state`） |
| `athena_oldest_nonterminal_task_age_seconds` | gauge | 最古の非終端 Task の経過秒数 |
| `athena_scheduler_blocked_total` | counter | 資源上限で起動できなかった回数 |
| `athena_privileged_requests_denied_total` | counter | 特権操作の拒否数（label: `request_kind`） |
| `athena_core_agent_restarts_total` | counter | Core Agent の再起動回数 |

- **ATH-OBS-006** (MUST) 上表のメトリクスを提供しなければならない。
- **ATH-OBS-007** (MUST) メトリクスを `GET /api/v1/metrics` で公開しなければならない。
- **ATH-OBS-008** (MAY) 将来 Tracing / Distributed Tracing へ拡張する。span 名は
  `athena.<module>.<operation>` 形式とする。

#### 30.4 相関キー

```text
Human 指示
  → Task ID
    → correlation_id（Task 単位で生成）
      → Event（correlation_id / causation_id）
      → 構造化ログ（correlation_id）
      → Agent Instance（aiprj_log_ref → .aiprj/AI_LOG/YYYY-MM-DD_NNN.md）
```

- **ATH-OBS-009** (MUST) Task、Event、ログ、aiprj AI_LOG を単一の相関キーで突き合わせ可能に
  しなければならない。
- **ATH-OBS-011** (MUST) Core Agent は `CoreAgentHeartbeat` Event を既定 20 秒間隔で発行しなければ
  ならない。これにより「状態遷移の不在」を検出できる。
- **ATH-OBS-012** (MUST) 非終端 Task について、状態変化 Event が
  `core.iteration_timeout × scheduler.stall_detection_factor` の間発生しない場合、`TaskStalled`
  Event を発行し人間へエスカレーションしなければならない（ATH-NFR-010 の補完）。
- **ATH-OBS-013** (MUST) `AgentWaiting` の `waits_on` から待機グラフを構成し、循環を検出した場合
  `TaskStalled` を発行しなければならない。
- **ATH-OBS-010** (MUST) Agent Instance の記録から対応する AI_LOG を特定できなければならない
  （ATH-EVENT-016）。

---

### 31. 非機能要件

ATHENA は拡張性、可観測性、再現性、障害耐性、疎結合、UI 独立性を重視する。本章はこれらを
測定可能な目標として定義する。目標値は v1 の工学目標であり、計測による確定が必要な項目は
各項目の「検証方法」欄に示す。

| ID | 要求 | 目標 | 検証方法 |
|---|---|---|---|
| **ATH-NFR-001** (MUST) | 同時実行 Agent Instance | 16 Instance を同時実行し（Core Agent の予約枠は別枠）、Instance 間の状態汚染がないこと | 負荷テスト（16 並列、状態検証） |
| **ATH-NFR-002** (MUST) | API 応答時間 | 読み取り系 p95 < 200ms（Task 1 万件時） | ベンチマーク |
| **ATH-NFR-003** (MUST) | 障害からの回復 | Core プロセス強制終了後の再起動で、非終端 Task をすべて永続ストアから復元し 10 秒以内に再開可能 | 統合テスト（`kill -9` → 再起動 → 状態照合） |
| **ATH-NFR-004** (MUST) | Event 書き込み | 200 event/s を取り落ちなく永続化 | 負荷テスト |
| **ATH-NFR-005** (MUST) | 再現性 | 同一 Task の実行履歴（Plan revision、Agent、Skill 版、Result）を完全に再構成可能 | Event replay テスト（ATH-EVENT-012） |
| **ATH-NFR-006** (MUST) | 拡張性（Runtime） | 新規 Runtime Adapter の追加に Core の変更を要さない | 契約テスト + 擬似 Runtime 追加 |
| **ATH-NFR-007** (MUST) | 拡張性（Skill） | 稼働中に Skill を追加し、再起動なしで次 Task から利用可能 | 統合テスト |
| **ATH-NFR-008** (MUST) | 疎結合 | Core のテストが `agent-cli` の実体を必要としない | 擬似 Runtime による Core テスト |
| **ATH-NFR-009** (MUST) | UI 独立性 | CLI と Studio が機能差なく同一 API を利用 | API 利用の網羅確認 |
| **ATH-NFR-010** (MUST) | 可観測性 | 任意の Task について、受付から終了までの全状態遷移を Event から再構成可能 | Event 連続性検査 |
| **ATH-NFR-011** (SHOULD) | 起動時間 | `athena-api` の起動完了まで 3 秒以内 | 計測 |
| **ATH-NFR-012** (SHOULD) | ログ量 | 通常運用で 1 Task あたり 1MB 未満 | 計測 |
| **ATH-NFR-013** (MUST) | 停止の安全性 | 停止要求から 30 秒以内に全 Instance を停止し、状態を永続化 | 統合テスト |
| **ATH-NFR-014** (MUST) | Core Agent の回復 | Core Agent Instance の強制終了後、Core Host が 30 秒以内に再起動し、文脈を再投入して判断を再開できる | 障害注入テスト（instance #0 を `kill -9` → 文脈照合） |
| **ATH-NFR-015** (MUST) | 障害の非波及 | 下位 Agent Instance の異常終了が Core Agent Instance へ波及しない（逆も同様）。下位 Agent の応答遅延で Core Agent が失敗しない（ATH-STATE-023） | 障害注入テスト（ATH-RT-013） |
| **ATH-NFR-016** (MUST) | 並行性 | 単一の Core Agent Instance が 8 Task を並行して進行させられる（`thread_id` 単位の処理） | 負荷テスト（8 Task 同時、各 Task の進行を確認） |
| **ATH-NFR-017** (MUST) | 二重実行の防止 | Core Agent 再起動後に同一 PlanStep が二重に委任されない | 障害注入テスト（委任直後に instance #0 を強制終了） |
| **ATH-NFR-018** (MUST) | 停滞の検出 | 階層の相互待ちおよび資源枯渇による停滞を、タイムアウト前に Event として検出できる | 統合テスト（意図的な hold-and-wait を構成） |

---

## Part VI 開発

### 32. Repository 構成とモジュール境界

#### 32.1 Repository 構成

```text
athena/
|
+-- Cargo.toml                         workspace 定義
|
+-- crates/
|   |  --- バイナリ ---
|   +-- athena-api/        Core API（常駐プロセス）。athena-core を静的リンク
|   +-- athena-cli/        athena（CLI）
|   +-- athena-studio/     Studio（Rust → WebAssembly）
|   |
|   |  --- ライブラリ ---
|   +-- athena-core/       Core Host（監督・状態・Event・権限強制）
|   +-- athena-agent/      Agent 定義 / Instance / 階層
|   +-- athena-runtime/    Runtime Interface
|   +-- athena-messaging/  Message / Broker
|   +-- athena-memory/     Memory Manager
|   +-- athena-tools/      Tool 定義と権限
|   +-- athena-skills/     Skill Manager / Registry / Catalog / Resolver
|   +-- athena-state/      State Manager / 遷移表
|   +-- athena-events/     Event 定義 / Store
|   +-- athena-persistence/ Repository 実装（`redb`）
|   +-- athena-config/     設定スキーマと解決
|   +-- athena-types/      共通型（第 26 章）
|
+-- runtimes/
|   +-- athena-runtime-agent-cli/   agent-cli Adapter と Process Manager
|   +-- athena-runtime-local/       LocalProcess Runtime（将来）
|
+-- agents/
|   +-- producer/
|   +-- director/
|   +-- leads/
|   +-- workers/
|
+-- personas/
|
+-- skills/
|
+-- tests/
|
+-- docs/
|
+-- README.md
+-- LICENSE                            MIT（ATH-DEV-019）
```

上記は **ATHENA 自身を開発するリポジトリ**の構成である。ATHENA を利用するプロジェクトへ
導入した結果の構成（`<PROJECT_ROOT>/.athena/`）とは別物であり、混同してはならない。

| | ATHENA 開発リポジトリ（本節） | 導入先プロジェクト（第 4.3.4 節） |
|---|---|---|
| 目的 | ATHENA の実装 | ATHENA による作業の実施 |
| 内容 | `crates/` / `runtimes/` / `tests/` | `.athena/` |
| `agents/` `personas/` `skills/` | 配布する既定の定義・Persona・Skill Package | そのプロジェクト固有の定義。`athena init` が既定分を複製する |
| 利用者 | ATHENA の開発者 | ATHENA の利用者 |

本リポジトリの `agents/` / `personas/` / `skills/` は、`athena init` が導入先の
`.athena/` へ配置する**既定の内容**である。導入後は各プロジェクトが独立して保持する。

- **ATH-DEV-009** (MUST) リポジトリは上記構成に従わなければならない。
- **ATH-DEV-020** (MUST) 導入先プロジェクトでは `.athena/var/` をバージョン管理対象から除外し
  なければならない。`config.yaml`、`agents/`、`personas/`、`skills/` は版管理対象としてよい
  （プロジェクトの構成情報であるため）。`athena init` は `.athena/.gitignore` に `var/` を
  記載しなければならない。実行時データを版管理へ混入させてはならない（ATH-OPS-006）。
- **ATH-DEV-021** (MUST NOT) `config.yaml` へ秘密情報を記述してはならない（ATH-CFG-005）。
  版管理対象となりうるため、API トークンおよび LLM Provider の鍵は環境変数または外部の秘密
  管理機構から取得する。
- **ATH-DEV-010** (MUST) バイナリ crate 名は第 4.3.2 節の実行単位名と一致させなければ
  ならない。
- **ATH-DEV-019** (MUST) 本リポジトリは **MIT License** で配布し、`LICENSE` をリポジトリ直下に
  置かなければならない。参照する `aiprj` / `agent-cli`（第 2 章）と同一条件を採る。第三者由来の
  コードおよび Skill を取り込む場合は、ライセンスの両立性を取り込み前に確認し、出典を記録
  しなければならない（ATH-SKILL-036）。

#### 32.2 モジュール境界の強制

保たれるべき境界は 3 つである。これらが規範であり、crate 名は表現にすぎない。

| 境界 | 内容 |
|---|---|
| Core は特定の Runtime に依存しない | Core は Runtime Interface のみを知る（ATH-RT-007） |
| UI は Core API のみを経由する | CLI と Studio は Core の内部へ到達しない（ATH-API-001） |
| ライブラリはバイナリに依存しない | 依存の向きを一方向に保つ |

許可される依存方向：

```text
athena-studio    → (HTTP / SSE) athena-api
athena-cli       → (HTTP / SSE) athena-api
athena-api       → athena-core, athena-types
athena-api       → runtimes/*                常駐プロセスは構成ルート（composition root）である。
                                             Core Host（athena-core）と Adapter 実装を同一プロセスで
                                             結線し、起動時に registry へ登録するのは常駐プロセスの
                                             責務である（第 2.1 節、ATH-RT-008）。Core が Adapter を
                                             知らない不変条件は変わらない（athena-core → runtimes/*
                                             は引き続き禁止）。
athena-core      → athena-agent, athena-messaging, athena-memory, athena-tools,
                   athena-skills, athena-state, athena-events, athena-persistence,
                   athena-config, athena-runtime, athena-types
athena-runtime   → athena-types
athena-skills    → athena-types
runtimes/*       → athena-runtime, athena-skills, athena-types
```

`runtimes/* → athena-skills` を許可する理由は、Skill Manager が Runtime の抽象層に属し
（第 6.1 節、ATH-SKILL-012）、Adapter が選択済み Skill を Instance の領域へ配置する
（ATH-SKILL-017）ためである。

禁止される依存：

```text
athena-core      → runtimes/*               禁止
athena-core      → agent-cli 固有 API       禁止
ライブラリ crate → バイナリ crate           禁止
athena-studio    → athena-core              禁止
athena-cli       → athena-core              禁止
runtimes/*       → athena-core              禁止
```

- **ATH-DEV-011** (MUST) 上記の禁止依存を検出する静的検査を CI に組み込まなければならない
  （ATH-DEV-004）。依存関係は workspace の各 `Cargo.toml` に宣言されるため、検査は依存グラフの
  照合として実装する。
- **ATH-DEV-012** (MUST) `athena-runtime` は Adapter 実装への参照を持ってはならない。Adapter は
  実行時に登録する（ATH-RT-008）。
- **ATH-DEV-023** (MUST) 上記のいずれにも該当しない依存を追加する場合、本節へ許可または禁止と
  して明記しなければならない。列挙にない依存を暗黙に許容してはならない。

---

### 33. 開発フェーズと MVP 受け入れ条件

#### 33.1 フェーズ

Core Agent Loop を最初に完成させる計画を採る。

| Phase | 内容 | 完了条件 |
|---|---|---|
| Phase 1 | Core Agent Loop（Understand / Plan / Decide / Act / Observe / Evaluate / Re-plan）、Core Host（監督・状態・Event・権限強制）、Task / Plan、擬似 Runtime 上の Core Agent Instance | 擬似 Runtime 上で Agent Loop が完結し、Event から全遷移を再構成できる。Core Agent の再起動と文脈再投入が動作する |
| Phase 2 | Agent Hierarchy（Producer / Director / Lead / Worker）、Message Broker、Persona | 2 階層以上の委任が動作し、委任深さ制限が機能する |
| Phase 3 | agent-cli Adapter、Workspace / aiprj 初期化、Process 管理、Core Agent を `agent-cli` #0 へ移行 | 実 Agent による Task 完遂。Core Agent と下位 Agent が別インスタンスで動作する |
| Phase 4 | Tool / Memory / Artifact、Human-in-the-Loop、Retry / Timeout / Cancellation / Recovery | 高リスク操作が承認を経て実行され、失敗が再計画へ接続される |
| Phase 5 | Skill Manager（Registry / Catalog / Resolver / Loader / Validator）、Skill API / CLI | 稼働中の Skill 追加が次 Task から利用可能 |
| Phase 6 | ATHENA CLI 完成（`athena chat` 中心） | CLI のみで全機能を操作可能 |
| Phase 7 | Studio | Dashboard / Task / Hierarchy / Approval / Skill / Event の表示と操作 |
| Phase 8 | Advanced Agent / Skill Selection、長期 Memory、Self Recovery、Agent Evaluation | 総合評価による選択が動作 |
| Phase 9 | Hub（要求が発生した場合） | — |
| Phase 10 | SDK（要求が発生した場合） | — |

- **ATH-DEV-018** (MUST) Phase 1 と Phase 2 の擬似 Runtime は、Runtime Interface（第 14.2 節）の
  実装として `athena-runtime` crate 内に置かなければならない。`agent-cli` Adapter（Phase 3）への
  依存を Phase 1 / Phase 2 で作ってはならない。
- **ATH-DEV-013** (MUST) Phase 1 を最優先で完成させなければならない（ATH-POLICY-015）。
- **ATH-DEV-022** (MUST) Phase 1 の着手前に、`agent-cli` の長命セッションの実挙動（文脈の保持、
  heartbeat、再起動時の文脈再投入）を検証しなければならない。第 2.2 節の前提と実装が異なる
  場合、第 2.2 節、第 7.5 節、第 14.4 節を改訂してから実装を進める。未検証のまま Adapter を
  実装してはならない。Core Agent が `agent-cli` #0 上の常駐 Agent であるため（第 7.5 節）、
  差分の影響は Adapter に留まらない。
- **ATH-DEV-014** (MUST) Phase 1 の Core テストを Studio 実装より先に完成させなければならない
  （ATH-DEV-003）。
- **ATH-DEV-015** (MUST) Phase 9 / Phase 10 は要求が発生するまで着手してはならない。

#### 33.2 MVP の範囲

```text
athena chat
    |
    v
athena-api → athena-core (Core Host)
    |
    +-- agent-cli #0 -- Core Agent（判断）
    |
    +-- agent-cli #1 -- Agent A
    +-- agent-cli #2 -- Agent B
    +-- agent-cli #3 -- Agent C
    |
    v
Result Evaluation（Core Agent が評価、Core Host が記録）
    |
    v
Human
```

MVP は Phase 1 から Phase 4 の範囲とする。

#### 33.3 MVP 受け入れ条件

MVP の受け入れ条件は、外部から観測可能な検証可能条件として定義する。

| ID | 受け入れ条件 | 検証方法 |
|---|---|---|
| **ATH-TEST-001** (MUST) | 人間が `athena chat` で自然言語指示を投入でき、Task が `CREATED` → `QUEUED` へ遷移する | E2E: 指示投入後 `GET /tasks/{id}` が `QUEUED` 以降、`TaskCreated` / `TaskQueued` Event が存在 |
| **ATH-TEST-002** (MUST) | Core Agent が指示から goal / constraints / completion_criteria を抽出し保存する | Task の該当フィールドが非空であること |
| **ATH-TEST-003** (MUST) | Plan が生成され、1 つ以上の PlanStep を持つ | `GET /tasks/{id}/plan` が `steps.length >= 1`、`PlanCreated` Event が存在 |
| **ATH-TEST-004** (MUST) | 必要 capability に適合する Agent が選択され、選択理由が記録される | `AgentSelected` Event に選択理由と候補が含まれる |
| **ATH-TEST-005** (MUST) | Agent Instance が起動し `IDLE` → `BUSY` へ遷移する | `AgentStarted` Event と Instance 状態 |
| **ATH-TEST-006** (MUST) | Core Agent が Agent へ `instruction` を送信する | `AgentMessageSent` Event と Message 記録 |
| **ATH-TEST-007** (MUST) | Agent の Result を受領し永続化する | `GET /tasks/{id}/results` が Result を返す |
| **ATH-TEST-008** (MUST) | Result が完了条件と照合され、検証対象成果物は Verification を経る | `EvaluationStarted` / `EvaluationPassed` Event、検証実施記録 |
| **ATH-TEST-009** (MUST) | 評価が不充足の場合、再計画または再試行が発生する | `TaskReplanning` または `TaskRetrying` Event、Plan revision の増加 |
| **ATH-TEST-010** (MUST) | 最終結果が利用者へ返却され Task が終端状態へ遷移する | CLI 出力に最終結果、`TaskCompleted` または `TaskFailed` Event |
| **ATH-TEST-011** (MUST) | 高リスク操作が承認なしに実行されない | 承認拒否時に対象操作が実行されないことを Tool 実行 Event の不在で確認 |
| **ATH-TEST-012** (MUST) | Core 再起動後に非終端 Task を復元できる | ATH-NFR-003 の統合テスト |

- **ATH-DEV-016** (MUST) MVP は上表のすべての条件を満たさなければならない。

---

### 34. テスト戦略

| 層 | 対象 | 要求 |
|---|---|---|
| 単体テスト | 状態遷移表、Plan 検証、権限検査、設定解決、エラー分類 | **ATH-TEST-013** (MUST) 第 17 章の遷移表の全行、および許可されない遷移の拒否を網羅する |
| 契約テスト | Runtime Adapter | **ATH-TEST-014** (MUST) すべての Adapter が同一の契約テスト集合に合格する |
| 統合テスト | Core + 擬似 Runtime | **ATH-TEST-015** (MUST) `agent-cli` の実体なしで Agent Loop 全体を検証する |
| 統合テスト | Core + agent-cli Adapter | **ATH-TEST-016** (MUST) 実 Agent による Task 完遂を検証する |
| E2E | `athena chat` シナリオ | **ATH-TEST-017** (MUST) 第 33.3 節の受け入れ条件を自動実行する |
| 障害注入 | Timeout / Agent 失敗 / Runtime 異常 / 承認期限切れ | **ATH-TEST-018** (MUST) 第 20.1 節の全エラー分類について、規定の処理（再試行 / 再計画 / 拒否）が行われることを検証する |
| セキュリティテスト | 権限境界 | **ATH-TEST-019** (MUST) 許可外 Tool、Workspace 外アクセス、権限超過 Skill、未認証 API が拒否されることを検証する |
| 回帰テスト | Skill 版固定 | **ATH-TEST-020** (MUST) Skill 更新が実行中 Instance の Skill Set に影響しないことを検証する |

- **ATH-TEST-024** (MUST) Core Agent Instance の障害・再起動・エスカレーション（第 7.5.3 節）を
  検証しなければならない。再起動上限到達時に Core Host が判断を代行しないことを含む。
- **ATH-TEST-025** (MUST) Core Agent からの特権操作要求が Core Host の検査を経ることを検証しなければ
  ならない（権限外要求が拒否されること）。
- **ATH-TEST-021** (MUST) 擬似 Runtime（決定論的に応答する Runtime 実装）をテスト基盤として提供
  しなければならない。
- **ATH-TEST-022** (MUST) LLM 呼び出しを含むテストは、応答を固定化できる仕組みを用いなければ
  ならない。外部 LLM の非決定性に依存した合否判定を CI に組み込んではならない。
- **ATH-TEST-023** (SHOULD) ライブラリ crate の単体テストカバレッジを 80% 以上とする。

---

### 35. ドキュメント生成規約

#### 35.1 @doc 指示

開発時に `@doc` が指示された場合、ATHENA のシステム概要ドキュメントを作成する。

```text
docs/
├── system-overview.md
├── system-overview-ja.md
└── diagrams/
    ├── system-architecture.drawio
    ├── system-architecture.png
    ├── agent-hierarchy.drawio
    ├── agent-hierarchy.png
    ├── execution-flow.drawio
    └── execution-flow.png
```

- **ATH-DOC-001** (MUST) `@doc` の成果物は Markdown（`.md`）としなければならない。
- **ATH-DOC-002** (MUST) 英語版と日本語版の両方を作成しなければならない。
- **ATH-DOC-003** (MUST) 日本語版は機械翻訳ではなく、構造を理解できる日本語技術文書としなければ
  ならない。
- **ATH-DOC-004** (MUST) システム概要の理解に必要な図を省略してはならない。規模が小さい場合、
  必要な図のみ作成してよい。

推奨構成：

```text
docs/
├── system-overview.md
├── system-overview-ja.md
├── architecture.md
├── architecture-ja.md
├── agents/
├── runtime/
└── diagrams/
```

#### 35.2 ドキュメント内容

システム概要ドキュメントは最低限以下を含む。

```text
 1. システム概要
 2. 解決する問題
 3. 設計思想
 4. システム全体構成
 5. ATHENA Core Agent
 6. Agent Hierarchy
 7. Agent間通信
 8. Runtime / agent-cli
 9. Tool
10. Memory
11. Skill
12. Human-in-the-Loop
13. 実行フロー
14. 状態・イベント
15. セキュリティ
16. 開発・拡張方針
```

- **ATH-DOC-005** (MUST) 上記項目を含めなければならない。日本語版も同一内容とする。
- **ATH-DOC-006** (MUST) 以下の関係が図と文章の両方から理解できなければならない。

```text
Human → ATHENA Core Agent → Director → Lead → Worker → Runtime → Tool / External System
（論理階層。プロセス構造は Runtime 配下の並列：第 6.2 節、ATH-HIER-010）
```

- **ATH-DOC-007** (MUST) `ATHENA Core Agent = AI Agent` であり `ATHENA Core ≠ 単純な Scheduler`
  であること、および `athena-core`（Core Host）は判断しないことを明記しなければならない。

#### 35.3 図の作成規約

図は draw.io（diagrams.net）で作成し、`.drawio`（編集マスター）と `.png`（表示用）の 2 種類を
保持する。

- **ATH-DOC-008** (MUST) 図は draw.io で編集可能な XML として作成しなければならない。
- **ATH-DOC-009** (MUST) `.drawio` を編集可能なマスターとして保存し、Markdown からは PNG を
  参照しなければならない。

```markdown
## システムアーキテクチャ

![ATHENA System Architecture](./diagrams/system-architecture.png)
```

- **ATH-DOC-010** (MUST NOT) テーブルを表す図形に rounded corner を使用してはならない
  （`rounded=0`）。
- **ATH-DOC-011** (MUST) 丸角図形を使用する場合は `rounded=1` と `arcSize=2` を併記しなければ
  ならない。`arcSize` の省略、および過大な値を使用してはならない。

```text
style="rounded=1;arcSize=2;whiteSpace=wrap;html=1;"
```

- **ATH-DOC-012** (MUST) すべての文字（タイトル、ノード名、ラベル、補足説明、注釈、凡例、
  テーブル内文字、矢印ラベル）のフォントサイズを 12pt 以上としなければならない。
- **ATH-DOC-013** (MUST NOT) 補足テキストのみを 12pt 未満に縮小してはならない。
- **ATH-DOC-014** (SHOULD) 可読性のため 14pt 以上を使用する。
- **ATH-DOC-015** (MUST NOT) `shape=table` / `tableRow` / `partialRectangle` のネスト構造を
  使用してはならない。
- **ATH-DOC-016** (MUST) テーブルは各セルを独立した `mxCell` として絶対座標で配置しなければ
  ならない。

```xml
<mxCell id="cell-1"
        value="項目"
        style="rounded=0;whiteSpace=wrap;html=1;fontSize=12;"
        vertex="1"
        parent="1">
  <mxGeometry x="100" y="100" width="180" height="40" as="geometry"/>
</mxCell>

<mxCell id="cell-2"
        value="内容"
        style="rounded=0;whiteSpace=wrap;html=1;fontSize=12;"
        vertex="1"
        parent="1">
  <mxGeometry x="280" y="100" width="300" height="40" as="geometry"/>
</mxCell>
```

- **ATH-DOC-017** (MUST) 各セルは `x` / `y` / `width` / `height` を明示しなければならない。
- **ATH-DOC-018** (MUST) 図はブラウザ上で Markdown を表示した状態で可読でなければならない。
  フォントサイズ、ノード間隔、矢印の交差、テキストの重なりを確認する。
- **ATH-DOC-019** (MUST NOT) 図を過度に縮小して情報を詰め込んではならない。複雑な構成は
  `system-architecture` / `agent-hierarchy` / `execution-flow` / `runtime-architecture` など
  複数の図へ分割する。
- **ATH-DOC-020** (MUST) `.drawio` を変更した場合、PNG を再生成し Markdown 表示を確認しなければ
  ならない。`.drawio` のみ更新して PNG を古いまま残してはならない。

```text
drawio 変更 → PNG 再生成 → Markdown 確認
```

#### 35.4 実装との整合性

- **ATH-DOC-021** (MUST) 実装変更により Architecture / Agent Hierarchy / Runtime / API / Tool /
  Skill / Memory / Security / Execution Flow のいずれかが変化した場合、関連ドキュメントを
  更新しなければならない。
- **ATH-DOC-022** (MUST) 本仕様書の変更と `docs/` の更新を同一の変更単位で行わなければならない。

---

### 36. aiprj 運用規約

#### 36.1 適用範囲

ATHENA で実行されるすべての Agent は `aquaxis/aiprj` を利用する。対象には Core Agent、Director、
Lead、Worker、Research / Coding / Verification / Documentation Agent、動的生成された Agent を
含む。

```text
Agent Workspace（= cwd）              .athena/var/instances/<instance_id>/
    |
    +-- .aiprj/                        aiprj が読む。書き込み領域のみ（ATH-AIPRJ-027）
    |     +-- AI_PRJ_TASKS.md
    |     +-- AI_LOG/
    |
    +-- .agent-cli/                    agent-cli が読む入力のみ（ATH-ACLI-014）
    |     +-- config.toml
    |     +-- commands/
    |     +-- persona.md
    |
    +-- skills/                        ATHENA の素材。agent-cli は読まない（ATH-ACLI-015）

読み取り（絶対パス：ATH-AIPRJ-028）
    <PROJECT_ROOT>/.aiprj/instructions.md
    <PROJECT_ROOT>/.aiprj/AI_PRJ_REQUIREMENTS.md
    <PROJECT_ROOT>/.aiprj/AI_PRJ_DESIGN.md
    <PROJECT_ROOT>/.aiprj/rules/

生成の原本（直接は参照されない：ATH-ACLI-013）
    <ATHENA_HOME>/var/agent-cli-template/
```

上図の `Agent Workspace` は Agent Instance ごとのディレクトリ
（`<ATHENA_HOME>/var/instances/<instance_id>/`、第 4.3.4 節）であり、`agent-cli` の cwd となる
（ATH-OPS-009）。`.aiprj/` の**書き込み対象**は cwd 相対で解決されるため、ここに置く。規約と
3 文書、`agent-cli` の設定とコマンド定義は共有し、絶対パスで参照する（ATH-AIPRJ-027、
ATH-AIPRJ-028、ATH-ACLI-012）。作業対象（`<PROJECT_ROOT>` の source / artifacts）も cwd の外に
あり、絶対パスで与える（ATH-OPS-010）。

- **ATH-AIPRJ-001** (MUST) すべての Agent は `aiprj` の作業規約に従って作業しなければならない。
- **ATH-AIPRJ-002** (MUST) Agent Instance ごとに `aiprj` 実行コンテキストを持たなければならない。
- **ATH-AIPRJ-003** (MAY) 複数 Agent が同一ソースツリーを共同利用する場合、共有プロジェクトの
  `aiprj` を共通の作業規約として利用してよい。その場合も Instance の実行状態、ログ、Message、
  成果物の帰属は ATHENA 側で管理しなければならない。プロジェクト単位の構成（第 4.3.4 節）では
  これが通常の形態であり、配置は ATH-AIPRJ-024 に従う。共有するのは作業規約と 3 文書の
  **読み取り**であり、管理領域は Instance ごとに分離する（ATH-AIPRJ-025）。

#### 36.2 起動時の初期化

- **ATH-AIPRJ-004** (MUST) Runtime は Agent 起動前に Workspace（当該 Instance の cwd、
  ATH-OPS-009）の `aiprj` 対応状態を確認しなければならない。
- **ATH-AIPRJ-005** (MUST) 未初期化の場合に限り初期化を実行しなければならない。`install.sh`
  の展開は**プロジェクトにつき 2 箇所に限る**（`<PROJECT_ROOT>` と
  `<ATHENA_HOME>/var/agent-cli-template/`）。Agent Instance ごとに展開してはならない。
  Instance の `.agent-cli/` は原本からの生成とする（ATH-AIPRJ-006、ATH-AIPRJ-027、
  ATH-ACLI-013）。
- **ATH-AIPRJ-027** (MUST) Agent Instance の Workspace の初期化は、**書き込み領域の生成のみ**と
  しなければならない。生成するのは `.aiprj/AI_PRJ_TASKS.md`（空または担当範囲のひな形）と
  `.aiprj/AI_LOG/` に限る。`instructions.md`、`rules/`、要求・設計文書を Instance へ複製しては
  ならない。これらはプロジェクトの `<PROJECT_ROOT>/.aiprj/` を正とし、読み取りのみとする
  （ATH-AIPRJ-024、ATH-AIPRJ-025）。
- **ATH-AIPRJ-028** (MUST) Instance が参照する `aiprj` の規約と 3 文書は、`commands/` 配下の
  **手順ファイル**の中で `<PROJECT_ROOT>/.aiprj/` への**絶対パス**として記述しなければならない。
  書き込み対象（`AI_PRJ_TASKS.md`、`AI_LOG/`）は cwd 相対で記述し、当該 Instance の領域へ
  解決させる。この書き分けによって、手順ファイルの原本を Instance ごとに書き換えず複製できる
  （ATH-ACLI-013）。
  手順ファイルは slash command として展開されない（第 2.2.3 節）。したがって Persona 本文および
  委任の `instruction` は、当該手順ファイルを**絶対パスで明示的に指示**しなければならない
  （ATH-ACLI-017）。指示を伴わない手順ファイルの配置のみで規約が適用されることを前提として
  はならない。
- **ATH-AIPRJ-029** (MUST) Instance の `aiprj` 管理領域は、`events.retention_days` の経過後に
  削除しなければならない（ATH-AIPRJ-026）。Instance は PlanStep ごとかつ試行ごとに生成される
  ため（ATH-SCHED-009、ATH-FAIL-007）、削除を行わないと領域が単調増加する。削除は
  `MemoryEvicted` ではなく Event 保持期間の掃除処理として実施し、実施を記録する。

```bash
curl -fsSL https://raw.githubusercontent.com/aquaxis/aiprj/main/install.sh | sh -s -- <directory>
```

手動セットアップが必要な環境では、リポジトリを取得して `install.sh` を利用してよい。

```bash
git clone https://github.com/aquaxis/aiprj.git
cd aiprj
./install.sh <target_directory>
```

- **ATH-AIPRJ-006** (MUST NOT) Agent 実行のたびに無条件で再インストールしてはならない。
- **ATH-AIPRJ-007** (MUST NOT) 既存の `instructions.md` を破棄・置換してはならない。既存の
  プロジェクト方針を尊重する。
- **ATH-AIPRJ-008** (MUST) 初期化の実施を `WorkspaceInitialized` Event として記録しなければ
  ならない（第 23.2 節）。

#### 36.3 Agent の実行モデル

```text
Agent 起動
   ↓
aiprj 環境確認
   ↓
instructions.md 読み込み
   ↓
要求・設計・タスクの整合性確認
   ↓
タスク実行
   ↓
AI_PRJ_TASKS.md 更新
   ↓
成果物検証
   ↓
AI_LOG 保存
   ↓
Agent 結果を ATHENA Core へ報告
```

- **ATH-AIPRJ-009** (MUST) Agent はタスク状態の変化ごとに `AI_PRJ_TASKS.md` を更新しなければ
  ならない。
- **ATH-AIPRJ-010** (MUST) Agent は作業ログを `AI_LOG/` へ保存しなければならない。ファイル名は
  `YYYY-MM-DD_NNN.md` とし、既存ログを上書きしてはならない。
- **ATH-AIPRJ-011** (MUST) Runtime は `aiprj` の `/ai`、`/next_ai`、`/close_ai` 相当の運用モデルを
  Agent 実行モデルへ対応付けなければならない。対応付けは slash command の起動ではなく、
  `commands/` 配下の手順ファイルを Persona と `instruction` が絶対パスで指示する形で行う
  （第 2.2.3 節、ATH-ACLI-017、ATH-AIPRJ-028）。
- **ATH-AIPRJ-012** (MUST) 3 文書（要求・設計・タスク）間の不整合を検出した場合、Agent は作業を
  進める前に解消しなければならない。

#### 36.4 責務分離

| 機能 | aiprj | ATHENA Core |
|---|---|---|
| AI 作業規約 | ○ | ○ |
| 要求・設計・タスク文書 | ○ | 管理・統合 |
| 個別 Agent の作業実行 | ○ | 委任・監督 |
| Agent 選択 | — | ○ |
| Agent 階層構築 | — | ○ |
| Agent間通信 | — | ○ |
| Skill 選択・提供 | — | ○ |
| Runtime 選択 | — | ○ |
| 再計画 | — | ○ |
| 結果評価 | 補助 | ○ |
| Human-in-the-Loop | — | ○ |
| 作業ログ | ○ | ○（Event として別管理） |

> **aiprj は Agent の作業規約・プロジェクト管理基盤であり、ATHENA Core Agent は Agent 群全体を
> 理解・計画・委任・評価する Executive Agent である。**

```text
ATHENA Core    != aiprj
ATHENA Runtime != aiprj
agent-cli      != aiprj
```

#### 36.5 禁止事項

- **ATH-AIPRJ-013** (MUST NOT) Agent が `aiprj` の規約を無視して作業してはならない。
- **ATH-AIPRJ-014** (MUST NOT) `instructions.md` を無視して独自の作業方針へ変更してはならない。
- **ATH-AIPRJ-015** (MUST NOT) 3 文書の整合性を無視してはならない。
- **ATH-AIPRJ-016** (MUST NOT) タスク状態を更新せずに作業完了としてはならない。
- **ATH-AIPRJ-017** (MUST NOT) 作業ログを意図的に残さないことをしてはならない。
- **ATH-AIPRJ-018** (MUST NOT) Agent が他 Agent の `aiprj` 管理領域を無断で変更してはならない。
- **ATH-AIPRJ-019** (MUST NOT) ATHENA Core の判断機能を `aiprj` へ移してはならない
  （ATH-POLICY-010）。
- **ATH-AIPRJ-020** (MUST NOT) `aiprj` を ATHENA Runtime そのものとして扱ってはならない。

#### 36.6 階層各層での利用

| 階層 | aiprj における管理対象 |
|---|---|
| Core Agent | プロジェクト全体の目的、制約、計画、成果 |
| Director Agent | 担当領域の要求・設計・タスクの整理と Lead への委任 |
| Lead Agent | 専門領域のタスク管理と Worker への委任 |
| Worker Agent | 割り当てられた実装・調査・検証作業の実行 |

- **ATH-AIPRJ-021** (MUST) 階層のすべてのレベルで `aiprj` を適用し、最下層の Worker まで作業規約と
  ログ管理を統一しなければならない。
- **ATH-AIPRJ-022** (MUST) Core Agent の Workspace（既定 `<PROJECT_ROOT>/.athena/var/core-agent`）も `aiprj` 管理下と
  しなければならない。Core Host はその初期化状態を ATHENA 起動時に確認する（ATH-CORE-021）。
- **ATH-AIPRJ-023** (MUST NOT) Core Agent の `aiprj` 管理領域と下位 Agent の `aiprj` 管理領域を
  共有してはならない。
- **ATH-AIPRJ-024** (MUST) `aiprj` の配置は以下の 3 層としなければならない（第 4.3.4 節）。
  作業対象（source / artifacts）の共有と、`aiprj` 管理領域の分離を混同してはならない。

| 層 | 位置 | 書き込み主体 | 内容 |
|---|---|---|---|
| プロジェクト | `<PROJECT_ROOT>/.aiprj/` | 人間、および Core Agent（Result 集約後） | 作業規約、要求・設計・タスクの 3 文書 |
| Core Agent | `<PROJECT_ROOT>/.athena/var/core-agent/.aiprj/` | Core Agent | Core Agent 自身のタスク管理と AI_LOG |
| 下位 Agent Instance | `<PROJECT_ROOT>/.athena/var/instances/<instance_id>/.aiprj/` | 当該 Instance | その Instance の担当範囲のタスク管理と AI_LOG |

- **ATH-AIPRJ-025** (MUST NOT) 下位 Agent Instance は `<PROJECT_ROOT>/.aiprj/` へ書き込んでは
  ならない。読み取りのみとする。複数 Instance が同一の `AI_PRJ_TASKS.md` と `AI_LOG/` を
  並行更新すると、`AI_LOG` の連番（ATH-AIPRJ-010）が競合し、タスク状態の更新が失われる。
- **ATH-AIPRJ-026** (MUST) Instance の `aiprj` 管理領域は、Instance が終端状態へ達した後も
  保持しなければならない。`AI_LOG` は実行の記録であり、Event（第 23 章）および Result
  （第 19 章）から参照される（ATH-EVENT-016）。削除は Event の保持期間
  （`events.retention_days`）に合わせる。

---

### 37. 初期実装でやらないこと

MVP では以下を実装しない。

```text
独自 LLM の開発
Agent 推論エンジンそのものの開発
agent-cli の全面的な置き換え
Kubernetes 必須化
分散 DB
SaaS 化
Agent Marketplace
課金システム
Hub の必須化
SDK の必須化
過度に高度な自律計画
不要なマイクロサービス化
```

- **ATH-DEV-017** (MUST NOT) 上記を MVP の構成要素としてはならない。

まず以下を確実に動作させる。

```text
Task → Plan → Agent Selection → Skill Selection → Runtime → Agent → Result → Evaluation
→ Re-plan → Final Result
```

