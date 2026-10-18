//! What a Runtime is told when it starts an agent (design section 5.1).
//!
//! Carries everything the Runtime needs and nothing it must decide (ATH-ACLI-006): the
//! workspace to enter, the work targets (absolute paths, ATH-OPS-010), the permission sets,
//! the deadlines, and the references an instance resumes or rebuilds from. There is no
//! field for "what the agent should decide": that reaches the agent as the delegation
//! `instruction`, routed by Core (design section 4.1).

use crate::error::{Result, RuntimeError};
use athena_types::{Deadlines, InstanceId, ModelRef, PlanStepId, SkillRef, TaskId, ToolName};
use serde::{Deserialize, Serialize};

/// The permission boundary the agent writes inside (ATH-SEC-010, ATH-CORE-043).
///
/// Paths are absolute; the union is `Workspace ∪ work_targets` minus the control surface.
/// The control surface (`.agent-cli/`, the instance's `skills/`,
/// `<PROJECT_ROOT>/.aiprj/instructions.md` + `rules/`) is never listed — exclusion here is
/// the guarantee that an agent cannot rewrite the rules it runs under (ATH-CORE-043,
/// design section 4.4).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FilesystemScope {
    /// The instance's cwd and what may be written there.
    pub workspace: String,
    /// Absolute work-target paths; the only other writable roots.
    pub work_targets: Vec<String>,
    /// Absolute read-only conventions: `<PROJECT_ROOT>/.aiprj/` conventions and rules.
    pub read_only: Vec<String>,
}

/// Everything a Runtime receives with a `start`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartContext {
    /// Owning Task; `None` for the Core Agent instance, which spans Tasks
    /// (ATH-STATE-012).
    pub task_id: Option<TaskId>,
    /// The PlanStep assigned; `None` for the Core Agent instance.
    pub plan_step_id: Option<PlanStepId>,
    /// The Core-issued Agent Instance identifier (ATH-CORE-041): the Runtime mints no
    /// aggregate identity.
    pub instance_id: InstanceId,
    /// The Workspace, which becomes the process cwd (ATH-OPS-009).
    pub workspace: String,
    /// Absolute work-target paths (ATH-OPS-010).
    pub work_targets: Vec<String>,
    /// Persona file reference, resolved under `<ATHENA_HOME>/personas/`.
    pub persona_ref: String,
    /// Persona version, recorded in history (ATH-PERSONA-005).
    pub persona_version: String,
    /// Skills resolved by the Skill Resolver, pinned to versions (ATH-SKILL-010).
    pub skill_set: Vec<SkillRef>,
    /// Tools this agent may run; deny by default (ATH-SEC-008).
    pub allowed_tools: Vec<ToolName>,
    /// Tools this agent may grant downward; empty unless stated (ATH-SEC-024).
    pub delegable_tools: Vec<ToolName>,
    /// The permission boundary for file access.
    pub filesystem_scope: FilesystemScope,
    /// Provider and model, mandatory in the definition (ATH-AGENT-008).
    pub model: ModelRef,
    /// Startup and execution budgets the Runtime must enforce (ATH-RT-006).
    pub deadlines: Deadlines,
    /// Snapshot reference to resume from, after `SUSPENDED` (ATH-STATE-021).
    pub resume_snapshot_ref: Option<String>,
    /// Atomic context document reference, after a Core Agent restart
    /// (ATH-CORE-029, ATH-CORE-046).
    pub context_document_ref: Option<String>,
}

impl StartContext {
    /// Validate the invariants the Runtime relies on before it acts on the context.
    ///
    /// # Errors
    /// [`RuntimeError::Validation`] when the context would start an agent the Runtime
    /// cannot supervise: an empty workspace (cwd must exist), a relative work target
    /// (ATH-OPS-010), or missing model coordinates (ATH-AGENT-008).
    pub fn validate(&self) -> Result<()> {
        if self.workspace.trim().is_empty() {
            return Err(RuntimeError::Validation(
                "workspace must be the absolute instance directory (ATH-OPS-009)".into(),
            ));
        }
        for target in &self.work_targets {
            let absolute = target.starts_with('/')
                || target.len() >= 3 && {
                    let bytes = target.as_bytes();
                    bytes[1] == b':' && bytes[2] == b'\\'
                };
            if !absolute {
                return Err(RuntimeError::Validation(format!(
                    "work target {target:?} must be an absolute path (ATH-OPS-010)"
                )));
            }
        }
        if self.model.provider.trim().is_empty() || self.model.name.trim().is_empty() {
            return Err(RuntimeError::Validation(
                "model must carry provider and name; ATHENA holds no default (ATH-AGENT-008)"
                    .into(),
            ));
        }
        Ok(())
    }
}
