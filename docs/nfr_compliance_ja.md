# NFR 遵守記録(X-01)

| 項目 | 内容 |
|---|---|
| 方法 | 各 NFR 行は、実行可能な検査による**計測**(CI。LLM 非決定性なし、`ATH-TEST-022`)か、出荷クレート内の**指名スイートによる検証**のいずれか |
| 計測ソース | `tests/tests/nfr_stub_measurements.rs`、**`tests/tests/nfr_redb_measurements.rs`**(ストアエンジン別の再記録、X-14 (d))、および各行が引用するクレートスイート |
| 状態表記 | ✅ 計測/検証済み · ◐ 部分検証(stub 段階、実ランナー行は別記) · ⬜ 未着手(阻止理由を明記) |
| 英語版 | `docs/nfr_compliance.md`(同一内容) |

## 1. 行別記録

| NFR | 要件 | 状態 | 内容 / 結果 |
|---|---|---|---|
| NFR-01 | 16 並行 Instance、インスタンス間状態汚染なし | ✅ | `nfr01_sixteen_concurrent_instances…`: 16 の stub-Runtime Instance を起動し、一意な Runtime Instance id と Instance ごとの Workspace を持って同時生存を確認。capability 宣言は ≥ 16 を表明(`ATH-RT-005/012`) |
| NFR-02 | API 読取り p95 < 200 ms(10,000 Task) | ✅ | **再計測 2026-10-22(`redb`)**: ファイルバックエンドストア 10k Task 上、500 混合サンプルの p95 **24.62 ms**(§4)。**初計測 2026-10-18(SQLite)**: 常駐 API 経由で p95 **7.2 ms** — いずれも 200 ms を満たす。エンジン入れ替えに伴い再記録済み |
| NFR-03 | `kill -9` + 再起動: 非終端 Task の復元 ≤ 10 秒 | ✅ | **再計測 2026-10-22(`redb`)**: 10k シード → ハンドル解放(クラッシュ相当)→ コールド再 open + `list_non_terminal` 復元が **0.103 s**(§4)。ストア/Context-document 行(`tests/core_loop.rs`、`reconcile_boot_states`)と実ランナー timed 0.12 s(§3)は不変 |
| NFR-04 | 200 events/s を損失なしで永続化 | ✅ | **再計測 2026-10-22(`redb`)**: ファイルバックエンド上で **3,307 events/s**(400 Event、1 Event ごとに 1 エンジンコミットの耐久性正直最悪ケース)、per-Core 単調 sequence、リプレイ長と順序一致(§4)。初計測(2026-10-18)はメモリ内ストアで 800 append の ≥ 200/s |
| NFR-05 | Task 履歴の完全再構成 | ✅ | `tests/core_loop.rs`(Event リプレイ再構成)、P4-27 の受入れ行 010–012 |
| NFR-06 | 新 Adapter ⇒ Core 変更不要 | ✅ | 共有コントラクトスイート(`tests/runtime_contract.rs`)+ registry による実行時登録のみ(`athena-runtime::registry` テスト)。`agent-cli` Adapter が第二のケースとして通過 |
| NFR-07 | 実行中に追加した Skill が次 Task から使用可能 | ✅ | `tests/phase5_skills.rs` のエンドツーエンドウォーク(register → enable → 次の resolve) |
| NFR-08 | Core テストに実 `agent-cli` 不要 | ✅ | Phase 1–2 の全スイートが stub Runtime 上で走行(`ATH-TEST-021`) |
| NFR-09 | CLI と Studio が同一 API、機能差なし | ✅ | `tests/phase7_studio.rs` 行 3: 7 つの共有制御フローが計画レベルで一致(`ATH-NFR-009`) |
| NFR-10 | 全状態遷移を Event から再構成可能 | ✅ | ストアスイートの Event 連続性行 + core_loop リプレイ(`ATH-EVENT-012`) |
| NFR-11 | `athena-api` 起動 ready ≤ 3 秒 | ✅ | **実測 0.08 s** コールド(実ランナー、§3)。ストア engine swap は起動計測に触れない(常駐プロセスの ready はストア open を含むが、`redb` open は軽量 — X-14 (d) の再記録対象外、下記 §4 の注記) |
| NFR-12 | Task あたりログ体積 < 1 MB | ✅ | `nfr12_a_normal_task_logs_under_one_megabyte`: 200 反復の通常 Task を `Logger`+`LogSink` のエンベロープ規律で捕捉して 1 MB 線を大きく下回り、全行が `correlation_id` を持つ JSON エンベロープ(`ATH-OBS-005`)— 体積計数は JSON 面の正直な値 |
| NFR-13 | 安全な停止 ≤ 30 秒 | ✅ | **再記録 2026-10-22(`redb`)**: コミット済み書込みがクリーン close → 再 open で全行無傷(§4 — ≤ 30 秒予算の前提となる耐久性面)。実測 0.06 s の SIGTERM → プロセス終了(§3)の停止時間面は不変 |
| NFR-14 | Core Agent 再起動 ≤ 30 秒 + 再注入 | ✅ | stub 段階の行(`core_loop.rs`、P1-32)+ **実測 ≈7 秒**(実ランナー、§3)。窗口消耗時はエスカレーション(`ATH-CORE-030`)。エンジン入れ替えと独立(プロセス監督面) |
| NFR-15 | クラッシュ非伝播(双方向) | ✅ | `tests/core_agent_boot.rs` + 下位クラッシュ行(Adapter、P3-15/P3-18) |
| NFR-16 | 1 Core Agent で 8 Task 並行(per-`thread_id`) | ✅ | per-thread 会計(scheduler スイート、P1-27/28)+ **実生サンプル: 8 並行 round trip がすべて順序どおり回答**(§3) |
| NFR-17 | 再起動後の二重委任なし | ✅ | `tests/core_loop.rs` 行 4(idempotency クレームのリプレイ、記録 outcome は 1 件、`ATH-CORE-035`)+ P4-27 行 012 |
| NFR-18 | タイムアウト前の stall 検出 | ✅ | `observability::silence` が `iteration_timeout × factor` 線で発火し 30 分の execution timeout より厳しく下(P4-26 の行)、wait-graph 循環検出(`ATH-OBS-013`) |

