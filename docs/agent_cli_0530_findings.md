# `agent-cli` 0.53.0 findings (the `ATH-DEV-022` re-verification, 2026-11-14)

| 項目 | 内容 |
|---|---|
| Basis | `ATH-DEV-022`'s verification had been performed against **0.29.0** (`docs/agent_cli_0290_findings.md`); the installed binary measured **0.53.0**, so the checklist was re-run. Probes ran under `/var/tmp` with a scratch `HOME`, never the operator's own config — 0.29.0's F-5 established that `agent-cli` rewrites a user-level file carrying legacy keys in place. |
| Main record | requirements §3.34.64; spec 改訂履歴 row dated 2026-11-14; the 0.29.0 record is kept as history (D-29) |
| Japanese version | `docs/agent_cli_0530_findings_ja.md`(同一内容) |

## 1. The 0.29.0 checklist, re-run

| Check | Observed on 0.53.0 |
|---|---|
| Config chain | **Unchanged.** `AGENT_CLI_CONFIG` → `~/.config/agent-cli/config.toml` overlaid by `./.agent-cli/config.toml` — stated in `--help` and exercised by every probe. The §2.2 premise stands |
| `serve` (foreground headless) | Starts and serves; **no panic**. A non-conformant persona is refused with a precise error rather than a hang (see F-1) |
| `spawn` (detached) | **The stdout contract is identical**: `spawned detached agent: id=<agent-id> name=<name> pid=<os pid> socket=<path>`. The launcher exits 0, the child outlives it, and the peer registers. D-18's capture of the peer agent-id from this line is safe on 0.53.0 |
| IPC `ask` / `list` / `stop` | Round trip, listing and graceful stop + deregistration all verified against live peers. **`ask`'s argument shape changed**: `agent-cli ask --persona <PERSONA> <PEER> <TEXT>` — the peer and text are positional and `--persona` is required |
| Context retention | **Holds.** A fact stated in one `ask` (`ZEBRA-7`) was recalled by a second, separate `ask` against the same resident peer |
| Procedure files over IPC | **Holds.** An absolute-path file read returned the file's content verbatim; `/update_ai` arrived as **literal text**, never expanded — `ATH-ACLI-016` / `ATH-ACLI-017` re-confirmed |
| Config keys | A file already carrying canonical spellings is **byte-identical** across an invocation (md5 checked), so the control-surface checksum (`ATH-CORE-044`) is safe. The legacy-rename *notice* still fires against a file with no legacy key — a spurious log line, **not** a rewrite |
| Long-lived residency | A peer spawned with no input was resident at 46 s; after processing two `ask`s it was still resident at **83 s** past its last input. `--help` states that `spawn` sets `--exit-when-idle` for the peers it launches by default; that termination was **not observed**. The long-lived-session premise of §2.2 / §7.5 / §14.4 holds — the figure is 83 s of observation, not a proof of indefinite residency |

## 2. Divergences found

| # | Divergence | Consequence and disposition |
|---|---|---|
| F-1 | **The persona front matter must carry `role`.** The 0.29.0 record required only that front matter exist; 0.53.0 refuses a persona whose front matter lacks `role` (`persona error: invalid YAML frontmatter: missing field role`), and a `spawn` of such a persona surfaces only as `detached agent did not register within 5s` at the launcher | **ATHENA conforms**: every shipped persona declares `name` / `role` / `level` / `allowed_tools`. This is why ATHENA's own spawn works while a minimal probe persona failed. Recorded so the next reader does not mistake the launcher's timeout message for a regression |
| F-2 | **`model` directly under `[provider]` is ignored.** With `[provider] kind = "ollama"` + `model = "glm-5.3:cloud"` — ATHENA's generated shape, verbatim — the peer ran on `glm-5.1:cloud`, the provider's built-in default. `[provider.<kind>] model` **is** honoured, and so is the `--model` flag | **An ATHENA defect, fixed in this change unit.** The generated overlay silently discarded the operator's stated model — the outcome `ATH-AGENT-008` exists to prevent, since ATHENA validates that a model is stated and then shipped a file where it had no effect. The overlay now writes `[provider.<kind>] model`, with the table key quoted when the provider name is not a bare TOML key (`llama.cpp`). Spec §2.2.1's `ATH-ACLI-012` mapping row is revised accordingly (`ATH-DEV-022`'s own instruction) |
| F-3 | **`ask`'s argument shape changed** (positional `<PEER> <TEXT>`, `--persona` required) | ATHENA does not shell out to `ask`; its transport addresses peers over IPC directly, so no ATHENA change follows. Recorded because the 0.29.0 record's probe commands no longer run as written |
| F-4 | `--help` claims `spawn` sets `--exit-when-idle` by default; a spawned peer was nonetheless resident 83 s after its last processed input | No ATHENA change. An `agent-cli`-side documentation/behaviour mismatch, recorded as an observation rather than relied upon in either direction: ATHENA's supervision already treats a departed instance #0 as a recovery target (`ATH-CORE-029`, X-62) |

## 3. The 0.29.0 follow-up items, re-measured

| Item | Result |
|---|---|
| F-4 (0.29.0) — first-spawn death on a cold boot | **Did not reproduce.** Four spawns with a conformant persona registered first-try, and ATHENA's own resident boots instance #0 first-try across sessions 067–072. The 0.29.0 criterion (a cold boot whose first spawn is resident without a restart) is met on this host; the quirk is not declared fixed upstream — it was not reproduced here |
| Registry hygiene after `kill -9` | **Improved.** 0.29.0 kept a stale entry in `list` until probed; on 0.53.0 the entry is **pruned** after `SIGKILL` (measured: listed count 0 five seconds after the kill). D-18's address-by-agent-id remains correct and costs nothing |
| ollama warm-up latency profile | **Not measured.** The probe `ask`s completed within their timeouts, but no first-load-vs-steady-state profile was taken, so no `startup_timeout` input is claimed from this session |

## 4. What this record does not claim

The NFR figures in `docs/nfr_compliance.md` were taken on 0.29.0 and **keep that version** —
none of them was re-measured here. This session re-ran the behavioural checklist
`ATH-DEV-022` names, not the performance measurements.
