//! Per-Instance `.agent-cli/` generation (P3-09/P3-11; ATH-ACLI-012/013/014; design
//! §5.3/§5.3.1): the template origin's common part + the definition's coordinates —
/// config.toml with `[tools]`, `[permissions]`, `[spawn]`, `[history]`, `[provider]`;
/// `commands/` copied; `persona.md` placed — idempotent, never `install.sh`.
use std::path::{Path, PathBuf};
use thiserror::Error;

use athena_types::RuntimeType;
use athena_types::{AgentDefinition, Deadlines};

/// Where the template lives (`<ATHENA_HOME>/var/agent-cli-template/`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateOrigin {
    /// The `config.toml` *common part* every Instance inherits.
    pub config_template: String,
}

/// What one Instance's generated `config.toml` records (the TOML text + the facts the
/// Adapter derives from them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedConfig {
    /// The TOML file text.
    pub toml: String,
    /// Whether the history toggle carried the ATHENA-side value
    /// (`[history] enabled = true`, ATH-ACLI-010).
    pub history_enabled: bool,
}

/// The short-term rendering facts (P4-07): the ceiling and the tokenizer that counts it,
/// both *resolved configuration* (`ATH-MEM-009` — never an implicit provider default;
/// `new()`'s values are the §28.1 configuration-file defaults, which the file states
/// explicitly, and the boot wiring passes the resolved values through
/// [`crate::adapter::AgentCliAdapter::with_short_term`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortTermRender {
    /// `memory.short_term_max_tokens`, mirrored into `[history] max_context_tokens`
    /// (`ATH-MEM-010`, `ATH-ACLI-010`).
    pub max_tokens: u64,
    /// `memory.tokenizer`, the named counting rule (`ATH-MEM-009`).
    pub tokenizer: String,
}

impl Default for ShortTermRender {
    fn default() -> Self {
        Self {
            max_tokens: 32_000,
            tokenizer: "provider-default-claude".into(),
        }
    }
}

/// Why generation refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GenerationFault {
    /// A mandatory coordinate missing from the definition (ATH-AGENT-008 bites here too:
    /// the model must exist or no config renders).
    #[error("the definition lacks model.provider / model.name")]
    ModelUnspecified,
    /// `memory.tokenizer` arrived empty: the count would depend implicitly on a
    /// provider default (`ATH-MEM-009`).
    #[error("memory.tokenizer must be named; an empty value cannot count (ATH-MEM-009)")]
    TokenizerUnstated,
    /// A runtime kind the Adapter's coordinator cannot render.
    #[error("the Adapter renders only agent-cli runtime instances: {0:?}")]
    UnsupportedRuntime(RuntimeType),
    /// The template source was unreadable.
    #[error("the template origin {0:?} is unreachable")]
    TemplateSource(String),
    /// A work target is not an absolute path (P3-13; ATH-OPS-010). `StartContext::validate`
    /// refuses it first; the generation refuses it again as the belt-and-braces gate.
    #[error("the work target {0:?} is not an absolute path (ATH-OPS-010)")]
    RelativeWorkTarget(String),
    /// A procedure file in the origin still carries the upstream cwd-relative `@.aiprj/`
    /// read directive (P3-10; ATH-AIPRJ-028): an origin whose reads are not absolute cannot
    /// reach an Instance whose cwd differs every start; the origin must be the `athena
    /// init` one.
    #[error(
        "the procedure file {0:?} carries a cwd-relative @.aiprj/ read; expand with `athena init` so the origin reads are absolute (ATH-AIPRJ-028)"
    )]
    CwdRelativeRead(String),
}

/// The coordinates ATHENA injects into the placed `persona.md` (P3-10/P3-13): the work
/// targets by absolute path (ATH-OPS-010 — no cwd-relative reference may be generated).
/// The procedure-file pointers are derived from the copied `commands/`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PersonaCoordinates {
    /// Absolute work-target paths.
    pub work_targets: Vec<String>,
}