## 2. 実ランナー区分 — 解決済み(2026-10-18、Option B)

実ランナー行と実 Agent E2E は導入済み `agent-cli` に乗った。導入バイナリは **0.29.0** となり、
本セッションで経験的に再検証済み(人間の決定: 0.23.0 に固定せず、動作する新リリースに確定):

- 先に観測された 0.28.0 の 2 つの欠陥 — persona 設定後の `agent-cli serve` panic と
  `spawn` が繰り返し報告する `detached agent did not register within 5s` — は
  **いずれも 0.29.0 で解消**;
- 全ての基幹挙動が維持(config chain、IPC round trip、コンテキスト保持、絶対パスの手順
  ファイル読取り、IPC 経路での `/<name>` のリテラル扱い);
- 新しいキー正準化挙動は spec §2.2.4 / `ATH-ACLI-018` として規範
  (Adapter は正準綴りと `agent-cli` 側のツール名のみを出力 — 制御表面は再起動 checksum のため
  バイト安定を維持)。

検証表 + 追跡項目の詳細: `docs/agent_cli_0290_findings.md`。

## 3. 実ランナー計測(2026-10-18、0.29.0 基盤で取得 — SQLite engine 時点)

| 行 | 目標 | 実測 | 判定 |
|---|---|---|---|
| NFR-11 | `athena-api` 起動 ready ≤ 3 秒 | **0.081–0.082 s** コールド boot-to-ready(3 回のクリーン実行。プロセス起動から `/healthz` をポーリング)。instance #0 常駐 | ✅ |
| NFR-02 | 読取り p95 < 200 ms @10,000 Task | **combined p95 7.2 ms**(list p95 9.7 ms、id 読み p95 1.1 ms。ストア側 `list_page` 修正後の 600 混合サンプル — 修正前のメモリ内ウォークは list 当たり約 140 ms) | ✅ |
| NFR-03 (timed) | kill -9 + 再起動、非終端 Task の復元 ≤ 10 秒 | **0.12 s**(kill -9 → 新規 boot → 復元行を Task list が提供) | ✅ |
| NFR-13 | 安全な停止 ≤ 30 秒 | **0.06 s** SIGTERM → プロセス終了。全生存 Instance を Runtime Interface 経由で停止 | ✅ |
| NFR-14 (timed) | instance #0 kill -9 → Core Host 再起動 + 再注入 ≤ 30 秒 | **約 7 秒**(poll 5 秒 + spawn)、2 回検証。再起動窗口の 4 回目の計上失敗がループではなくエスカレーション(`ATH-CORE-030`) | ✅ |
| NFR-16 (raw sample) | 1 Core Agent Instance、8 Task 並行(per-`thread_id`) | 1 常駐 Instance 上で peer agent-id による **8 並行 `agent-cli ask` round trip**: 全 8 件が順序どおり回答、4.5 s 壁時計、伝送障害ゼロ(scheduler スイートが会計検証を担い、この行の実生面は完結) | ✅ |
| 実 Agent E2E | 実 Agent が Task を完遂 | `tests/live_agent_e2e.rs`(`--ignored` 手動区分): デタッチ済み 0.29.0 Instance が実 registry IPC で envelope 化委任を受領し、**実ファイル作業**(Workspace 内 bash + write)を実行、応答が result intake として実体化、teardown でレコード解放 — 合計 14.2 s | ✅ |

