# Persona generation as a candidate capability — the Phase 8 evaluation (P8-05)

| 項目 | 内容 |
|---|---|
| Status | Evaluated (Phase 8, P8-05) |
| Root requirements | `ATH-HIER-018` (v1 dynamic-definition scope), `ATH-HIER-008` (MAY dynamic generation) |
| Decision | **Persona prose generation stays out of v1** — the evaluation closes the deferred item with a recorded rationale, no spec change |
| Japanese version | `docs/persona_generation_ja.md`(同一内容) |

## 1. What was deferred

`ATH-HIER-018` limits v1 dynamic Agent-definition generation to **combinations of existing
personas**, capabilities, and subsets of existing `allowed_tools`. Persona *prose
generation* (composing a new persona body for a specialization the roster does not cover)
was explicitly deferred to Phase 8 for evaluation (`ATH-HIER-018`).

## 2. The evaluation

**Benefit.** When the required capability set has no covering persona, the hierarchy must
re-decompose or escalate (`ATH-AGENT-005`). Generated personas would widen the constructible
hierarchy and reduce escalations for long-tail specializations.

**Risks assessed against the implemented architecture:**

| Risk | Basis |
|---|---|
| Generated prose could contradict the precedence stack or carry its own authority claims | `ATH-SKILL-026`/`ATH-SKILL-027` scan skill text at composition; the same scan applies to skill *material*, and a generated persona body would need the identical write-authority gate (`Layer::writable_by` — personas are a human/system-writable layer). A generation path that writes persona bodies is a new write authority over the `AiprjRules`-adjacent control surface — the thing the control-surface isolation (`ATH-CORE-043`) exists to prevent |
| Generated prose confers disguised permissions | The permission-integrity audit (P4-04) checks wire schemas and declared facts, not free text; a publishing persona could instruct an agent to claim permissions the definition does not carry. Runtime facts would still guard execution, but the *persona* layer — read by the agent as standing duties — would carry un-audited text |
| Auditability | Personas are version-controlled files whose provenance is the repository history (`R-PER-05`). Generated prose assembled at run time has no such trail unless a dedicated generation record (requester, sources, checksum, approval) lands first |
| Trust basis | v1 trusts persona content because it is human-authored and reviewed. Generated content would move personas into the same trust class as Skill sources, where trust rests on validation (`ATH-SKILL-035`/`036`) — signing and registry distribution are deliberately not v1 requirements |

## 3. Preconditions for revisiting

Enabling persona generation would require, before any implementation:

1. A generation gate that runs the precedence composition check (P5-11's face) over the
   generated body **before acceptance**, refusing any nullification attempt.
2. A persona-generation record (requester, the source personas combined, checksum where
   the generated file is materialized) with the Events to audit it.
3. `pattern`-level policy configuration: which levels may request generation, approval for
   `high` (`ATH-SKILL-024`'s analogue), retained forever under `ATH-REGISTER`-style history.
4. A spec revision covering all four points — never working-code first (`ATH-DEV-007`).

## 4. Decision

The combination-based restriction of `ATH-HIER-018` remains the construction rule; the
dynamic-definition surface (`ATH-HIER-008`) stays bounded by existing personas. Persona
generation remains out of scope past Phase 8 until the preconditions in §3 are implemented
and the spec revised. No requirement ID was reused or vacated (`ATH-GEN-006`).
