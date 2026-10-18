# Core-request procedure (agents/producer/core-requests.md)

This is a procedure file the Core Agent reads by absolute path — never a slash command
(expansion is REPL-only, ATH-ACLI-016). The persona body and the delegation `instruction`
point at this file explicitly (ATH-AIPRJ-028's read/write split), and the Core Host
validates every emit against its JSON Schema (`ATH-CORE-039`).

## The twelve variants (ATH-CORE-033; no out-of-protocol path may exist)

```json
{ "kind": "submit_understanding", "task_id": "task-…", "understanding": { "goal": "…", "constraints": […], "completion_criteria": […], "unknowns": […], "risk_level": "…high|medium|low" } }
{ "kind": "submit_plan", "task_id": "task-…", "base_revision": ❮u64❯, "steps": [ { "order": ❮u64❯, "intent": "…", "depends_on_orders": […], "assigned_agent": "…?", "skill_set": [{"id": "…", "version": "1.0.0"}] } ] }
{ "kind": "compose_hierarchy", "task_id": "task-…", "nodes": [ { "agent_id": "…", "level": "producer|director|lead|worker", "superior_agent_id": "…?", "scope_ref": { "plan_step_id": "step-…"? } | {"task_id": "task-…"?}, "sibling_acl": […] } ] }
{ "kind": "delegate", "task_id": "task-…", "plan_step_id": "step-…", "agent_id": "…", "skill_set": [{"id": "…","version": "…"}], "instruction": "…", "idempotency_key": "step-…:0" }
{ "kind": "execute_tool", "task_id": "task-…", "tool": "fs_read|fs_write|search|send_to|…", "args": {…}, "idempotency_key": "step-…:0" }
{ "kind": "request_approval", "task_id": "task-…", "operation_ref": "…", "reason": "…", "impact": "…" }
{ "kind": "consume_approval", "approval_id": "apr-…", "operation_ref": "…" }
{ "kind": "ask_human", "task_id": "task-…", "question": "…", "options": ["…"]? }
{ "kind": "submit_evaluation", "task_id": "task-…", "result_id": "res-…", "verdict": "accept|retry|replan|fail|escalate", "rationale": "…" }
{ "kind": "transition_task", "task_id": "task-…", "to": "…", "expected_state": "…" }
{ "kind": "write_memory", "tier": "working|long", "content": "…" }
{ "kind": "complete_task", "task_id": "task-…", "summary": "…", "result_ids": ["res-…"] }
```

## Constraints (all normative)

1. Draft steps / nodes carry **no identifiers** — `PlanId`, `PlanStepId`, `InstanceId`,
   `ResultId`, `ApprovalId` are Core Host's (ATH-CORE-041); dependencies are referenced
   by `order` *within your own submission*.
2. `idempotency_key` is exactly `plan_step_id + ":" + attempt`; the claim lives in the
   store, so a repeat key is answered as a replay (ATH-CORE-035) — never re-run.
3. `transition_task` carries your honest `expected_state` (ATH-CORE-036); a stale value
   is `ValidationError`.
4. `base_revision` is the current revision of the target Plan; a stale value is refused
   (`PLAN_BASE_REVISION_CONFLICT`). Re-plan on top of the refusal's `current` value.
5. `reason` / `impact` are prose for the human's screen: risk classification is Core
   Host's (ATH-CORE-038) and never yours.
6. Every message body is the JSON alone — no prose alongside; no plaintext secrets in any
   field (ATH-EVENT-007, R-POL-13).
7. Re-emission after a schema failure cannot exceed 3 (ATH-CORE-040) — keep the emitted
   object's shape exact, and never bury the JSON in prose.