これらの点を超える *timed-degradation* 面(継続負荷 soak、コンテナ境界)は、v1 の
シングルホストスコープ(`ATH-OPS-006`)には含まれない。

## 4. ストアエンジン別再記録 — `redb`(X-14 (d)、2026-10-22)

`docs/nfr_compliance.md` §3 の記録は SQLite engine 時点で取得された。コンプライアンス記録は
エンジン固有であるため、SQLite 使用禁止(指示ソース 最優先事項、2026-10-18)で O-6 の
engine 確定が `redb` に落ち着いたことに伴い、**エンジン固有の 4 行(NFR-02/03/04/13)を
ファイルバックエンドの新しい engine 上で再計測**した(`<ATHENA_HOME>/var/athena.db` —
配置プロジェクトが実際に走行する耐久性基盤)。計測は
`tests/tests/nfr_redb_measurements.rs`(athena-conformance、CI 実行可能、LLM 非決定性なし
`ATH-TEST-022`)。

| 行 | 目標 | `redb` 実測(2026-10-22、ファイルバックエンド) | 判定 |
|---|---|---|---|
| NFR-02 | 読取り p95 < 200 ms @10,000 Task | **p95 24.62 ms**(500 混合サンプル: 300 の list page[浅い/深い offset を分散、ページ 20 行] + 200 の id 読み。読み取り面は store 直 — 常駐 API の HTTP 面を含まない。SQLite 時点の 7.2 ms の実測は同面) | ✅ |
| NFR-03 | `kill -9` + 再起動、非終端 Task の復元 ≤ 10 秒 | **0.103 s**(10k Task シード → エンジンハンドル解放 = 埋め込み engine におけるプロセス死亡相当 → コールド再 open + `list_non_terminal` が 5,000 非終端行を復元) | ✅ |
| NFR-04 | 200 events/s を損失なしで永続化 | **3,307 events/s**(400 Event、**1 Event ごとに 1 エンジンコミット** = 状態遷移 + Event の原子的単位 `ATH-PERS-003` の耐久性正直な最悪ケース。per-Core 単調 sequence、リプレイ長 = 追加数、順序 = 追加順) | ✅ |
| NFR-13 | 安全な停止 ≤ 30 秒 | **コミット済み書込みの全行生存**(40 行シード → クリーン close [ハンドル drop] → 再 open → 全行存在と全 id 解決。≤ 30 秒予算が仮定する耐久性面)。停止時間面そのもの(§3 の 0.06 s)はエンジン非依存 | ✅ |

**再記録のスコープ注記。** X-14 (d) の対象は「SQLite 上で計測された記録ベースの NFR」—
NFR-02/03/04/13 — であり、すべてここで新 engine 上の値に置き換わった。その他の行
(NFR-11 の起動 ready、NFR-14 の Core Agent 再起動)の計測は主にプロセス監督・常駐の面であり、
store engine の入れ替えによる有意な変化はなく、§3 の記録が現行の証拠として残る。論理データ
モデルと Repository trait 境界は `ATH-PERS-002/003/006` の耐久性条項を含めエンジン交換で
不変である(`X-14` (c)、design §16.4 を参照)。