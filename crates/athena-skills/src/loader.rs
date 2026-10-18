//! The Skill Loader (P5-07; spec §12.6→§17 / `ATH-SKILL-017`, `ATH-ACLI-015`):
//! place **only** the skills the Core Agent resolved ([`Resolution`]) under the target
//! Agent Instance's workspace (`<ATHENA_HOME>/var/instances/<instance_id>/skills/`).
//! Nothing unselected is placed; nothing is ever written under `.agent-cli/skills/`
//! (that path would imply a mechanism `agent-cli` does not have).

use crate::registry::SkillStore;
use crate::resolver::Resolution;
use athena_types::{Skill, SkillId};
use std::collections::HashMap;
use std::path::Path;

/// Why a placement was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LoaderFault {
    /// A registered row is missing from the registered tree (the Registry space and the
    /// on-disk copy disagree; a registration bug, never repaired by an implicit
    /// fallback).
    #[error("registered row {id}@{version} is missing from the registered tree")]
    RegisteredCopyMissing {
        /// The skill.
        id: String,
        /// The version.
        version: String,
    },
    /// The workspace must not be the registered tree itself.
    #[error("the workspace and the registered tree must differ (ATH-SKILL-038's isolation)")]
    TreesCollision,
    /// The disk refused.
    #[error("workspace write failed: {0}")]
    Io(String),
}

/// One placed skill — the frozen-start fact (`ATH-SKILL-010`): id, pinned version, and
/// the recorded content checksum (`ATH-SKILL-021`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// The skill.
    pub skill_id: SkillId,
    /// The pinned version.
    pub version: String,
    /// The material's checksum at placement (`ATH-SKILL-021`'s recorded face).
    pub checksum: String,
    /// The absolute SKILL.md path inside the workspace.
    pub skill_md_path: String,
}

/// Place the selected skills: copy `registered_dir/<id>/<version>/` →
/// `<workspace>/skills/<id>/` for exactly the resolution's members. Only the placed set
/// is returned; the resolution is the authority — a registered skill outside it is
/// never placed (`ATH-SKILL-017`), and unmaterializable rows refuse whole
/// (`ATH-SKILL-018` keeps the registered body authoritative; the workspace copy is a
/// copy, updated only through re-resolution).
///
/// # Errors
/// [`LoaderFault`] — first failure surfaced whole, nothing half-placed (the walk
/// verifies every source copy before any write).
pub fn place(
    resolution: &Resolution,
    registered_dir: &Path,
    workspace: &Path,
    store: &dyn SkillStore,
) -> Result<Vec<Placement>, LoaderFault> {
    if workspace.starts_with(registered_dir) || registered_dir.starts_with(workspace) {
        return Err(LoaderFault::TreesCollision);
    }

    // The resolution's pins, in order.
    let pins: HashMap<String, String> = resolution
        .members
        .iter()
        .map(|member| (member.id.as_str().to_string(), member.version.clone()))
        .collect();

    // The unmaterialization check: each row still holds a registered copy, and the copy's
    // checksum still equals the record (`ATH-SKILL-018`'s authority face).
    let mut prepared: Vec<_> = pins
        .iter()
        .map(|(id, version)| {
            let row: Skill = SkillStore::read(store, &SkillId::new(id.clone()), version)
                .map_err(LoaderFault::Io)?
                .ok_or_else(|| LoaderFault::RegisteredCopyMissing {
                    id: id.clone(),
                    version: version.clone(),
                })?;
            let source = registered_dir.join(id).join(version).join("SKILL.md");
            let material = std::fs::read_to_string(&source).map_err(|error| {
                LoaderFault::RegisteredCopyMissing {
                    id: id.clone(),
                    version: version.clone(),
                }
                .checked_by(error)
            })?;
            let checksum = compact(&package_checksum(&material));
            if checksum != row.checksum && !row.checksum.is_empty() {
                // The registered row's checksum is the fact; a disagreeing body means the
                // registered area was tampered with — surface, never overwrite.
                return Err(LoaderFault::RegisteredCopyMissing {
                    id: id.clone(),
                    version: version.clone(),
                });
            }
            // The absolute SKILL.md path that reaches into the Agent Instance's workspace.
            let skill_md_path = workspace.join("skills").join(id).join("SKILL.md");
            Ok((row.clone(), material, skill_md_path.display().to_string()))
        })
        .collect::<Result<Vec<_>, LoaderFault>>()?;

    // Everything verified: write the copies.
    let mut placed = Vec::with_capacity(prepared.len());
    prepared.sort_by(|a, b| a.0.id.as_str().cmp(b.0.id.as_str()));
    for (row, material, skill_md_path) in prepared {
        let dir = workspace.join("skills").join(row.id.as_str());
        std::fs::create_dir_all(&dir).map_err(|error| LoaderFault::Io(error.to_string()))?;
        std::fs::write(dir.join("SKILL.md"), material.clone().into_bytes())
            .map_err(|error| LoaderFault::Io(error.to_string()))?;
        placed.push(Placement {
            skill_id: row.id.clone(),
            version: row.version.clone(),
            checksum: row.checksum.clone(),
            skill_md_path,
        });
    }
    Ok(placed)
}

fn package_checksum(material: &str) -> String {
    let mut hasher = sha2::Sha256::new();
    use sha2::Digest as _;
    hasher.update(material.as_bytes());
    let digest = hasher.finalize();
    digest.iter().fold(String::new(), |mut text, byte| {
        use std::fmt::Write as _;
        let _ = write!(text, "{byte:02x}");
        text
    })
}

fn compact(hex: &str) -> String {
    format!("sha256:{hex}")
}

trait Checked {
    fn checked_by(self, error: std::io::Error) -> Self;
}

impl Checked for LoaderFault {
    fn checked_by(self, _error: std::io::Error) -> Self {
        self
    }
}
