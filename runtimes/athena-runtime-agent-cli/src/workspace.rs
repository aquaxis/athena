//! Instance Workspace construction (P3-05/P3-06; spec §14.4, §36, design §21).

use std::path::{Path, PathBuf};
use thiserror::Error;

/// Why workspace construction failed (I/O-shaped).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkspaceFault {
    /// The path was touched but is not a directory.
    #[error("{0:?} exists and is not a directory")]
    NotADirectory(String),
    /// A write-area file already existed with content the init must not overwrite
    /// (ATH-AIPRJ-006: re-running never discards).
    #[error("the write-area file {0:?} exists; aiprj init never overwrites (ATH-AIPRJ-006)")]
    Exists(String),
    /// The Instance Workspace carries aiprj **read-side** material — `instructions.md`,
    /// `rules/`, or the requirement/design documents (ATH-AIPRJ-027). The start refuses:
    /// the convention's read side belongs to the project layer, referenced by absolute
    /// path (`ATH-AIPRJ-028`), never copied into an Instance area.
    #[error("the workspace carries aiprj read-side material at {path:?}: {detail} (ATH-AIPRJ-027)")]
    Contaminated {
        /// The first forbidden path found (the report names one; refusal is whole).
        path: String,
        /// What it is.
        detail: String,
    },
    /// Filesystem failures surfaced with their path.
    #[error("filesystem failure on {path:?}: {detail}")]
    Io {
        /// The touched path.
        path: String,
        /// The error text.
        detail: String,
    },
}

/// The layout one Instance's Workspace ends up with (the §14.4 diagram's per-instance
/// slice).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLayout {
    /// The Instance's cwd root.
    pub root: PathBuf,
    /// The aiprj write area: `<root>/.aiprj/`.
    pub aiprj: PathBuf,
    /// `AI_PRJ_TASKS.md` inside the write area.
    pub tasks_file: PathBuf,
    /// `AI_LOG/` inside the write area.
    pub logs_dir: PathBuf,
    /// The generated `.agent-cli/` root.
    pub agent_cli: PathBuf,
    /// The resolved skills placement (ATH-SKILL-017: `<workspace>/skills/`).
    pub skills: PathBuf,
}

/// Compute the layout from a root (no I/O).
#[must_use]
pub fn workspace_layout(root: &Path) -> WorkspaceLayout {
    let aiprj = root.join(".aiprj");
    WorkspaceLayout {
        root: root.to_path_buf(),
        aiprj: aiprj.clone(),
        tasks_file: aiprj.join("AI_PRJ_TASKS.md"),
        logs_dir: aiprj.join("AI_LOG"),
        agent_cli: root.join(".agent-cli"),
        skills: root.join("skills"),
    }
}

/// The Core Agent's dedicated Workspace root (P3-15): `<ATHENA_HOME>/var/core-agent/` —
/// never shared with subordinate Instances and never `<PROJECT_ROOT>` (`ATH-OPS-009`,
/// spec §7.5.1). The Core Agent Session's boot context is built from this path.
#[must_use]
pub fn core_agent_workspace(athena_home: &Path) -> PathBuf {
    athena_home.join("var").join("core-agent")
}

fn io_wrap(path: &Path, error: std::io::Error) -> WorkspaceFault {
    WorkspaceFault::Io {
        path: path.display().to_string(),
        detail: error.to_string(),
    }
}

/// Initialize the Instance Workspace: directories + the aiprj **write areas only** —
/// `AI_PRJ_TASKS.md` (created empty when absent) and `AI_LOG/` — never the conventions,
/// the three documents, or `instructions.md` (ATH-AIPRJ-025/027; the read side rides the
/// command definitions' absolute-path references, ATH-AIPRJ-028).
///
/// `allow_existing` mirrors the idempotency reality: a re-run on a live Workspace must
/// not clobber the agent's task file or logs. `false` refuses an existing write-area.
///
/// # Errors
/// [`WorkspaceFault`] with the touched path.
pub fn init_workspace(
    root: &Path,
    allow_existing: bool,
) -> Result<WorkspaceLayout, WorkspaceFault> {
    let layout = workspace_layout(root);
    std::fs::create_dir_all(&layout.root).map_err(|error| io_wrap(&layout.root, error))?;
    if layout.root.is_file() {
        return Err(WorkspaceFault::NotADirectory(
            layout.root.display().to_string(),
        ));
    }
    std::fs::create_dir_all(&layout.aiprj).map_err(|error| io_wrap(&layout.aiprj, error))?;
    std::fs::create_dir_all(&layout.logs_dir).map_err(|error| io_wrap(&layout.logs_dir, error))?;
    std::fs::create_dir_all(&layout.skills).map_err(|error| io_wrap(&layout.skills, error))?;
    if layout.tasks_file.exists() && !allow_existing {
        return Err(WorkspaceFault::Exists(
            layout.tasks_file.display().to_string(),
        ));
    }
    if !layout.tasks_file.exists() {
        std::fs::write(&layout.tasks_file, "")
            .map_err(|error| io_wrap(&layout.tasks_file, error))?;
    }
    Ok(layout)
}

