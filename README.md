# ATHENA

> Japanese version: [README_ja.md](./README_ja.md)(same content)

ATHENA is a system architecture that organizes multiple AI agents to carry out work requested in
natural language. The human gives a single natural-language instruction. The **ATHENA Core Agent
is an AI agent itself** — not a workflow engine — that understands the instruction, plans,
delegates down a hierarchy of Director / Lead / Worker agents, evaluates what comes back, and
re-plans when needed.

> The name is an acronym — **ATHENA = Autonomous Task Hierarchy & Execution Network
> Architecture** — and, per the specification (§1.1), it is not a product name but the name of a
> **system architecture** for organizing multiple AI agents to carry out tasks.

The system consists of five execution units plus a single authoritative store (`athena.db`, the `redb` engine — pure-Rust embedded,
ACID, file-backed)
that holds all persisted state:

| Unit | Kind | Role |
|---|---|---|
| `athena-api` | Resident process | Core API (HTTP + SSE); listens on 8080 by default; the composition root wiring Core Host and the Adapter implementations into one process |
| `athena-core` | Library crate | Core Host — supervision, state, event recording, policy enforcement. **It does not decide** |
| `athena-runtime-agent-cli` | Library crate | the `agent-cli` Runtime Adapter + Process Manager |
| `athena` | CLI | command-line UI (`athena chat` designed as a first-class feature) |
| `athena-studio` | Rust → WebAssembly | browser UI; listens on 5173 by default |

