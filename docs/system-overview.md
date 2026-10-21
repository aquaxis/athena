# ATHENA System Overview

| Item | Value |
|---|---|
| Version | 1.0 (2026-10-18) |
| Source of truth | `spec/athena_spec_1.0.md` (Version 1.0) |
| Japanese version | `docs/system-overview_ja.md` (same content) |
| License | MIT |

> **Mandatory statement** (`ATH-DOC-007`): the ATHENA Core Agent **is an AI Agent** —
> ATHENA Core ≠ a simple scheduler; and `athena-core` (the Core Host) **does not make
> decisions**. It supervises, records state, and enforces policy only.

## 1. System overview

ATHENA is a system architecture that organizes multiple AI agents to carry out work
requested in natural language. A human gives one natural-language instruction; the Core
Agent — an AI agent itself, not a workflow engine — understands it, plans, delegates down
a hierarchy of Director / Lead / Worker agents, evaluates what comes back, and re-plans
when needed. Five execution units make this up: `athena-api` (the Core API resident
process), `athena-core` (the Core Host library), `athena-runtime-agent-cli` (the Runtime
Adapter), the `athena` CLI, and `athena-studio` (Rust → WebAssembly). All persisted state
lives in one authoritative store (`athena.db`, the pure-Rust embedded engine `redb`).

## 2. The problem it solves

Operating a single long-lived AI agent does not scale to real workloads: context floods,
one failed step stalls everything, high-risk operations run unguarded, and there is no
reconstructible record of who did what and why. ATHENA solves this by decomposing work
into a hierarchy of small, well-scoped units, keeping a durable, event-sourced record of
every state transition, isolating agents as processes with per-agent permission
boundaries, and requiring human approval for exactly the operations that need it.

## 3. Design philosophy

Four invariants govern every choice — violating one is a spec violation, not a trade-off:

1. **The centre is an AI Agent, not a scheduler.** Core Host supervises and records; it
   never decides.
2. **All thinking happens in Agent Processes on `agent-cli`**, including the Core Agent's
   (instance #0). Core Host holds no inference.
3. **Agent Processes are outside the trust boundary, Core Agent included.** Core Host is
   the single Policy Enforcement Point.
4. **The hierarchy is logical** — message routing plus permission containment, never
   process parentage; all Instances sit in parallel under the Runtime.

Prefer simplicity over distribution: one process hosts the API and Core Host in v1.

## 4. System architecture

![ATHENA System Architecture](./diagrams/system-architecture.png)

Requests enter only through the Core API (`/api/v1`); CLI and Studio are thin UIs over
the same surface. The Core Host drives Agent Processes exclusively through the Runtime
Interface, and every meaningful transition is persisted as an Event in the same
transaction as the state write.

## 5. ATHENA Core Agent

The Core Agent is the Producer-level AI agent that decides: understand → plan → decide →
act → observe → evaluate → re-plan. It runs as a dedicated, long-lived `agent-cli`
instance #0 (started with `spawn`, cwd `var/core-agent/`), session-scoped, never bound to
one Task, and communicating only through the Message Broker. Every privileged request it
makes (agent start, tool execution, state transition, skill enablement, approval
consumption) passes Core Host inspection as a typed `CoreRequest`; identifiers and plan
revisions are issued by Core Host alone. On its failure the recovery procedure is
deterministic — checksum check, context rebuild from Working Memory + Events, restart
with re-injection in a bounded sliding window; exhausting it escalates to a human and
Core Host never substitutes its own judgement.

## 6. Agent Hierarchy

![Delegation Chain](./diagrams/execution-flow.png)

Four levels with fixed work-unit mapping: Producer ↔ root Task, Director ↔ a group of
SubTasks, Lead ↔ one SubTask, Worker ↔ one PlanStep. Delegation targets the immediate
child level only (explicit Core-Agent-granted level skipping aside), depth defaults to 4,
`allowed_tools(child) ⊆ delegable_tools(parent)`, and same-level Instances run in
parallel by default. The hierarchy is a tree, unique parentage, acyclic; agents never
start agents — only Core Host creates Instances.

## 7. Inter-agent communication

All agent-to-agent traffic goes through the Message Broker inside Core Host; agents are
never wired to each other. Messages satisfy the §10.2 envelope, `thread_id` is issued by
Core Host (one thread per delegation), order is guaranteed within a thread, delivery is
at-least-once with Broker-side dedup, reply deadlines exist on `instruction`/`question`,
and sibling communication needs an explicitly recorded `sibling_acl` grant. `report` /
`complete` / `error` / control messages are never dropped; control messages (cancel /
interrupt) jump the queue.

## 8. Runtime / agent-cli

The Runtime Interface (`start` / `send` / `receive` / `status` / `stop`
/ `collect_result` / `capabilities`) is defined before any Adapter; Core must not branch
on the runtime kind — Adapters register into a registry, and every Adapter passes one
shared contract test suite. The `agent-cli` Adapter gives each Agent Instance its own
Workspace (`var/instances/<id>/`, cwd; Core Agent at `var/core-agent/`), generates its own
`.agent-cli/` from the template (complete `config.toml`, procedure-file `commands/`,
`persona.md`), initializes only the aiprj write areas, enforces startup/execution
timeouts and heartbeats, distinguishes graceful stops from kills, and reports abnormal
exits as `RuntimeError`. Procedure files are read by absolute path — never invoked as
slash commands.

## 9. Tool

Nine base tools (`fs_read`, `fs_write`, `shell`, `git`, `search`, `send_to`, `process`,
`terminal`, `network`) with fixed default risk levels; grants are per agent and deny by
default. Core Host's Tool Manager is the enforcement point: an out-of-grant request is a
`PermissionError`, `fs_read`/`fs_write` are confined to the path boundary (Workspace ∪
work targets, control surface excluded — exclusion wins over membership), high-risk
executions need approval first, and start/finish/failure land as Events plus
`tool_executions` rows.

## 10. Memory

Three tiers, separated in implementation: Short Term (in the Agent Process, token-counted
with the configured tokenizer), Working (per Task, until terminal state; holds the
restart-context set), Long Term (per Project, explicit writes only — Core Agent judgement
or human instruction, provenance carried, deletion Event-recorded, ceiling pressure
escalates rather than digesting). Eviction summarization is delegated to the agent named
by `memory.summarizer`; if unavailable, only references are kept. v1 keeps all tiers in
the same persistent store behind the Repository interface.

## 11. Skill

![Skill Pipeline](./diagrams/skill-pipeline.png)

A Skill Package carries `SKILL.md` (the body the agent reads) + `manifest.yaml`; the
pipeline is Validator → Registry (registered copy in `skills.dir`, state per `(id,
version)`) → Catalog (only `ENABLED` selectable) → Resolver (transitive closure, SemVer
max-satisfying, permission union, tool axes) → Loader (only the selected skills into the
Instance's `skills/`) → Delivery (a per-skill procedure file citing the material by
absolute path). Skills are added while running and usable from the next Task; hot reload
requires `hot_reload_safe` plus a permission re-check and a new revision; precedence is
Security/System policy ▸ aiprj rules ▸ skill instructions ▸ task prompt, and a skill can
never nullify the rules above it. Externally fetched skills are allow-listed by publisher
and source; high-risk use needs approval.

## 12. Human-in-the-Loop

Operations classified `high` by configuration require human approval before execution.
The human decides on Core Host's **canonical operation descriptor** (tool, arguments,
target, risk, requesting instance) — agent `reason`/`impact` prose is shown but marked
untrusted and never drives classification. Approvals carry a deadline (silence ⇒ reject),
one operation per approval, pattern scopes from the configured enumeration with TTL
bounds; consumption re-classifies risk. Waiting Instances are suspended (no process
held), capped by ratio. Clarifications (`ask_human`) put the Task into `WAITING_INPUT`
with its own deadline; expiry fails the Task — silence is never read as an answer.

