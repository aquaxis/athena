# NFR compliance record (X-01)

| 項目 | 内容 |
|---|---|
| Method | Every NFR row is either **measured by an executable check** (CI, no LLM non-determinism, `ATH-TEST-022`) or **verified by the named suite** in the shipped crates |
| Measurement source | `tests/tests/nfr_stub_measurements.rs` (athena-conformance) and the crate suites cited per row; **engine-specific rows re-recorded in `tests/tests/nfr_redb_measurements.rs`** (X-14 (d)) |
| Status convention | ✅ measured/verified · ◐ partially verified (stub-level; real-runner row pending) · ⬜ open with the blocking reason stated |
| Japanese version | `docs/nfr_compliance_ja.md`(同一内容) |
| Store-engine note | The compliance record is engine-specific. The four store-bound rows (NFR-02/03/04/13) were measured on the SQLite engine at 2026-10-18 and **re-recorded on the operative `redb` engine at 2026-10-22** after the SQLite prohibition (instruction source 最優先事項， 2026-10-18) settled the engine end-choice (O-6) — see §4. NFR-11/13's process-level faces (boot, SIGTERM→exit) are engine-independent and stand |

## 1. Row-by-row record

| NFR | Requirement | Status | Where / result |
|---|---|---|---|
| NFR-01 | 16 concurrent Instances, no cross-instance state contamination | ✅ | `nfr01_sixteen_concurrent_instances…`: 16 stub-Runtime instances started and all live simultaneously with unique Runtime Instance ids and per-instance Workspaces; capability declaration asserted ≥ 16 (`ATH-RT-005/012`) |
| NFR-02 | API reads p95 < 200 ms at 10,000 Tasks | ✅ | **Re-recorded 2026-10-22 on the `redb` engine (file-backed)**: reads p95 **24.6 ms** @10k Tasks (§4). Earlier SQLite record: combined p95 7.2 ms (2026-10-18) |
| NFR-03 | `kill -9` + restart: non-terminal Tasks restored ≤ 10 s | ✅ | Store/Context-document rows (`tests/core_loop.rs`, `reconcile_boot_states`) + **re-recorded 2026-10-22 on the `redb` engine**: cold reopen + restore of 5,000 non-terminal Tasks **0.103 s** (§4). Earlier SQLite record: 0.12 s (2026-10-18) |
| NFR-04 | 200 events/s persisted without loss | ✅ | **Re-recorded 2026-10-22 on the `redb` engine (file-backed, one engine commit per Event)**: 3,307 events/s, no loss, replay order identical (§4). Earlier records: in-memory stub lane ≥ 200/s (`nfr04_event_write_throughput…`), SQLite lane |
| NFR-05 | Full Task-history reconstruction | ✅ | `tests/core_loop.rs` (Event replay reconstruction), P4-27's acceptance rows 010–012 |
| NFR-06 | New Adapter ⇒ no Core change | ✅ | the shared contract suite (`tests/runtime_contract.rs`) + registry-only registration (`athena-runtime::registry` tests); the `agent-cli` Adapter passes it as the second case |
| NFR-07 | Skill added while running usable from the next Task | ✅ | `tests/phase5_skills.rs` end-to-end walk (register → enable → next resolution) |
| NFR-08 | Core tests need no real `agent-cli` | ✅ | every Phase 1–2 suite runs on the stub Runtime (`ATH-TEST-021`) |
| NFR-09 | CLI and Studio same API, no feature gap | ✅ | `tests/phase7_studio.rs` row 3: seven shared control flows asserted plan-identical (`ATH-NFR-009`) |
| NFR-10 | Every transition reconstructible from Events | ✅ | Event continuity rows in the store suites + core_loop replay (`ATH-EVENT-012`) |
| NFR-11 | `athena-api` ready ≤ 3 s | ✅ | **Measured 0.08 s** cold on the real runner (§3) — a process-boot face, engine-independent |
| NFR-12 | Log volume < 1 MB per Task | ✅ | `nfr12_a_normal_task_logs_under_one_megabyte`: a 200-iteration normal Task captured through the `Logger`+`LogSink` envelope discipline measures well under the 1 MB line, and every line is a JSON envelope carrying `correlation_id` (`ATH-OBS-005`) — so the volume count is the honest JSON-face number |
| NFR-13 | Safe shutdown ≤ 30 s | ✅ | **Measured 0.06 s** SIGTERM→exit with every live Instance stopped (§3) + **re-recorded 2026-10-22 on the `redb` engine**: committed writes survive a clean close and reopen with every row intact (§4). The ≤ 30 s budget's durability assumption now holds on the operative engine |
| NFR-14 | Core Agent restart ≤ 30 s with context re-injection | ✅ | Stub-level rows (`core_loop.rs`, P1-32) + **timed ≈7 s** on the real runner; window exhaustion escalates (`ATH-CORE-030`) (§3) |
| NFR-15 | Crash non-propagation, both directions | ✅ | `tests/core_agent_boot.rs` + the adapter's subordinate crash rows (P3-15/P3-18) |
| NFR-16 | 8 Tasks concurrently on one Core Agent (per-`thread_id`) | ✅ | per-thread accounting (scheduler suites, P1-27/28) + **live raw sample: 8 concurrent round trips all answered in order** (§3) |
| NFR-17 | No double delegation after restart | ✅ | `tests/core_loop.rs` row 4 (idempotency claim replays, one recorded outcome, `ATH-CORE-035`) + P4-27 row 012 |
| NFR-18 | Stall detection before timeout | ✅ | `observability::silence` fires at the `iteration_timeout × factor` line, strictly below the 30-min execution timeout (P4-26's row), wait-cycle detection (`ATH-OBS-013`) |

## 2. Real-runner lane — resolved (2026-10-18, Option B)

The real-runner rows and the live-Agent E2E rode an installed `agent-cli`. The installed
binary is now **0.29.0** and was re-verified empirically this session as the Option-B basis
(the human decision: do not pin 0.23.0, settle on a working newer release):

- the two 0.28.0 defects observed earlier — `agent-cli serve` panicking after persona
  configuration and `spawn` repeatedly reporting `detached agent did not register within 5s` —
  are **both resolved in 0.29.0**;
- every load-bearing behaviour held again (config chain, IPC round trips, context retention,
  absolute-path procedure-file reads, literal `/<name>` over IPC);
- the new key-canonicalization behaviour is normative as spec §2.2.4 / `ATH-ACLI-018`
  (the Adapter emits canonical spellings and `agent-cli`-side tool names only — the control
  surface stays byte-stable for the restart checksum).

Full verification table + follow-up items: `docs/agent_cli_0290_findings.md`.

## 3. Real-runner measurements (taken 2026-10-18 on the 0.29.0 basis, SQLite engine)

| Row | Target | Measured | Verdict |
|---|---|---|---|
| NFR-11 | `athena-api` ready ≤ 3 s | **0.081–0.082 s** cold boot-to-ready (three clean runs; `/healthz` polled from process launch), instance #0 resident | ✅ |
| NFR-02 | reads p95 < 200 ms @10,000 Tasks | **combined p95 7.2 ms** (list p95 9.7 ms, read-by-id p95 1.1 ms; 600 mixed samples after the store-side `list_page` fix — the in-memory slice walked ~140 ms/list before) — *superseded by the §4 `redb` re-record* | ✅ |
| NFR-03 (timed) | kill -9 + restart, non-terminal Tasks restored ≤ 10 s | **0.12 s** (kill -9 → fresh boot → Task list serving the restored rows) — *superseded by the §4 re-record* | ✅ |
| NFR-13 | safe shutdown ≤ 30 s | **0.06 s** SIGTERM → process exit, every live Instance stopped through the Runtime Interface (the process face stands; the store durability face is re-recorded in §4) | ✅ |
| NFR-14 (timed) | instance #0 kill -9 → Core Host restart + re-injection ≤ 30 s | **≈7 s** (poll interval 5 s + spawn), verified twice; the restart window's 4th counted failure escalated (`ATH-CORE-030`) rather than looping | ✅ |
| NFR-16 (raw sample) | one Core Agent Instance, 8 Tasks concurrent (per-`thread_id`) | **8 concurrent `agent-cli ask` round trips** by peer agent-id on one resident instance: all 8 answered in order, 4.5 s wall, zero transport faults (with the scheduler suites as the accounting verification, this row's live face closes) | ✅ |
| Live-Agent E2E | a real agent completes a Task | `tests/live_agent_e2e.rs` (`--ignored` manual lane): the detached 0.29.0 instance received the enveloped delegation over the real registry IPC, executed **real file work** (bash + write in its Workspace), the reply materialized as the result intake, and the record released at teardown — 14.2 s total | ✅ |

The remaining *timed-degradation* faces beyond these points (sustained load soak, container
boundaries) are not in the v1 single-host scope (`ATH-OPS-006`).

## 4. Store-engine re-record (X-14 (d), taken 2026-10-22 on the `redb` engine)

The engine-specific rows re-measured on the **operative** engine — `redb` 2.6.4, file-backed
(durability-honest, the shape a deployed project runs at `<ATHENA_HOME>/var/athena.db`) — via
`tests/tests/nfr_redb_measurements.rs` (athena-conformance; deterministic CI, no LLM):

| Row | Target | Measured on `redb` (file-backed) | Verdict |
|---|---|---|---|
| NFR-02 | reads p95 < 200 ms @10,000 Tasks | **p95 24.6 ms** over 500 mixed samples (300 `list_page` pages across shallow/deep offsets + 200 reads by id; key-skip pagination — skipped rows are keys, never hydrated). The face is store-direct; the earlier SQLite record (7.2 ms) was the resident API's HTTP face, so the two lanes are not numerically comparable — both sit two orders under the 200 ms budget | ✅ |
| NFR-03 | non-terminal Tasks restored after a crash ≤ 10 s | **0.103 s** — cold reopen (the crash face for an embedded engine: the owning handle dropped, the file remains) + the `list_non_terminal` restore query over 10,000 seeded rows (5,000 non-terminal restored) | ✅ |
| NFR-04 | 200 events/s persisted without loss | **3,307 events/s** over 400 per-Event engine commits — one commit per Event is the durability-honest worst case (each is the transition+Event atomicity unit, `ATH-PERS-003`); per-Core monotonic sequence held, replay length/order identical | ✅ |
| NFR-13 | safe shutdown ≤ 30 s | **committed writes survive a clean close and reopen** — every row intact after drop + reopen (the durability face the ≤ 30 s shutdown budget assumes; NFR-11/13's process-level faces stand from §3, engine-independent) | ✅ |

Measurement basis: debug-profile `cargo test`, the same lane as the stub measurements (§1);
the absolute numbers are lane-relative (a release build is faster in the same direction), the
verdict is against each target. The NFR-02 gap vs the SQLite record (24.6 ms vs 7.2 ms)
stays two orders under the 200 ms budget — the redb key-skip pagination preserves the
deep-page latency shape the 2026-10-18 `list_page` fix delivered.