> **Mandatory statement** (`ATH-DOC-007`): the ATHENA Core Agent **is an AI Agent** —
> ATHENA Core ≠ a simple scheduler. `athena-core` (the Core Host) does **not** decide; it
> supervises, records state, and enforces policy only. All thinking happens in Agent Processes
> on `agent-cli` (Core Agent's instance #0 included), and Agent Processes sit outside the trust
> boundary.

## Repository layout

```text
athena/
+-- Cargo.toml                        workspace
+-- crates/
|   +-- athena-api/                   bin   Core API (resident)
|   +-- athena-cli/                   bin   the `athena` command
|   +-- athena-studio/                bin   Rust → WebAssembly
|   +-- athena-core/                  lib   Core Host
|   +-- athena-agent/                 lib   Agent definitions / instances / hierarchy
|   +-- athena-runtime/               lib   Runtime Interface (+ stub Runtime)
|   +-- athena-messaging/             lib   Message / Broker
|   +-- athena-memory/                lib   Memory Manager
|   +-- athena-tools/                 lib   Tool definitions and permissions
|   +-- athena-skills/                lib   Skill Manager / Registry / Catalog / Resolver
|   +-- athena-state/                 lib   State Manager and transition tables
|   +-- athena-events/                lib   Event definitions / Store
|   +-- athena-persistence/           lib   Repository implementations (the `redb` store)
|   +-- athena-config/                lib   configuration schema and resolution
|   +-- athena-types/                 lib   shared types (spec §26)
+-- runtimes/
|   +-- athena-runtime-agent-cli/     lib   agent-cli Adapter + Process Manager
+-- agents/  personas/  skills/       defaults distributed by `athena init`
+-- tests/                            conformance suites
+-- docs/                             system documentation (English + Japanese)
+-- spec/                             system specification (the single source of truth)
+-- LICENSE                           MIT
```

Module boundaries (dependency directions) are enforced mechanically by a static check in CI
(`tests/dependency_boundaries.rs`).

## Documentation

| Document | Content |
|---|---|
| `spec/athena_spec_1.0.md` | system specification (Version 1.0); every normative statement carries a requirement ID (`ATH-*`) |
| `docs/system-overview.md` | system overview (English) |
| `docs/system-overview_ja.md` | system overview (Japanese) |
| `docs/nfr_compliance.md` (+ `_ja.md`) | non-functional requirement (NFR-01…18) verification record |
| `docs/should_compliance.md` (+ `_ja.md`) | SHOULD-level requirement audit and deviation record |
| `docs/license-provenance.md` (+ `_ja.md`) | third-party material licence review and provenance |
| `docs/skill_delivery.md` (+ `_ja.md`) | the settled mechanism that carries skills to the agent |
| `docs/persona_generation.md` (+ `_ja.md`) | the Phase-8 persona-generation evaluation record |
| `docs/agent_cli_0290_findings.md` (+ `_ja.md`) | the `agent-cli` 0.29.0 verification record |
| `docs/diagrams/` | draw.io masters and the generated PNGs |

## Quick start

### 1. Install (builds the tool and puts it on PATH)

```sh
curl -fsSL https://raw.githubusercontent.com/aquaxis/athena/main/install.sh | sh
```

The one-liner checks the prerequisites, clones this repository, builds it in release mode with
`cargo build --release`, and installs the two binaries onto PATH: `athena` (the CLI) and
`athena-api` (the resident Core API). It is idempotent and tool-only — it never touches a
project's configuration or data. From an existing clone, run `./install.sh`; or build from
source with `cargo build --release` (the binaries then live at `./target/release/` — prefix
every command below with that path instead).

### 2. Use it

ATHENA is built per project: its files live inside the project directory you want it to work on.

```sh
cd ~/projects/my-project

# Generates .athena/ (config.toml, agents/, personas/, skills/, var/)
# and expands aiprj (.aiprj/, .agent-cli/); initialize the project once
athena init

# Start the resident Core API (listens on 127.0.0.1:8080) — the composition
# root: store integrity, Core Agent residency, supervision, safe shutdown
athena-api

# Edit .athena/config.toml and set the three operator-supplied values:
#   - core.agent.provider        (no default — ATH-AGENT-008)
#   - core.agent.model           (no default — ATH-AGENT-008)
#   - core.agent.delegable_tools (the permission ceiling must be explicit — ATH-SEC-024)
# Secrets never belong in this file (ATH-CFG-005): API tokens and provider keys
# come from environment variables or a secret manager. Configuration is applied
# at startup — restart athena-api after editing (R-CFG-09).
#
# Example core.agent values (an EXAMPLE only — nothing is pre-configured; pick your own
# provider, model, and ceiling):

```toml
[core.agent]
provider = "ollama"                          # example: the runtime's provider decision
model = "glm-5.3-flash:cloud"                # example: the provider's model
allowed_tools = ["fs_read", "fs_write", "send_to"]  # minimal by default (ATH-SEC-025)
delegable_tools = [
  "fs_read",
  "fs_write",
  "shell",   # medium-risk band — the rm -rf line still asks for approval
  "git",
  "send_to",
]
```
```

Then send work in natural language:

```sh
athena run "implement the TODO API"   # create + queue a Task
athena status                          # overall system status
athena task list                       # the Task list
```

Or use the conversational entry point — `athena chat` is a first-class natural-language
interface: it shows Task progress, surfaces approval requests inline, and accepts answers
to clarifying questions in the conversation.

Further commands: `athena task create/show/cancel`, `athena plan show`, `athena agent list/show`,
`athena instance list/stop` (admin), `athena runtime list`, `athena event tail`,
`athena approval list/approve/reject`, and the full §24.3 skill vocabulary —
`athena skill install/list/show/search/enable/disable/update/remove/validate` (enable/disable/
update/remove/validate pin `(id, version)`, `ATH-SKILL-020`; `update`/`validate` record the
request — the resident's skill lifecycle settles `UPDATING`/`VALIDATING` rows via the §12.5
Skill Manager / Skill Validator legs, `SkillUpdated`/`SkillValidated`/`SkillValidationFailed`).
Every command accepts `--json` for scripting. The CLI talks to the Core API only (`/api/v1`);
it never launches `agent-cli` directly (`ATH-CLI-001`/`002`).

Planning: when the Core Agent submits a plan, the resident accepts it as a plan — Core Host
mints the `PlanId`/`PlanStepId` values, stamps the revision (a submission must name the
revision it edited, and a stale one is refused with the current revision handed back), checks
`depends_on` for cycles before anything is written, and records the confirmation as
`PlanCreated` or `PlanRevised`. Earlier revisions are kept, never overwritten. A plan
submission does **not** settle its Task: a plan is the work's shape, not its product, so the
Task stays running and the agent receives a `control_response` naming what was minted.

Retention: the resident sweeps on a one-minute cadence — Events past `events.retention_days`,
expired clarifications, lapsed approvals, idempotency keys, delivery records, and each finished Instance's
`.aiprj` work area once it ages past the same Event horizon. An area is deleted only when the
store row and the directory together identify it as one ATHENA created for that Instance
(recorded path, or the exact `var/instances/<id>/.aiprj` layout, resolved inside the instances
tree and carrying the `AI_LOG/` shape the Runtime leaves); anything else is counted and left on
disk. An approval whose deadline passes is resolved as a **refusal**, never as silent consent —
both expiry edges (a request never answered, and a granted scope that lapses before it is
consumed) walk to `EXPIRED`, and a Task waiting on it fails rather than waiting forever. Every
cycle's counts ride the sweep's audit Event.

Observability: `GET /api/v1/metrics` answers the whole §30.3 catalog in Prometheus text —
all 23 families, each from recorded facts. Task, instance, approval, message and Event rows
feed the store-side families (counts by state, durations, failure and retry classes by
`error_class`, Skill loads by `skill_id`, approvals by outcome, the Broker dwell gauge, the
Core Agent's last-Event age), while the request families (`athena_api_requests_total`,
`athena_api_request_duration_seconds`) come from the API's own served requests, labelled by
matched route and status. A family no row answered renders a single zero so a scraper's
charts never toggle on series presence; an open label value the store never answered is
absent rather than invented.

## Updating

Keep the tool and a project's definitions current:

```sh
athena update
```

Running `athena update` performs the work equivalent to the one-line installer
(`curl -fsSL https://raw.githubusercontent.com/aquaxis/athena/main/install.sh | sh`):
it fetches the published repository, builds it in release mode, and re-installs
the `athena` and `athena-api` binaries onto PATH. When a `.athena/` project resolves
from the invocation directory, it additionally updates **only** the project's
`personas/` and `skills/` from the repository's shipped defaults — additively, never
overwriting a definition you have modified (`config.toml`, `agents/`, and `var/` are
never touched). In a directory without a `.athena/`, only the tool refresh runs.

Implementation-status note: the CLI verbs send the requests to the Core API and render the
responses — the plan face stays available with `` `--dry-run` ``, and `` `--json` `` carries the
API's own response body (`athena status` reads `GET /api/v1/status` — store counters plus the
Core Agent's standing — and `athena chat` runs its session loop: it posts the Task, polls the
state, and presents a clarification or an approval inline). `athena token issue --name <name>
--role <viewer|operator|admin>` (or `` `--json` ``) issues a token — the plaintext is shown once
and the registry stores the SHA-256 hash only (`ATH-SEC-005`, under
`<ATHENA_HOME>/var/api-tokens.json`) — and `athena token list` prints the records. The API's
read surface is served (`R-API-09`): agent definitions, agent instances (plus the admin
emergency stop on `/instances/{id}/stop`), runtime instances, tool executions, evaluations,
events, hierarchies, and each task's plan/results/messages are all real store rows,
cursor-paginated (`ATH-API-005`). The Studio
builds to WebAssembly via `scripts/build-studio.sh` and serves the built module on port 5173
(`athena-studio --api http://127.0.0.1:8080`); a page on the Studio origin reads the
CORS-restricted API (loopback Studio origins only), authenticating with a pasted token kept in
`sessionStorage` — adopted at boot, re-pasted once through an in-page panel when the API
answers 401. Deep links (`/?view=…`, `/?task=…`) land their screen, the tab bar survives every
screen change, and the Live screen follows the event stream through an authenticated fetch
replay anchored on `Last-Event-ID`; the page itself creates no tasks — task creation stays
with the CLI (the API's one-lease discipline).

Subordinate agents are live: the `agents` list in `.athena/config.toml` is registered into
the store at every startup (its `persona` file's front matter supplies `level` and
`allowed_tools` when an entry omits them, and `provider`/`model` stay mandatory —
`ATH-AGENT-008`), so a delegation can be admitted to a real definition (`ATH-AGENT-002`);
the Core Agent is never listed there (instance #0 is `core.agent`'s own face,
`ATH-CORE-022`). An admitted delegation is then **started**: the Instance row walks
`CREATED → STARTING → IDLE` with its `AgentStarting` / `AgentStarted` Events on the Task's
correlation chain, the Runtime Instance row is seeded 1:1 (`ATH-RT-012`) and the row records
the Runtime's own id and the `AI_LOG` area it wrote; a refused start lands
`STARTING → FAILED` with `AgentFailed` (`error_class` / `attempt` /
`side_effects = unknown`) rather than leaving the row mid-flight. The Core Agent's own
lifecycle now answers in catalog Events too (`CoreAgentStarted` / `CoreAgentRestarted` /
`CoreAgentEscalated`, `ATH-CORE-031`), which is what makes
`athena_core_agent_restarts_total` report observed restarts instead of a constant zero.

A started Instance is then **told what to do**: the delegation instruction the Core Agent
wrote travels to it as an `instruction` Message through the Broker (`ATH-MSG-001` — never a
direct path), on the delegation's own thread (`ATH-MSG-013`), under the
`timeout.message_reply` deadline (`ATH-MSG-005`), with the route checked against the
hierarchy (parent-child always, siblings only through the allow-list the Core Agent recorded
— `ATH-MSG-006`/`018`) and the dwell ceiling honoured (`messaging.max_queue_depth`,
`ATH-MSG-009`, answered as a transient refusal). The row and its delivery record are
persisted before the send is acknowledged (`ATH-MSG-012`), the peer's taking it is recorded
as acceptance (`ATH-MSG-014`), and both halves emit `AgentMessageSent` /
`AgentMessageReceived` (`ATH-MSG-007`). A message the peer never took stays dwelling, which
is exactly what `athena_message_queue_depth` reports.

And a restart no longer leaves ghosts: at boot the resident settles whatever the previous
process left non-terminal (`ATH-CORE-048`). The liveness fact comes from the store's own
record — the Runtime Instance row's pid, probed against the OS — because the previous
resident's process table died with it; a row whose process cannot be named is finalized
`FAILED` with side effects **unknown** and its `RUNNING` PlanStep goes `ORPHANED` in the
same transaction, which is precisely the state that gates any re-run behind an objective
Verification (`ATH-STATE-029`). A process that survived the restart is left alone.

`EVALUATING` now has substance (§19.2: the result an agent returned is not the final
result). `submit_evaluation` lands an **Evaluation row** with its evaluator, method,
verdict and rationale (`ATH-EVAL-003`) plus an `EvaluationPassed` / `EvaluationFailed`
Event, and the Task settles against what is recorded: an objective Verification that
accepted drives `SUCCESS`; any other recorded verdict drives `FAILED` with the verdict
preserved; and a Result carrying verifiable deliverables that no objective Verification
accepted can never be `SUCCESS` (`ATH-EVAL-002`). The Core Agent's own judgement is
recorded under the method `agent_judgement` — deliberately not one of the objective five —
so it can never accept a deliverable by itself (`ATH-EVAL-005`). A prose answer with no
deliverable and no evaluation still settles on the reported status: that is the case
`ATH-EVAL-002` does not reach, and every settlement now records which ground decided it.

A rejected approval no longer dead-ends. `transition_task` is accepted as a request Core
Host executes: the optimistic `expected_state` is checked (`ATH-CORE-036`), the §17.1 table
confirms the edge, and the Event is written with keys taken from store facts. The accepted
edges are the replanning cycle — `WAITING_APPROVAL` / `WAITING_INPUT` / `EVALUATING` →
`REPLANNING`, and `REPLANNING` → `RUNNING` — which are the rows the table assigns to the
Core Agent. Replanning is refused until the superseding plan revision exists, so
`TaskReplanning` never names a replacement that did not happen: submit the new plan first,
then request the transition. Other edges are refused *by name* (acceptance belongs to the
Result settlement, clarifications to `ask_human`, cancellation to the human), because a
second path to a gated edge would be a hole in the gate.

The subordinates finally have a reader. Each tick, after the outbound act, the resident
sweeps the reply deadlines it still owes (`ATH-MSG-005`): an `instruction` whose deadline
passes fires an `AgentMessageTimeout` observation — observation only, because the
resident's own patience is not a verdict, and the Core Instance never falls to a lapsed
wait (`ATH-STATE-023`). It then polls the runtime stream of every non-terminal Instance
the store holds: a `report` lands its row and its verbatim-relay observations
(`ATH-HIER-015`), appended as records whose consequence stays the deciding agent's
(`ATH-CORE-022`); an `error` lands its row and stands as an observation; a `complete`
walks `collect_result`, copies the instance-area artifacts into the ATHENA home
(`ATH-RESULT-004`), refuses any Task the state gate does not hand it, and moves the Task
`RUNNING` → `EVALUATING` with `TaskResultReceived` in one transaction (`ATH-PERS-003`) —
the boundary's provisional result identity re-minted Core-side (`ATH-CORE-041`), the
agent's self-declared status carried in metadata for the evaluation to judge
(`ATH-EVAL-005`). The Broker's route check and delivery dedup govern the whole walk
(`ATH-MSG-006`/`012`/`014`): a redelivered message is answered by the delivery already
recorded.

The catalog's silent events speak. Eight catalog events that had no production writer now
walk their true faces (`X-54`): `SystemStarted` opens the resident's boot after
`ConfigurationLoaded`, and `SystemStopping` closes it on the shutdown path — the system's
own lifetime, recorded; `WorkspaceLeased` rides the Task's create-time acquire and
`WorkspaceReleased` ends the Task's lease at its terminal edge (`ATH-DATA-009`'s
exclusivity); `RuntimeInstanceCreated` lands when the §26.6 row is inserted, and
`RuntimeInstanceStopped` terminates the row when the resident stops the instance
(`ATH-RT-010`); a successful settlement carries `AgentCompleted` — the agent's work done;
and skill registration records `SkillRegistered` with its `skill_id`/`version`/`checksum`
triple (`ATH-EVENT-017`). Every payload is the catalog's own — no shape was invented — and
the event catalog remains closed (`ATH-EVENT-009`).

