# License provenance (P-lic record; `ATH-DEV-019`, X-07)

| 項目 | 内容 |
|---|---|
| License | MIT — `LICENSE` at the repository root (matches `aiprj` and `agent-cli`) |
| Scope | Every third-party material in the repository, before intake and at intake |
| Japanese version | `docs/license-provenance_ja.md`(同一内容) |

## 1. Vendored third-party material

| Material | Origin | Version | License | State at intake |
|---|---|---|---|---|
| `aiprj` expansion contents (instructions.md seed, `rules/`, the `.agent-cli/` project overlay, commands) | `aquaxis/aiprj` (https://github.com/aquaxis/aiprj) | 1.1.0 | MIT | Files unmodified except the deliberate read-directive rewrites (`ATH-AIPRJ-028`'s absolute-path form); provenance recorded in `crates/athena-cli/src/template.rs`; `.claude/` and the upstream README are not part of ATHENA's expansion (`ATH-CLI-009`) |
| `aiprj` command procedure files (`ai.md` / `next_ai.md` / `close_ai.md`) | `aquaxis/aiprj` 1.1.0 | MIT | Unmodified; the runtime treats them as procedure files read by absolute path (`ATH-ACLI-017`), never slash commands (`ATH-ACLI-016`) |

## 2. Cargo dependencies

Workspace dependencies come from crates.io and keep their upstream licenses (the
MIT / Apache-2.0 dual set for the major infrastructure crates — tokio, axum, serde,
schemars, ulid, sha2, thiserror — and compatibly licensed leaf crates). No
dependency is forked in-tree; every one is declared in `Cargo.toml` files, which the
CI dependency-boundary job parses (`ATH-DEV-011`). A license change for any intake
candidate is checked against MIT compatibility before the dependency is added.

**Engine note (X-14, 2026-10-22).** The store engine is **`redb` 2.6.4**
(MIT OR Apache-2.0, https://github.com/cberner/redb) — MIT-compatible, so R-LIC-01's
discipline is satisfied. The earlier `rusqlite` dependency (the SQLite engine) is no
longer part of the workspace: it was removed from the workspace declaration (2026-10-22,
AI_LOG 2026-10-22_001) after the store's realization switched to `redb` (2026-10-18);
its licence obligation leaves with it. The `rusqlite` rows above were removed from this
record as part of the same change unit (`ATH-DEV-019`, R-DOC-05).