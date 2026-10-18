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
rusqlite, schemars, ulid, sha2, thiserror — and compatibly licensed leaf crates). No
dependency is forked in-tree; every one is declared in `Cargo.toml` files, which the
CI dependency-boundary job parses (`ATH-DEV-011`). A license change for any intake
candidate is checked against MIT compatibility before the dependency is added.

## 3. Skills

No third-party Skill Package source has been ingested into `<ATHENA_HOME>/skills/` by the
development state of Phase 8. Any future intake follows `ATH-SKILL-035`/`036`: license
compatibility review first, publisher/source allow-list at registration, checksum verified
against the manifest, provenance appended to this record at intake time.
