# `agent-cli` 0.29.0 findings (Option-B basis, 2026-10-18)

| 項目 | 内容 |
|---|---|
| Basis | Human decision: do not pin 0.23.0; verify and settle on a working newer release. Installed binary: `agent-cli` **0.29.0**. Probes ran under `/tmp` with per-probe overlays; the records below are the empirical facts of this re-verification. |
| Main record | requirements §9.5; spec 改訂履歴 rows dated 2026-10-18 |
| Japanese version | `docs/agent_cli_0290_findings_ja.md`(同一内容) |

## 1. Confirmed behaviours (all load-bearing rows green)

| Check | Observed |
|---|---|
| Config chain | `AGENT_CLI_CONFIG` → `~/.config/agent-cli/config.toml` overlaid by `./.agent-cli/config.toml` — unchanged from the §2.2 premise |
| `serve` (foreground headless) | Starts, registers as a peer, stays resident; **no panic** (the 0.28.0 `serve` failure is resolved in 0.29.0) |
| `spawn` (detached) | Registers within its window; the launcher exits 0 and prints `id=<agent-id> name=<name> pid=<os pid> socket=<path>`; the detached child outlives the launcher |
| IPC `send` / `ask` / `list` / `stop` | Round trip, listing, graceful stop + deregistration all verified against a live peer |
| Context retention | Two `ask`s over one resident peer: the earlier-stated fact was recalled (`[history] enabled = true`) |
| Procedure files over IPC | An absolute-path file read worked verbatim; `/name` arrived as literal text — never expanded (`ATH-ACLI-016` / `ATH-ACLI-017` re-confirmed on 0.29.0) |
| Config keys | Legacy spellings are canonicalized **at load**: `[tools.shell]` → `[tools.bash]`, `[permissions.defaultMode]` → `[permissions.default_mode]`; the user-level file was rewritten in place; a file already carrying canonical spellings stayed byte-identical across serve/ask/stop (md5 checked) |

## 2. Findings that shaped the Adapter and the resident binary

| # | Finding | Landed as |
|---|---|---|
| F-1 | Duplicate display names + **stale registry entries linger until pruned** make *name*-addressing ambiguous (a lingering `live-worker` entry broke a name-addressed `ask`) | The Adapter captures the peer **agent-id** from the spawn launcher's stdout and addresses the transport by it (decision D-18) |
| F-2 | `--persona` resolves **from cwd as given** — no `--dir` flag exists; the old `--persona-file` flag is gone | argv = `spawn --persona .agent-cli/persona.md --name <id>`; the generated overlay states `persona_file = ".agent-cli/persona.md"` (cwd-relative like `commands_dir`) |
| F-3 | The persona file **must begin with YAML front matter** (`---`) — a body-only file fails at persona load | The Adapter reads the referenced persona file (R-PER-01's real shape) at start; the deterministic tests keep the bare-slug body |
| F-4 | **First-spawn death quirk**: on some fresh boots the *first* detached child exited shortly after birth (observed twice: a lane whose first spawn died and the supervision restarted successfully; later lanes booted cleanly first-try). The supervision poll detects the death and the deterministic recovery (§7.5.3) makes instance #0 resident within one poll interval — so operationally self-healing — but the root cause is untriaged | Recorded as the **next P0-verification item** (§3) |
| F-5 | The user-level config's legacy keys kept firing the rename notice on every invocation — ATHENA's own generated overlays must be (and now are) canonical-only (`ATH-ACLI-018`), with the byte-stability (md5) fact guarding the control-surface checksum (`ATH-CORE-044`) | Spec §2.2.4 + Adapter generation |

## 3. Next P0-verification checklist (the follow-up items)

1. **F-4 root cause**: reproduce the first-spawn death in isolation (fresh home, cold boot,
   child exit within seconds of registration); check whether it is the cwd-overlay race, the
   ancestor-lineage env (`AGENT_CLI_ANCESTORS`), or a provider-warmup exit. The supervision's
   recovery masks it in production; the verification criterion is *a cold boot whose first
   spawn is resident without a restart*.
2. Duplicate-name registry hygiene over long sessions: confirm 0.29.0's pruning behaviour after
   kill -9 (Phase 0 observed pruning for 0.23.0; the observed 0.29.0 `list` kept stale entries
   until probed) and the stop-by-stale-id SIGTERM fallback's residual recycled-pid risk (agent-cli
   side concern; ATHENA's own states gate retry on Verification regardless).
3. The ollama provider warm-up latency profile (first-model-load vs steady-state) as the
   per-Runtime `startup_timeout` input for production hosts.

## 4. Real-runner measurements taken on this basis

Full row-by-row record: `docs/nfr_compliance.md` — NFR-11 **0.08 s**, NFR-02 p95 **7.2 ms**
@10,000 Tasks, NFR-03 **0.12 s**, NFR-13 **0.06 s**, NFR-14 **≈7 s** (twice; window-exhaustion
escalation also observed, `ATH-CORE-030`), NFR-16's **8-parallel round trips** all answered in
order, and the live-Agent E2E (`tests/live_agent_e2e.rs`) completed a real delegation → real
file work → Result intake → teardown release.