/// The Workspace's `aiprj` readiness, read from disk — no I/O beyond stat (P3-16,
/// `ATH-AIPRJ-024`: readiness before start; initialize only when uninitialized).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiprjReadiness {
    /// The write areas (`.aiprj/AI_PRJ_TASKS.md` + `.aiprj/AI_LOG/`) exist: nothing to do,
    /// and the start must not touch them again (idempotence, `ATH-AIPRJ-006`).
    Ready,
    /// No `.aiprj/` at all — the areas initialize at this start.
    Uninitialized,
    /// The areas exist but are incomplete; the init fills **only what is missing**.
    Incomplete {
        /// The missing write-area items, by name.
        missing: Vec<&'static str>,
    },
}

/// One start's workspace preparation (`ATH-AIPRJ-024`): readiness first, then init
/// **only when** the areas are not [`AiprjReadiness::Ready`], with only-missing fills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedWorkspace {
    /// The layout after preparation.
    pub layout: WorkspaceLayout,
    /// What the disk showed before this start.
    pub readiness_before: AiprjReadiness,
    /// Whether *this start* initialized (or repaired) the areas — the fact the Core
    /// Host turns into the `WorkspaceInitialized` Event (emission is Core's, D-1).
    pub initialized: bool,
}

/// The forbidden **read-side** material an Instance Workspace must never carry
/// (`ATH-AIPRJ-027`): placement alone would resolve the conventions against the Instance
/// cwd instead of the project layer.
const FORBIDDEN_READ_SIDE: &[(&str, &str)] = &[
    ("instructions.md", "the project instructions"),
    ("rules", "the rules directory"),
    ("AI_PRJ_REQUIREMENTS.md", "the requirements document"),
    ("AI_PRJ_DESIGN.md", "the design document"),
];

/// Read one Workspace's `aiprj` readiness from disk (P3-16, `ATH-AIPRJ-024`), without
/// initializing anything: [`AiprjReadiness::Uninitialized`] when no `.aiprj/` exists,
/// [`AiprjReadiness::Incomplete`] when the areas exist but are partial, and
/// [`AiprjReadiness::Ready`] when both write areas stand.
///
/// # Errors
/// [`WorkspaceFault::Contaminated`] when read-side material sits in the area
/// (`ATH-AIPRJ-027`); I/O faults surfaced whole.
pub fn aiprj_readiness(root: &Path) -> Result<AiprjReadiness, WorkspaceFault> {
    let layout = workspace_layout(root);
    if layout.aiprj.exists() {
        for (name, detail) in FORBIDDEN_READ_SIDE {
            let forbidden = layout.aiprj.join(name);
            if forbidden.exists() {
                return Err(WorkspaceFault::Contaminated {
                    path: forbidden.display().to_string(),
                    detail: (*detail).to_string(),
                });
            }
        }
    }
    Ok(if !layout.aiprj.exists() {
        AiprjReadiness::Uninitialized
    } else {
        let mut missing = Vec::new();
        if !layout.tasks_file.exists() {
            missing.push("AI_PRJ_TASKS.md");
        }
        if !layout.logs_dir.is_dir() {
            missing.push("AI_LOG/");
        }
        if missing.is_empty() {
            AiprjReadiness::Ready
        } else {
            AiprjReadiness::Incomplete { missing }
        }
    })
}

/// Prepare the Workspace for one agent start (P3-16): refuse read-side contamination,
/// compute the readiness, initialize only when not [`AiprjReadiness::Ready`] —
/// write areas only, never the conventions.
///
/// `allow_existing` is the init's idempotency door (see [`init_workspace`]).
///
/// # Errors
/// [`WorkspaceFault::Contaminated`] when read-side material sits in the area
/// (`ATH-AIPRJ-027` — the start refuses, never repairs silently); other
/// [`WorkspaceFault`]s from the init's I/O.
pub fn prepare_workspace(
    root: &Path,
    allow_existing: bool,
) -> Result<PreparedWorkspace, WorkspaceFault> {
    let layout = workspace_layout(root);
    let readiness = aiprj_readiness(root)?;
    if readiness == AiprjReadiness::Ready {
        return Ok(PreparedWorkspace {
            layout,
            readiness_before: readiness,
            initialized: false,
        });
    }
    let layout = init_workspace(root, allow_existing)?;
    Ok(PreparedWorkspace {
        layout,
        readiness_before: readiness,
        initialized: true,
    })
}
