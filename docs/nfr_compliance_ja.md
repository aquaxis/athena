# NFR 適合記録(X-01)

| 項目 | 内容 |
|---|---|
| 方法 | 各 NFR 行は、**実行可能な検査による計測**(CI。LLM の非決定性を含まない、`ATH-TEST-022`)または出荷クレート内の**指名スイートによる検証**のいずれかで裏付ける |
| 計測ソース | `tests/tests/nfr_stub_measurements.rs`(athena-conformance)および各行に引用したクレートスイート |
| 状態表記 | ✅ 計測/検証済み · ◐ 部分検証(擬似レベル。実ランナー行は未完) · ⬜ 未完(阻害理由を明記) |
| 英語版 | `docs/nfr_compliance.md`(同一内容) |

## 1. 行単位の記録

| NFR | 要件 | 状態 | 根拠 / 結果 |
|---|---|---|---|
| NFR-01 | 16 同時 Instance、Instance 間の状態混入なし | ✅ | `nfr01_sixteen_concurrent_instances…`: 擬似 Runtime の 16 Instance が同時生存し、Runtime Instance id は一意、Workspace も Instance 毎。capability 宣言は 16 以上を検証(`ATH-RT-005`/`012`) |
| NFR-02 | API 読み取り p95 < 200 ms @10,000 Task | ✅ | **2026-10-18 に計測**。常駐 API + 10k Task のストアで合算 p95 **7.2 ms**(一覧 p95 9.7 ms / by-id 読み取り p95 1.1 ms。本書 §3) |
| NFR-03 | `kill -9` + 再起動: 非終端 Task を 10 秒以内に復元 | ✅ | Store/Context-document の行(`tests/core_loop.rs`、`reconcile_boot_states`)+ 実ランナーでの**タイム計測 0.12 秒**(§3) |
| NFR-04 | 200 event/s を損失なしで永続 | ✅ | `nfr04_event_write_throughput…`: 800 件の追記、Core 毎の単調 sequence、再生長と順序が一致(200/s 以上を計測、メモリ内ストア) |
| NFR-05 | Task 履歴の完全再構成 | ✅ | `tests/core_loop.rs`(Event 再生による再構成)、P4-27 の受入行 010–012 |
| NFR-06 | 新 Adapter 追加時に Core 変更不要 | ✅ | 共有契約スイート(`tests/runtime_contract.rs`)+ registry 経由のみの登録(`athena-runtime::registry` テスト)。`agent-cli` Adapter が第 2 ケースとして通過 |
| NFR-07 | 稼働中に追加した Skill が次 Task から使用可能 | ✅ | `tests/phase5_skills.rs` のエンドツーエンド walk(register → enable → 次の解決) |
| NFR-08 | Core テストは実 `agent-cli` を要求しない | ✅ | Phase 1–2 の全スイートが擬似 Runtime 上で動作(`ATH-TEST-021`) |
| NFR-09 | CLI と Studio が同一 API で機能差なし | ✅ | `tests/phase7_studio.rs` 第 3 行: 7 つの共有制御フローで要求計画が同一であることを検証(`ATH-NFR-009`) |
| NFR-10 | 全遷移を Event から再構成可能 | ✅ | Store スイートの Event 連続性の行 + core_loop 再生(`ATH-EVENT-012`) |
| NFR-11 | `athena-api` 起動 ≤ 3 秒 | ✅ | 実ランナーでのコールド **0.08 秒**(§3) |
| NFR-12 | ログ量 < 1 MB/Task | ✅ | `nfr12_a_normal_task_logs_under_one_megabyte`: 200 反復の通常 Task を `Logger`+`LogSink` の封筒規律で捕集し 1 MB 線を十分下回ること、全行が `correlation_id` を持つ JSON 封筒であること(`ATH-OBS-005`)を確認 — 計測値は JSON 面の率直な数値 |
| NFR-13 | 安全な停止 ≤ 30 秒 | ✅ | SIGTERM → exit を **0.06 秒**で計測。生存中の全 Instance を Runtime Interface 経由で停止(§3) |
| NFR-14 | Core Agent 再起動 ≤ 30 秒 + 文脈再投入 | ✅ | 擬似レベルの行(`core_loop.rs`、P1-32)+ 実ランナーで**約 7 秒**。窓枯渇時は escalate(`ATH-CORE-030`)(§3) |
| NFR-15 | クラッシュ非伝播(双方向) | ✅ | `tests/core_agent_boot.rs` + Adapter の下位クラッシュ行(P3-15/P3-18) |
| NFR-16 | 1 Core Agent で 8 Task 同時(per-`thread_id`) | ✅ | per-thread の計上(scheduler スイート、P1-27/28)+ **実サンプル: 8 並行往復が全て順序通り応答**(§3) |
| NFR-17 | 再起動後の二重委任なし | ✅ | `tests/core_loop.rs` 第 4 行(idempotency claim の再生、記録成果は 1 件、`ATH-CORE-035`)+ P4-27 第 012 行 |
| NFR-18 | タイムアウト前の stall 検出 | ✅ | `observability::silence` が `iteration_timeout × factor` の線で発火し 30 分の実行タイムアウトより厳密に手前(P4-26 の行)、待機グラフ閉路検出(`ATH-OBS-013`) |