## 13. Execution flow

```text
Human instruction → Task CREATED → QUEUED
  → Core Agent: SubmitUnderstanding / SubmitPlan (id issuance + revision bump)
  → Decide: ComposeHierarchy (admission control) — AgentSelected / SkillSelected
  → Act: Delegate / ExecuteTool (idempotency key), approval gate on high risk
  → Worker runs the PlanStep on its Instance → report → Result → store copy
  → Evaluate: objective Verification for verifiable deliverables (accept basis)
  → SUCCESS / RETRYING / REPLANNING / FAILED + user-visible reason and attempt list
```

## 14. State and events

Five state machines (Task, Agent Instance, Skill, Approval, PlanStep) are implemented
exactly as tabulated in §17; transitions are validated centrally by the State Manager
with `expected_state` checks, each transition appends its Event in the same transaction.
Events satisfy the §23.1 envelope (monotonic per-Core `sequence`, order per `task_id`),
are immutable, carry no secrets or full agent input, persist before SSE delivery, support
gap fetch by `sequence`, and replay reconstructs the run without side effects. Retention
is 90 days by default, preserving final results.

## 15. Security

Agent Processes — Core Agent included — are untrusted: Core Host is the single Policy
Enforcement Point. Bearer tokens with one of three roles (`viewer` / `operator` / `admin`)
guard the API, stored hashed only; `healthz` alone is unauthenticated and exposes no
state. Permission sets are minimal by default, delegation cannot widen them, no operation
lets an agent change its own permissions (audited structurally against the wire schema
too), filesystem scopes exclude the control surface, and secrets never appear in
plaintext anywhere.

## 16. Development and extension

The repository is one Cargo workspace with the dependency direction enforced in CI
(any unlisted edge fails). Rust everywhere; the browser UI is Rust → WebAssembly. Extend
by registering a new Runtime Adapter (no Core change), adding skills through the
Registry, or operating through the Core API alone — never by reaching Core internals.
Phases 9 (Hub) and 10 (SDK) wait for a requirement to arise.
