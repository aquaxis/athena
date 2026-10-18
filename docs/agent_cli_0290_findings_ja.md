# `agent-cli` 0.29.0 の検証記録(Option B の基盤、2026-10-18)

| 項目 | 内容 |
|---|---|
| 基盤 | 人間の判断: 0.23.0 に固定せず、動作する新しいリリースを検証して据える。インストール済みバイナリは `agent-cli` **0.29.0**。プローブは `/tmp` 配下でプローブ毎のオーバーレイ付きで実施。以下の記録はこの再検証の経験的事実である |
| 主記録 | requirements §9.5。仕様 改訂履歴の 2026-10-18 付の行 |
| 英語版 | `docs/agent_cli_0290_findings.md`(同一内容) |

## 1. 確認された挙動(負荷を担う行はすべて green)

| 検査 | 観測 |
|---|---|
| 設定チェーン | `AGENT_CLI_CONFIG` → `~/.config/agent-cli/config.toml` を `./.agent-cli/config.toml` がオーバーレイ — §2.2 の前提から不変 |
| `serve`(フォアグラウンドの headless) | 起動し、peer として登録し、常駐を維持。**panic なし**(0.28.0 の `serve` 失敗は 0.29.0 で解消) |
| `spawn`(非同期) | 窓内で登録。ランチャーは exit 0 で `id=<agent-id> name=<name> pid=<os pid> socket=<path>` を印字。非同期の子はランチャーより長生きする |
| IPC `send` / `ask` / `list` / `stop` | 生存 peer に対し、往復・一覧・graceful stop + 登録解除をすべて検証 |
| 文脈の保持 | 常駐 1 peer への 2 回の `ask` で、先に述べた事実を想起(`[history] enabled = true`) |
| IPC 経路の手順ファイル | 絶対パスのファイル読み取りが原文どおり成功。`/name` はリテラル文として到達し、決して展開されない(`ATH-ACLI-016` / `ATH-ACLI-017` を 0.29.0 で再確認) |
| 設定キー | legacy 綴りが**読み込み時に**正準化: `[tools.shell]` → `[tools.bash]`、`[permissions.defaultMode]` → `[permissions.default_mode]`。ユーザーレベルのファイルはその場で書き戻された。正準綴り済みのファイルは serve/ask/stop を通じてバイト同一のまま(md5 確認) |

## 2. Adapter と常駐バイナリの形を決めた所見

| # | 所見 | 着地 |
|---|---|---|
| F-1 | 重複する表示名と** prune されるまで残る stale な registry エントリ**により、*名前*による address は曖昧になる(残存する `live-worker` エントリが name-addressed の `ask` を壊した) | Adapter は spawn ランチャーの stdout から peer の **agent-id** を捕捉し、輸送はそれで address する(決定 D-18) |
| F-2 | `--persona` は**与えられた cwd から**解決される — `--dir` フラグは存在しない。旧 `--persona-file` フラグは消滅 | argv = `spawn --persona .agent-cli/persona.md --name <id>`。生成オーバーレイは `persona_file = ".agent-cli/persona.md"` を宣言(`commands_dir` と同じく cwd 相対) |
| F-3 | persona ファイルは **YAML front matter(`---`)で始まらなければならない** — 本文のみのファイルは persona 読み込みで失敗 | Adapter は参照された persona ファイル(R-PER-01 の実形)を start 時に読む。決定論的テストは slug のみの本文を維持 |
| F-4 | **初回 spawn 死のクセ**: 一部の新規 boot で、*最初の*非同期子が誕生直後に exit した(2 回観測。最初の spawn が死んだレーンでは監督が正常に再起動し、後続のレーンは初回からクリーンに boot)。監督 poll は死を検出し、決定論的復旧(§7.5.3)が 1 poll 間隔以内に instance #0 を常駐させる — 運用上は自己修復 — が、根因は未特定 | **次の P0 検証項目**として記録(§3) |
| F-5 | ユーザーレベル設定の legacy キーが毎回 rename 通知を発火させていた — ATHENA 自身の生成オーバーレイは(そして現在は)正準のみでなければならない(`ATH-ACLI-018`)。バイト安定性(md5)の事実が制御表面チェックサム(`ATH-CORE-044`)を守る | 仕様 §2.2.4 + Adapter 生成 |

## 3. 次の P0 検証チェックリスト(フォローアップ項目)

1. **F-4 の根因**: 初回 spawn 死を隔離して再現する(新規 home、コールド boot、登録後数秒での
   子 exit)。cwd オーバーレイの競合、祖先系列の env(`AGENT_CLI_ANCESTORS`)、provider の
   warm-up exit のいずれかを判別する。監督の復旧は本番でこれを覆い隠す。検証基準は
   **再起動なしで最初の spawn が常駐するコールド boot** である。
2. 長時間セッションでの重複名 registry の衛生: kill -9 後の 0.29.0 の pruning 挙動を確認
   (Phase 0 は 0.23.0 の pruning を観測。観測された 0.29.0 の `list` は probe まで stale
   エントリを保持した)。stale-id での stop の SIGTERM フォールバックに残る pid 再利用の
   リスクも確認(agent-cli 側の関心事。ATHENA 自身の状態は再試行を Verification で
   ゲートする)。
3. ollama provider の warm-up レイテンシの profile(初回モデルロード対定常状態)を、本番
   ホストの per-Runtime `startup_timeout` の入力として採番。

## 4. この基盤で取られた実ランナー計測

行単位の完全な記録: `docs/nfr_compliance_ja.md` — NFR-11 **0.08 秒**、NFR-02 p95
**7.2 ms** @10,000 Task、NFR-03 **0.12 秒**、NFR-13 **0.06 秒**、NFR-14 **約 7 秒**
(2 回。窓枯渇の escalation も観測、`ATH-CORE-030`)、NFR-16 の **8 並行往復**がすべて
順序通り応答、live-Agent E2E(`tests/live_agent_e2e.rs`)は実委任 → 実ファイル作業 →
結果取込 → teardown での解放を完遂。
