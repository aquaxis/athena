# NFR compliance record (X-01)

| 項目 | 内容 |
|---|---|
| Method | Every NFR row is either **measured by an executable check** (CI, no LLM non-determinism, `ATH-TEST-022`) or **verified by the named suite** in the shipped crates |
| Measurement source | `tests/tests/nfr_stub_measurements.rs` (athena-conformance) and the crate suites cited per row |
| Status convention | ✅ measured/verified · ◐ partially verified (stub-level; real-runner row pending) · ⬜ open with the blocking reason stated |
| Japanese version | `docs/nfr_compliance_ja.md`(同一内容) |

## 1. Row-by-row record

| NFR | Requirement | Status | Where / result |
|---|---|---|---|
| NFR-01 | 16 concurrent Instances, no cross-instance state contamination | ✅ | `nfr01_sixteen_concurrent_instances…`: 16 stub-Runtime instances started and all live simultaneously with unique Runtime Instance ids and per-instance Workspaces; capability declaration asserted ≥ 16 (`ATH-RT-005/012`) |
| NFR-02 | API reads p95 < 200 ms at 10,000 Tasks | ✅ | **Measured 2026-10-18** on the resident API over a 10k-Task store: combined p95 **7.2 ms** (list p95 9.7 ms / read-by-id p95 1.1 ms; §3 of this document) |
| NFR-03 | `kill -9` + restart: non-terminal Tasks restored ≤ 10 s | ✅ | Store/Context-document rows (`tests/core_loop.rs`, `reconcile_boot_states`) + **timed measure 0.12 s** on the real runner (§3) |
| NFR-04 | 200 events/s persisted without loss | ✅ | `nfr04_event_write_throughput…`: 800 appends, per-Core monotonic sequence, replay length and order identical (measured ≥ 200/s, in-memory store) |
| NFR-05 | Full Task-history reconstruction | ✅ | `tests/core_loop.rs` (Event replay reconstruction), P4-27's acceptance rows 010–012 |
| NFR-06 | New Adapter ⇒ no Core change | ✅ | the shared contract suite (`tests/runtime_contract.rs`) + registry-only registration (`athena-runtime::registry` tests); the `agent-cli` Adapter passes it as the second case |
| NFR-07 | Skill added while running usable from the next Task | ✅ | `tests/phase5_skills.rs` end-to-end walk (register → enable → next resolution) |
| NFR-08 | Core tests need no real `agent-cli` | ✅ | every Phase 1–2 suite runs on the stub Runtime (`ATH-TEST-021`) |
| NFR-09 | CLI and Studio same API, no feature gap | ✅ | `tests/phase7_studio.rs` row 3: seven shared control flows asserted plan-identical (`ATH-NFR-009`) |
| NFR-10 | Every transition reconstructible from Events | ✅ | Event continuity rows in the store suites + core_loop replay (`ATH-EVENT-012`) |
| NFR-11 | `athena-api` ready ≤ 3 s | ✅ | **Measured 0.08 s** cold on the real runner (§3) |
| NFR-12 | Log volume < 1 MB per Task | ✅ | `nfr12_a_normal_task_logs_under_one_megabyte`: a 200-iteration normal Task captured through the `Logger`+`LogSink` envelope discipline measures well under the 1 MB line, and every line is a JSON envelope carrying `correlation_id` (`ATH-OBS-005`) — so the volume count is the honest JSON-face number |
| NFR-13 | Safe shutdown ≤ 30 s | ✅ | **Measured 0.06 s** SIGTERM→exit with every live Instance stopped (§3) |
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

## 3. Real-runner measurements (taken 2026-10-18 on the 0.29.0 basis)

| Row | Target | Measured | Verdict |
|---|---|---|---|
| NFR-11 | `athena-api` ready ≤ 3 s | **0.081–0.082 s** cold boot-to-ready (three clean runs; `/healthz` polled from process launch), instance #0 resident | ✅ |
| NFR-02 | reads p95 < 200 ms @10,000 Tasks | **combined p95 7.2 ms** (list p95 9.7 ms, read-by-id p95 1.1 ms; 600 mixed samples after the store-side `list_page` fix — the in-memory slice walked ~140 ms/list before) | ✅ |
| NFR-03 (timed) | kill -9 + restart, non-terminal Tasks restored ≤ 10 s | **0.12 s** (kill -9 → fresh boot → Task list serving the restored rows) | ✅ |
| NFR-13 | safe shutdown ≤ 30 s | **0.06 s** SIGTERM → process exit, every live Instance stopped through the Runtime Interface | ✅ |
| NFR-14 (timed) | instance #0 kill -9 → Core Host restart + re-injection ≤ 30 s | **≈7 s** (poll interval 5 s + spawn), verified twice; the restart window's 4th counted failure escalated (`ATH-CORE-030`) rather than looping | ✅ |
| NFR-16 (raw sample) | one Core Agent Instance, 8 Tasks concurrent (per-`thread_id`) | **8 concurrent `agent-cli ask` round trips** by peer agent-id on one resident instance: all 8 answered in order, 4.5 s wall, zero transport faults (with the scheduler suites as the accounting verification, this row's live face closes) | ✅ |
| Live-Agent E2E | a real agent completes a Task | `tests/live_agent_e2e.rs` (`--ignored` manual lane): the detached 0.29.0 instance received the enveloped delegation over the real registry IPC, executed **real file work** (bash + write in its Workspace), the reply materialized as the result intake, and the record released at teardown — 14.2 s total | ✅ |

The remaining *timed-degradation* faces beyond these points (sustained load soak, container
boundaries) are not in the v1 single-host scope (`ATH-OPS-006`).
