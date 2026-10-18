//! The placed-skill **delivery mechanism** (P5-08; open item O-2 settled).
//!
//! `agent-cli` has no skill mechanism (spec §2.2). The `skills/` material the Loader
//! places under the workspace is inert until the *Agent* reaches it. The design
//! commitment (design §9.2) is settled here: delivery rides the two channels
//! `agent-cli` provably reads —
//!
//! 1. the Instance's generated **`.agent-cli/commands/`** directory, which per the
//!    `agent-cli` 0.23.0 verification (spec §2.2.3, `ATH-ACLI-016`/`017`) the Agent
//!    reads as **procedure files by absolute path** in the IPC route; and
//! 2. the generated **`persona.md`**, whose working-convention section may reference
//!    the placed material.
//!
//! The concrete form: per placed skill, one procedure file
//! `.agent-cli/commands/athena-skill-<id>.md` — front matter-free, its *entire content*
//! is an absolute-path instruction — "Read `<workspace>/skills/<id>/SKILL.md` (absolutely,
//! version-pinned) and apply its procedure when the capability is required." The Agent
//! never receives skill instructions except through the placed body (the Catalog only
//! *names* a skill; prose injection is what the message channel carries, and risk-bearing
//! skill use routes through approval — P5-12).
//!
//! Nothing here ever writes `.agent-cli/skills/` (`ATH-ACLI-015` MUST NOT).

use crate::loader::Placement;
use std::path::Path;

/// Why delivery was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeliveryFault {
    /// The Instance's `.agent-cli/commands/` root could not be written.
    #[error(".agent-cli/commands write failed: {0}")]
    Io(String),
    /// The forbidden path was passed (`ATH-ACLI-015`).
    #[error(".agent-cli/skills/ is never a delivery target (ATH-ACLI-015)")]
    ForbiddenTarget,
}

/// The delivery plan: the procedure files the mechanism asks the wiring to generate
/// under the Instance's `.agent-cli/commands/` tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    /// One procedure file per placed skill.
    pub files: Vec<DeliveryFile>,
}

/// One delivered file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryFile {
    /// Absolute path of the procedure file.
    pub path: String,
    /// Its body (the instruction text).
    pub body: String,
    /// The skill it delivers.
    pub skill_id: String,
}

/// Build and write the delivery for one placement set.
///
/// `workspace` is the Instance's workspace root (the Loader's write area);
/// `agent_cli_dir` is the Instance's `.agent-cli/` root (the generated area). The
/// mechanism composes: `<workspace>/skills/<id>/SKILL.md` (material, by path) +
/// `<agent_cli_dir>/commands/athena-skill-<id>.md` (the procedure that reads it).
///
/// # Errors
/// [`DeliveryFault`] — any disk failure surfaced whole.
pub fn deliver(
    placed: &[Placement],
    workspace: &Path,
    agent_cli_dir: &Path,
) -> Result<Delivery, DeliveryFault> {
    if agent_cli_dir.join("skills").exists() {
        // The forbidden subtree must not exist; refuse to touch the tree it belongs to.
        return Err(DeliveryFault::ForbiddenTarget);
    }
    let commands = agent_cli_dir.join("commands");
    std::fs::create_dir_all(&commands).map_err(|error| DeliveryFault::Io(error.to_string()))?;

    let mut files = Vec::with_capacity(placed.len());
    for placement in placed {
        let name = format!("athena-skill-{}.md", placement.skill_id.as_str());
        let path = agent_cli_dir.join("commands").join(&name);
        // The instruction cites the absolute path of the placed material.
        let material_path = workspace
            .join("skills")
            .join(placement.skill_id.as_str())
            .join("SKILL.md")
            .display()
            .to_string();
        let body = format!(
            "ATHENA skill procedure — placed material\n\n\
             Skill `{id}` (version {version}, checksum {checksum}).\n\n\
             Before using this skill, read the pinned material and follow its procedure. and apply it:\n\n\
             Read the file `{material_path}` (absolute path; version-pinned at start\n\
             ATH-SKILL-020; immune to UPDATING/REMOVED mid-run ATH-STATE-009) and follow it.\n",
            id = placement.skill_id.as_str(),
            version = placement.version,
            checksum = placement.checksum,
            material_path = material_path,
        );
        std::fs::write(&path, body.as_bytes())
            .map_err(|error| DeliveryFault::Io(error.to_string()))?;
        files.push(DeliveryFile {
            path: path.display().to_string(),
            body,
            skill_id: placement.skill_id.as_str().to_string(),
        });
    }
    Ok(Delivery { files })
}
