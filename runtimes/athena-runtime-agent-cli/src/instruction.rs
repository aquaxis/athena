//! The delegation envelope (P3-17, spec §21.4 / design §21.4):
//! the `/ai`, `/next_ai`, `/close_ai` operating model mapped onto the agent execution
//! model **by procedure-file direction**, never by slash command (`ATH-ACLI-016`).
//!
//! Phase 0 proved `commands_dir` expansion is REPL-only: over the IPC path ATHENA drives
//! (`send`/`ask`), `/<name>` arrives as literal text. The operating model therefore maps
//! as: every delegation instruction directs the agent to the corresponding procedure
//! files **by absolute path** (`ATH-AIPRJ-011`, `ATH-ACLI-017`) — placement alone
//! applies nothing.

use std::path::{Path, PathBuf};

/// The aiprj operating-model procedures ATHENA's execution model maps onto (the vendored
/// `commands/` names; `ATH-AIPRJ-011`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Procedure {
    /// `/ai` — execute the delegated instruction under the conventions.
    RunJob,
    /// `/next_ai` — advance to the next assigned unit.
    NextJob,
    /// `/close_ai` — save `AI_LOG` and report the Result to Core.
    CloseJob,
}

impl Procedure {
    /// The vendored file name this procedure lives in.
    #[must_use]
    pub const fn file_name(self) -> &'static str {
        match self {
            Self::RunJob => "ai.md",
            Self::NextJob => "next_ai.md",
            Self::CloseJob => "close_ai.md",
        }
    }

    /// The absolute path of the procedure inside the Instance Workspace.
    #[must_use]
    pub fn path_in(self, workspace: &Path) -> PathBuf {
        workspace
            .join(".agent-cli")
            .join("commands")
            .join(self.file_name())
    }

    /// The execution-model meaning, in the direction text (`ATH-AIPRJ-011`'s mapping).
    #[must_use]
    pub const fn meaning(self) -> &'static str {
        match self {
            Self::RunJob => {
                "execute the delegated instruction under the aiprj conventions it names \
                 (reads are absolute into the project layer; the write targets — \
                 .aiprj/AI_PRJ_TASKS.md and .aiprj/AI_LOG/ — are cwd-relative)"
            }
            Self::NextJob => {
                "advance to the next assigned unit only on direction; a v1 Instance holds \
                 exactly one PlanStep (one Worker per step), so this is not the default path"
            }
            Self::CloseJob => {
                "before reporting: save the run log to .aiprj/AI_LOG/YYYY-MM-DD_NNN.md \
                 (never overwrite an existing log), update .aiprj/AI_PRJ_TASKS.md, then \
                 report the Result to Core"
            }
        }
    }
}

/// Why the envelope refuses (`ATH-ACLI-016`, `ATH-ACLI-017`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvelopeFault {
    /// The instruction body is empty — nothing to delegate.
    #[error("the delegation instruction is empty")]
    EmptyInstruction,
    /// The body is a bare slash command: over the IPC path it would arrive as literal
    /// text (`ATH-ACLI-016`); ATHENA directs procedure files instead.
    #[error(
        "the delegation must not invoke a slash command ({0:?}); direct the procedure file by absolute path (ATH-ACLI-016)"
    )]
    SlashCommandRefused(String),
    /// The procedure file the direction names was never placed in the Workspace.
    #[error(
        "the procedure file {0:?} is not placed in the Workspace; the operating model cannot be directed (ATH-ACLI-017)"
    )]
    ProcedureMissing(PathBuf),
}

/// The delegation envelope (P3-17): the instruction body plus the operating-model
/// direction naming the placed procedure files **by absolute path**.
///
/// # Errors
/// [`EnvelopeFault::EmptyInstruction`], [`EnvelopeFault::SlashCommandRefused`] (a bare
/// `/<name>` body, the dependence `ATH-ACLI-016` forbids), or
/// [`EnvelopeFault::ProcedureMissing`] when the Workspace lacks a placed procedure file.
pub fn delegation_envelope(workspace: &Path, instruction: &str) -> Result<String, EnvelopeFault> {
    let bare = instruction.trim();
    if bare.is_empty() {
        return Err(EnvelopeFault::EmptyInstruction);
    }
    for name in ["/ai", "/next_ai", "/close_ai"] {
        if bare == name {
            return Err(EnvelopeFault::SlashCommandRefused(name.to_string()));
        }
    }
    let run = Procedure::RunJob.path_in(workspace);
    let next = Procedure::NextJob.path_in(workspace);
    let close = Procedure::CloseJob.path_in(workspace);
    for path in [&run, &next, &close] {
        if !path.is_file() {
            return Err(EnvelopeFault::ProcedureMissing(path.clone()));
        }
    }
    let absolute = |path: &Path| path.display().to_string();
    Ok(format!(
        "{bare}\n\n---\n\
         ATHENA execution direction (ATH-AIPRJ-011; read the files by absolute path — \
         never invoke slash commands, they do not expand over the IPC path, \
         ATH-ACLI-016):\n\
         1. Read {} — meaning: {}.\n\
         2. Read {} when directed — meaning: {}.\n\
         3. Read {} when the work is complete — meaning: {}.",
        absolute(&run),
        Procedure::RunJob.meaning(),
        absolute(&next),
        Procedure::NextJob.meaning(),
        absolute(&close),
        Procedure::CloseJob.meaning(),
    ))
}

/// The operating-model section injected into the placed `persona.md` (P3-17): each
/// placed procedure file paired with its `/name` legacy tag and the execution-model
/// meaning — pointing, never invoking.
pub fn operating_model_section(workspace: &Path) -> String {
    let mut section = String::from(
        "## Operating model (ATH-AIPRJ-011; the /ai, /next_ai, /close_ai tags are NOT commands)\n",
    );
    for procedure in [Procedure::RunJob, Procedure::NextJob, Procedure::CloseJob] {
        let path = procedure.path_in(workspace);
        let placed = if path.is_file() {
            path.display().to_string()
        } else {
            format!("(not placed: {})", path.display())
        };
        section.push_str(&format!("- {} → {}\n", procedure.file_name(), placed));
        section.push_str(&format!("  meaning: {}\n", procedure.meaning()));
    }
    section.push_str(
        "ATHENA directs these files by the absolute paths above; a bare \"/ai\" over the \
         IPC path is literal text, not an expansion (ATH-ACLI-016).\n",
    );
    section
}