/// The ATHENA → `agent-cli` tool-vocabulary projection (design §5.3.1, D-11; ATH-ACLI-018):
/// the two vocabularies differ, so the rendered `[tools] enabled` carries only
/// `agent-cli`-canonical names — never the ATHENA wire forms. Peer-manipulating tools
/// (`spawn`, `stop_agent`, `list_agents`, `monitor`) have no mapping and are excluded
/// categorically: an Agent Process must not act on the peer registry outside Core Host's
/// supervision (ATH-HIER-011, ATH-MSG-001).
#[must_use]
pub fn project_tools(allowed: &[athena_types::ToolName]) -> Vec<&'static str> {
    use athena_types::ToolName;
    let mut names: Vec<&'static str> = Vec::new();
    for tool in allowed {
        let mapped: &[&'static str] = match tool {
            ToolName::FsRead => &["glob", "grep", "read"],
            ToolName::FsWrite => &["edit", "write"],
            ToolName::Shell | ToolName::Git | ToolName::Process | ToolName::Terminal => &["bash"],
            ToolName::Search => &["glob", "grep"],
            ToolName::SendTo => &["send_to"],
            ToolName::Network => &["webfetch", "websearch"],
        };
        names.extend_from_slice(mapped);
    }
    names.sort_unstable();
    names.dedup();
    names
}