## The four core personas speak Japanese and carry their reply protocol

The four core level personas are themselves Japanese — `personas/director_ja.md`,
`lead_ja.md`, `producer_ja.md`, `worker_ja.md` are the shipped personas (the defaults
`athena init` installs and the shipped config's `personas/producer_ja.md` reference), with
no English originals left behind. Each body embeds the `agent-cli` spawn-report protocol in
**both directions**: the parent's delegation prompt tells the child to report with
`send_to(peer=<親id>, delivery="report")` (plus the `delivery="prompt"` + `text="report
sent"` ping when the parent must act immediately), and the persona's own reporting duty
carries the same wording toward its superior. Front matter keeps the four keys and
`send_to` in `allowed_tools`.

## X-55: the edges into evaluation open the walk

`EvaluationStarted` (the catalog row `ATH-TEST-008` names as MVP-MUST) now rides its two
production `RUNNING -> EVALUATING` edges: an answer intake (`ingest_answer`) and a
subordinate's `complete` arriving through the resident's reception leg each append the
event once the transition commits — the payload the catalog's `empty()`, the actor the
walker of the edge (Core Agent on the intake, the System on the broker boundary), the
cause the transition's own `TaskResultReceived` carry (`ATH-EVENT-001`). The event
catalog stays closed (`ATH-EVENT-009`); no shape was invented.

## X-56: the Workspace initialization is recorded

`WorkspaceInitialized` (`ATH-AIPRJ-008`, MUST) now has a production emitter. The start
walk's happy leg appends it beside the `aiprj` area it records, carrying the §23.2 payload
pair — the `workspace` the row itself names (`ATH-OPS-009`, never a reconstruction) and
`aiprj_version` from the vendored `AIPRJ_VERSION` constant — on the walk's own correlation
chain, with `Actor::Runtime` and the row's Task/PlanStep/Instance coordinates.

The emission is **conditional on the Runtime's own fact**, not on a successful start: the
adapter's workspace preparation reports whether *this* start initialized (or repaired) the
areas, a start that found them `Ready` reports `false`, and nothing is recorded for it —
the Event-stream face of `ATH-AIPRJ-006`'s "no unconditional re-install". The fact reaches
Core the way the pid already does: the composition root answers it for the handle the
Runtime returned (`RuntimeStartFacts`), so `athena-core` neither computes nor guesses it,
and the spec-pinned `RuntimeInstanceRef` / `RuntimeInstanceStatus` gain no field. A handle
the manager holds no record for is logged as the anomaly it is and claims no
initialization.

## X-57: the Core Agent's stop is recorded

`CoreAgentFailed` (`ATH-CORE-031`, MUST) now has production emitters — it was the one row of
the four that `ATH-CORE-031` names without an emitter anywhere, while
`CoreAgentStarted` / `Restarted` / `Escalated` were wired by X-50.

Two sites record it, both where the failure is **observed** rather than where it is judged:
the supervision poll's death detection (`ATH-CORE-028`), written before the checksum, the
restart window and the recovery decide anything; and a boot that never brought instance #0
up — the only place that can record it, since the poll finds no record for an agent that
never started. The payload is the catalog's `empty()` (`ATH-EVENT-009` stays closed — the
reason rides the resident's log, no field invented), the actor is the Core Agent, and the
envelope joins no Task chain (`ATH-STATE-012`).

Recording the stop claims nothing further: a restart that follows is `CoreAgentRestarted`,
and a window-exhausted halt is `CoreAgentEscalated` (`ATH-CORE-030`) — neither is inferred
from the failure, and the boot arm records no escalation because it consults no window.

## Licence

MIT License — see [LICENSE](./LICENSE). Licence compatibility of third-party material is
reviewed before intake and its provenance recorded in `docs/license-provenance.md`
(`ATH-DEV-019`).
