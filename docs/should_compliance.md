# SHOULD-requirement compliance record (X-06)

| 項目 | 内容 |
|---|---|
| Method | Every SHOULD-level requirement of the requirements document is audited: held, or recorded as a deviation with rationale (`X-06`'s rule — no silent deviation) |
| Status | Audited at the Phase-8 completion state (2026-10-18) |
| Japanese version | `docs/should_compliance_ja.md`(同一内容) |

## 1. Held

| Requirement | How it is held |
|---|---|
| `ATH-CORE-015` — investigation PlanStep when `unknowns` non-empty | Agent-side guidance; the producer protocol files carry the rule (`agents/producer/core-requests.md`); Core Host cannot force it (the choice is the agent's) — recorded as guidance-held |
| `ATH-AGENT-006` — score multiple candidates | **Implemented** in P8-01 (`athena-agent::scoring`, the §8.4 formula) |
| `R-AGT-06` — one Worker Instance per PlanStep | Held by construction: a v1 Instance holds exactly one PlanStep (`ATH-HIER-014`'s scoping; context hands over through Artifacts/Results and `depends_on`), asserted by the adapter/delegation-context tests |
| `R-CFG-09` — reload by restart; hot change = skill enablement only | Held: no configuration hot-reload path exists; the Registry's `refresh` is the only hot operation (`ATH-SKILL-008`) |
| `R-TST-10` — library-crate unit coverage ≥ 80 % | Measured `cargo llvm-cov --workspace`: total 83.8 % lines / 85.5 % functions; the library-crate aggregate (excluding the two bin applications' boot faces) is **89.3 %** — see X-03's record |
| NFR-11 / NFR-12 — startup ≤ 3 s, logs < 1 MB/Task | Not yet measured (resident-process rows, X-01's open set — no implementation conflicts them) |

## 2. Deviations, with rationale

### D-SHOULD-1 — token expiry (R-SEC-03's "SHOULD set token expiry")

**Deviation.** `athena-api::auth::TokenRegistry::issue` records no expiry; issued tokens
are static until revoked.

**Rationale.** v1 authentication is a *statically issued Bearer token* by requirement
(`R-SEC-04`): single user, single host, loopback default. Expiry without a renewal story
would dead-lock an operator mid-session (a Task's approval wait outlives a 1-hour token),
and the approval/operation trails persisting into long mutterings need a *stable
principal* to keep the audit lines joinable (`ATH-OBS-004`). The role-check location
(`athena-api::roles`) is the piece that must survive later auth upgrades (`ATH-SEC-027`),
and it is already isolated: adding expiry extends `authenticate`, never the per-endpoint
checks.

**Re-enable conditions.** With OIDC/OAuth2 work (`R-SEC-04`'s noted later addition):
`issue` gains `expires_at`, `load` refuses expired rows at boot, and `authenticate`
refuses an expired token with the 401 envelope and the audit Event — the spec section
covering authentication is revised in the same change unit.

### D-SHOULD-2 — stronger isolation for externally sourced skills (R-SEC-11)

**Deviation.** The shipped Runtime layer implements the process boundary only
(`IsolationBoundary::Process`); no container Runtime Adapter is shipped with v1.

**Rationale.** No externally sourced Skill has been ingested (`ATH-SKILL-036`'s v1
default denies every external fetch — empty allow-list), so the exposure the SHOULD
guards does not arise: every skill in the selection set is in-project and checksum- and
readonly-control-surface-constrained. The boundary *declaration* face is implemented and
enforceable now: `RuntimeCapabilities.isolation` is asserted against what the Runtime
actually provides (`ATH-SEC-015`), and `at_least_as_strong_as` gives the upgrade
comparison R-SEC-11 reasons about — a container Adapter would register its stronger
boundary and the harness gates (route selection for skills with external provenance)
have the fact they need without any Core change (`ATH-RT-007`).

**Re-enable conditions.** Before the first externally sourced Skill is admitted:
a container Runtime Adapter implementing the Runtime Interface and passing the shared
contract suite, with `skills.allowed_publishers`/`allowed_sources` populated, and the
route rule "external-skill agents only on `isolation ≥ Container`" wired at selection.