/// Render the per-Instance `config.toml` (P3-09's file content contract).
///
/// The `[spawn]` values floor at ATHENA's own limit (`ATH-ACLI-009` — agent-cli's
/// defaults are lower); `[history]` carries `memory.short_term_max_tokens`
/// (ATH-ACLI-010); `[provider]` is the definition's coordinates, un-defaulted.
pub fn render_config(
    definition: &AgentDefinition,
    _deadlines: &Deadlines,
    max_spawn_depth: u64,
    max_spawn_children: u64,
    short_term: &ShortTermRender,
) -> Result<GeneratedConfig, GenerationFault> {
    if definition.model.provider.trim().is_empty() || definition.model.name.trim().is_empty() {
        return Err(GenerationFault::ModelUnspecified);
    }
    // P4-07 (`ATH-MEM-009`): the short-term counting never runs without a *named*
    // tokenizer — an empty `memory.tokenizer` cannot reach the wire.
    if short_term.tokenizer.trim().is_empty() {
        return Err(GenerationFault::TokenizerUnstated);
    }
    if definition.runtime != RuntimeType::AgentCli {
        return Err(GenerationFault::UnsupportedRuntime(definition.runtime));
    }
    let spawn_depth = max_spawn_depth.max(athena_types::AgentLevel::Worker.depth() + 1);
    let spawn_children = max_spawn_children.max(1);
    // ATH-ACLI-018: `agent-cli`-canonical tool names, TOML-quoted — the ATHENA wire forms
    // (`fs_read`, `shell`, …) are a different vocabulary and would name tools `agent-cli`
    // does not have (and a bare word would not even parse as a TOML string).
    let tools = project_tools(&definition.allowed_tools)
        .iter()
        .map(|name| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let toml = format!(
        "# generated per-instance by ATHENA (never install.sh, ATH-ACLI-013)\n\
         [runtime]\ncommands_dir = \".agent-cli/commands\"\npersona_file = \".agent-cli/persona.md\"\n\n\
         [tools]\nenabled = [{tools}]\n\n\
         [permissions]\ndefault_mode = \"allow\"\n\n\
         [spawn]\nmax_depth = {spawn_depth}\nmax_children = {spawn_children}\n\n\
         [history]\nenabled = true\nmax_context_tokens = {short_term_tokens}\n\n\
         [provider]\nkind = \"{}\"\nmodel = \"{}\"\n",
        definition.model.provider,
        definition.model.name,
        short_term_tokens = short_term.max_tokens,
    );
    Ok(GeneratedConfig {
        toml,
        history_enabled: true,
    })
    // A history ceiling reads the config's `memory.short_term_max_tokens` — the
    // specification says equal to that; the P3-09 line `max_context_tokens` is what the
    // config's default 32 000 carries, the placeholder kept for the session's check.
}

/// Write `.agent-cli/` under the Workspace: the generated `config.toml`, the copied
/// `commands/`, and the placed `persona.md`. Idempotent — a re-run overwrites the
/// generated artifacts with the same values (the write is source-of-truth, not
/// agent-owned, ATH-CORE-043).
///
/// `template_dir` is the `commands/` origin (ATH-ACLI-013: `var/agent-cli-template/`),
/// whose `commands/` carries **absolute** `<PROJECT_ROOT>/.aiprj/` reads (the `athena init`
/// origin). The copy is verbatim from an origin of that shape — the guard below refuses a
/// cwd-relative read rather than propagating a convention that would resolve against an
/// Instance's cwd.
///
/// # Errors
/// [`GenerationFault::TemplateSource`] on template reads; [`GenerationFault::CwdRelativeRead`]
/// when the origin is not the `athena init` one; [`GenerationFault::RelativeWorkTarget`]
/// when a work target is not absolute.
pub fn generate_agent_cli(
    agent_cli_dir: &Path,
    template_dir: &Path,
    persona_body: &str,
    work_targets: &[String],
    generated: &GeneratedConfig,
) -> Result<(), GenerationFault> {
    std::fs::create_dir_all(agent_cli_dir)
        .map_err(|_| GenerationFault::TemplateSource(agent_cli_dir.display().to_string()))?;
    std::fs::write(agent_cli_dir.join("config.toml"), &generated.toml)
        .map_err(|_| GenerationFault::TemplateSource(agent_cli_dir.display().to_string()))?;
    let commands_target = agent_cli_dir.join("commands");
    let commands_source = template_dir.join("commands");
    let mut procedure_files = Vec::new();
    if commands_source.is_dir() {
        std::fs::create_dir_all(&commands_target)
            .map_err(|_| GenerationFault::TemplateSource(commands_source.display().to_string()))?;
        for entry in std::fs::read_dir(&commands_source)
            .map_err(|_| GenerationFault::TemplateSource(commands_source.display().to_string()))?
        {
            let entry = entry.map_err(|_| {
                GenerationFault::TemplateSource(commands_source.display().to_string())
            })?;
            let target = commands_target.join(entry.file_name());
            let bytes = std::fs::read(entry.path())
                .map_err(|_| GenerationFault::TemplateSource(entry.path().display().to_string()))?;
            let text = String::from_utf8_lossy(&bytes);
            // The origin must be the `athena init` one: its `@.aiprj/` reads are absolute
            // (ATH-AIPRJ-028). Carrying a cwd-relative read into an Instance whose cwd is
            // `var/instances/<id>/` would resolve the convention against the wrong root.
            if text.contains("@.aiprj/") {
                return Err(GenerationFault::CwdRelativeRead(
                    entry.path().display().to_string(),
                ));
            }
            std::fs::write(&target, &bytes)
                .map_err(|_| GenerationFault::TemplateSource(target.display().to_string()))?;
            procedure_files.push(target);
        }
        procedure_files.sort();
    }
    let persona = render_persona(persona_body, work_targets, &procedure_files)?;
    std::fs::write(agent_cli_dir.join("persona.md"), persona)
        .map_err(|_| GenerationFault::TemplateSource(agent_cli_dir.display().to_string()))?;
    Ok(())
}

/// Render the placed `persona.md`: the persona source's body, then the execution
/// coordinates ATHENA injects at generation — the procedure files by **absolute** path
/// (ATH-ACLI-017: placement alone never applies the convention) and the work targets by
/// **absolute** path (ATH-OPS-010). The aiprj write targets stay cwd-relative
/// (`.aiprj/AI_PRJ_TASKS.md`, `AI_LOG/`) — the read/write split of ATH-AIPRJ-028.
///
/// # Errors
/// [`GenerationFault::RelativeWorkTarget`] when a work target is not absolute.
pub fn render_persona(
    body: &str,
    work_targets: &[String],
    procedure_files: &[PathBuf],
) -> Result<String, GenerationFault> {
    for target in work_targets {
        if !Path::new(target).is_absolute() {
            return Err(GenerationFault::RelativeWorkTarget(target.clone()));
        }
    }
    let mut persona = body.trim_end().to_string();
    persona.push_str("\n\n---\n");
    persona.push_str(
        "<!-- Execution coordinates injected by ATHENA at generation (ATH-ACLI-017, \
         ATH-OPS-010). Read paths below are absolute; the aiprj write targets are \
         cwd-relative (.aiprj/AI_PRJ_TASKS.md, .aiprj/AI_LOG/). -->\n",
    );
    persona.push_str("## Procedure files (read by absolute path)\n");
    if procedure_files.is_empty() {
        persona.push_str("- (none placed here)\n");
    }
    for file in procedure_files {
        persona.push_str(&format!("- {}\n", file.display()));
    }
    // P3-17: the aiprj operating model, mapped by direction — each placed procedure file
    // with its `/name` legacy tag and execution-model meaning. Realized as *the persona
    // pointing at the files*; `send`'s envelope says the same in the delegation body
    // (ATH-AIPRJ-011, ATH-ACLI-016/017). With no files placed there is nothing to point
    // at, and the block above already says so.
    if let Some(root) = procedure_files
        .first()
        .and_then(|path| workspace_root_of(path))
    {
        persona.push('\n');
        persona.push_str(&crate::instruction::operating_model_section(&root));
    }
    persona.push_str("## Work targets (absolute)\n");
    if work_targets.is_empty() {
        persona.push_str("- (none assigned; the Workspace is the scope)\n");
    }
    for target in work_targets {
        persona.push_str(&format!("- {target}\n"));
    }
    Ok(persona)
}

/// The `filesystem_scope` construction (P3-12 / ATH-SEC-010): the Workspace ∪ the work
/// targets, minus the control surface, minus `.athena/` — the boundaries the agent's
/// Runtime-layer permission projection carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesystemScope {
    /// Absolute writable roots (Workspace cwd + the work targets).
    pub writable: Vec<PathBuf>,
    /// Absolute read-only anchors (the project conventions).
    pub read_only: Vec<PathBuf>,
    /// Excluded roots (the control surface, `.athena/`, other Instances).
    pub excluded: Vec<PathBuf>,
}

