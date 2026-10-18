//! Embedded template content for `athena init` (ATH-CLI-009, ATH-ACLI-013).
//!
//! Two kinds of content, each with its own drift policy:
//!
//! 1. **ATHENA's distributed defaults** (`personas/`, `agents/`) are included directly from the
//!    repository's own directories with `include_str!`, so a repository edit and the binary the
//!    `athena init` produces can never diverge. These are the defaults spec section 32.1 lists
//!    under `agents/ personas/ skills/` for copying into a target project.
//!
//! 2. **The `aiprj` expansion contents** are a verbatim copy of `aquaxis/aiprj` 1.1.0 (MIT
//!    License) — `instructions.md` seed, `rules/`, the `.agent-cli/` project overlay and the
//!    command wrappers — held under `vendor/aiprj/`. Provenance per R-LIC-01 / X-07: source
//!    repository `https://github.com/aquaxis/aiprj` (v1.1.0), license MIT, files unmodified.
//!    The quoted rule files are the project's operation conventions; the command wrappers are
//!    the `/setup_ai /ai /update_ai /next_ai /close_ai` entries `install.sh` expands.

/// The producer persona (repo default, P1-44).
pub const PERSONA_PRODUCER: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../personas/producer.md"
));

/// The director persona (repo default, P2-19).
pub const PERSONA_DIRECTOR: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../personas/director.md"
));

/// The lead persona (repo default, P2-19).
pub const PERSONA_LEAD: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../personas/lead.md"
));

/// The worker persona (repo default, P2-19).
pub const PERSONA_WORKER: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../personas/worker.md"
));

/// The Core Agent loop procedure (repo default).
pub const AGENT_PRODUCER_LOOP: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../agents/producer/loop.md"
));

/// The Core Request catalogue procedure (repo default).
pub const AGENT_PRODUCER_CORE_REQUESTS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../agents/producer/core-requests.md"
));

/// The `aiprj` `instructions.md` seed (`instructions.md.org` in the upstream template). An
/// existing project's `instructions.md` is never overwritten (ATH-AIPRJ-007).
pub const AIPRJ_INSTRUCTIONS: &str = include_str!("../vendor/aiprj/instructions.md.org");

/// The `aiprj` operation rules: `(target file name, content)`.
pub const AIPRJ_RULES: &[(&str, &str)] = &[
    (
        "exec_job.md",
        include_str!("../vendor/aiprj/rules/exec_job.md"),
    ),
    (
        "setup_project.md",
        include_str!("../vendor/aiprj/rules/setup_project.md"),
    ),
    (
        "update_project.md",
        include_str!("../vendor/aiprj/rules/update_project.md"),
    ),
    (
        "close_ai.md",
        include_str!("../vendor/aiprj/rules/close_ai.md"),
    ),
];

/// The `.agent-cli/commands/` command wrappers `(target file name, content)`.
pub const AIPRJ_COMMANDS: &[(&str, &str)] = &[
    (
        "setup_ai.md",
        include_str!("../vendor/aiprj/commands/setup_ai.md"),
    ),
    ("ai.md", include_str!("../vendor/aiprj/commands/ai.md")),
    (
        "update_ai.md",
        include_str!("../vendor/aiprj/commands/update_ai.md"),
    ),
    (
        "next_ai.md",
        include_str!("../vendor/aiprj/commands/next_ai.md"),
    ),
    (
        "close_ai.md",
        include_str!("../vendor/aiprj/commands/close_ai.md"),
    ),
];

/// The `aiprj` project-local `agent-cli` overlay `config.toml` (the template origin's common
/// part, ATH-ACLI-013).
pub const AIPRJ_AGENT_CLI_CONFIG: &str = include_str!("../vendor/aiprj/agent-cli-config.toml");

/// The `.gitignore` block `aiprj install.sh` adds at a project root.
pub const AIPRJ_GITIGNORE_ENTRIES: &str = include_str!("../vendor/aiprj/gitignore-entries");

/// The default `config.yaml` written into `.athena/` (ATH-CLI-009: 既定値).
///
/// Values mirror `athena-config`'s schema defaults (spec section 28.1). The dependency graph
/// forbids the CLI from reading the schema itself, so the pairing is held by the conformance
/// drift test (`tests/init_defaults.rs` in `athena-conformance`), which parses and validates
/// this text through the real schema.
pub const DEFAULT_CONFIG_YAML: &str = include_str!("default-config.yaml");