## 2. 実ランナー レーン — 解決(2026-10-18、Option B)

実ランナーの行と live-Agent E2E はインストール済み `agent-cli` 上で実施した。インストール
バイナリは **0.29.0** であり、本セッションで経験的に再検証の上、Option B の基盤として確定
した(人間の判断: 0.23.0 に固定せず、動作する新しいリリースに据える):

- 以前観測された 0.28.0 の 2 つの欠陥 — persona 設定後の `agent-cli serve` の panic と、
  `spawn` が繰り返し報告していた `detached agent did not register within 5s` — は
  **いずれも 0.29.0 で解消**;
- 負荷を担う挙動はすべて再確認(設定チェーン、IPC 往復、文脈保持、絶対パスの手順ファイル
  読み取り、IPC 経路での `/<name>` のリテラル扱い);
- 新たなキー正準化の挙動は仕様 §2.2.4 / `ATH-ACLI-018` として規範化(Adapter は正準綴りと
  `agent-cli` 側の Tool 名のみを出力 — 再起動時チェックサムのため制御表面はバイト安定を保つ)。

検証表の全体とフォローアップ項目: `docs/agent_cli_0290_findings_ja.md`。

## 3. 実ランナー計測(2026-10-18、0.29.0 基盤で実施)

| 行 | 目標 | 計測値 | 判定 |
|---|---|---|---|
| NFR-11 | `athena-api` 準備 ≤ 3 秒 | コールド boot-to-ready **0.081–0.082 秒**(3 回のクリーン実行。`/healthz` をプロセス起動からポーリング)、instance #0 常駐 | ✅ |
| NFR-02 | 読み取り p95 < 200 ms @10,000 Task | **合算 p95 7.2 ms**(一覧 p95 9.7 ms、by-id p95 1.1 ms。ストア側 `list_page` 修正後の 600 混合サンプル — 修正前のメモリ内スライスは一覧あたり約 140 ms を歩行) | ✅ |
| NFR-03(計時) | kill -9 + 再起動、非終端 Task の復元 ≤ 10 秒 | **0.12 秒**(kill -9 → 新規 boot → Task 一覧が復元行を返す) | ✅ |
| NFR-13 | 安全な停止 ≤ 30 秒 | **0.06 秒**。SIGTERM → プロセス終了、生存中の全 Instance を Runtime Interface 経由で停止 | ✅ |
| NFR-14(計時) | instance #0 の kill -9 → Core Host 再起動 + 再投入 ≤ 30 秒 | **約 7 秒**(poll 間隔 5 秒 + spawn)を 2 回検証。再起動窓の 4 回目の算入失敗はループせず escalate(`ATH-CORE-030`) | ✅ |
| NFR-16(生サンプル) | 1 Core Agent Instance、8 Task 同時(per-`thread_id`) | 常駐 1 インスタンスへの peer agent-id による **8 並行 `agent-cli ask` 往復**: 8 件すべてが順序通り応答、壁時間 4.5 秒、輸送障害ゼロ(scheduler スイートが計上検証を担うことで、本行の実運用の面を閉じる) | ✅ |
| Live-Agent E2E | 実 Agent による Task 完遂 | `tests/live_agent_e2e.rs`(`--ignored` 手動レーン): 非同期の 0.29.0 インスタンスが実 registry IPC 経由で封筒化された委任を受領、**実ファイル作業**を実行(Workspace 内の bash + write)、応答が結果取込として現れ、teardown で記録を解放 — 合計 14.2 秒 | ✅ |

これらの点を超える**計時劣化**の面(持続負荷の soak、コンテナ境界)は、v1 の単一ホストの
対象外である(`ATH-OPS-006`)。