/// Build the scope from the Workspace, the work targets, and the exclusions the
/// §29 security chapter prescribes (`.athena/` as a whole except the instance's own
/// directory; the Instance's own `.agent-cli/` and `skills/` on top, ATH-CORE-043).
#[must_use]
pub fn build_scope(
    workspace: &Path,
    work_targets: &[PathBuf],
    project_root: &Path,
) -> FilesystemScope {
    let mut writable = vec![workspace.to_path_buf()];
    writable.extend(work_targets.iter().cloned());
    let read_only = vec![
        project_root.join(".aiprj").join("instructions.md"),
        project_root.join(".aiprj").join("rules"),
    ];
    let excluded = vec![
        project_root.join(".athena"),
        workspace.join(".agent-cli"),
        workspace.join("skills"),
    ];
    FilesystemScope {
        writable,
        read_only,
        excluded,
    }
}

/// The Instance Workspace root behind one placed procedure file: `<root>/.agent-cli/
/// commands/<file>` → `<root>` (generation's persona block anchors the operating-model
/// section at the true absolute paths).
#[must_use]
pub fn workspace_root_of(procedure_file: &Path) -> Option<PathBuf> {
    let ancestors: Vec<_> = procedure_file.ancestors().map(Path::to_path_buf).collect();
    let here = procedure_file.parent()?;
    if here.file_name()? != "commands" {
        return None;
    }
    let agent_cli = here.parent()?;
    if agent_cli.file_name()? != ".agent-cli" {
        return None;
    }
    ancestors
        .iter()
        .find(|ancestor| ancestor.join(".agent-cli") == agent_cli.to_path_buf())
        .cloned()
}
