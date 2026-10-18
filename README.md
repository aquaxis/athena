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

The system consists of five execution units plus a single authoritative SQLite store (`athena.db`)
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
|   +-- athena-persistence/           lib   Repository implementations (SQLite)
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

# Generates .athena/ (config.yaml, agents/, personas/, skills/, var/)
# and expands aiprj (.aiprj/, .agent-cli/); initialize the project once
athena init

# Start the resident Core API (listens on 127.0.0.1:8080) — the composition
# root: store integrity, Core Agent residency, supervision, safe shutdown
athena-api

# Edit .athena/config.yaml and set the three operator-supplied values:
#   - core.agent.provider        (no default — ATH-AGENT-008)
#   - core.agent.model           (no default — ATH-AGENT-008)
#   - core.agent.delegable_tools (the permission ceiling must be explicit — ATH-SEC-024)
# Secrets never belong in this file (ATH-CFG-005): API tokens and provider keys
# come from environment variables or a secret manager. Configuration is applied
# at startup — restart athena-api after editing (R-CFG-09).
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

Further commands: `athena task show/cancel`, `athena plan show`, `athena agent list/show`,
`athena instance list/stop` (admin), `athena runtime list`, `athena event tail`,
`athena approval list/approve/reject`, `athena skill list/show/search/register/enable/disable`.
Every command accepts `--json` for scripting. The CLI talks to the Core API only (`/api/v1`);
it never launches `agent-cli` directly (`ATH-CLI-001`/`002`).

Implementation-status note: the CLI verbs currently compose and print the request they would
send to the Core API (method / path / body). Tokens are recorded only as SHA-256 hashes under
`<ATHENA_HOME>/var/api-tokens.json` (`ATH-SEC-005`). The Studio UI builds to WebAssembly via
`scripts/build-studio.sh` and serves on port 5173
(`athena-studio --api http://127.0.0.1:8080`).

## Licence

MIT License — see [LICENSE](./LICENSE). Licence compatibility of third-party material is
reviewed before intake and its provenance recorded in `docs/license-provenance.md`
(`ATH-DEV-019`).
