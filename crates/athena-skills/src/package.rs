//! The Skill Package model (spec §12.2): the on-disk package — `SKILL.md` (the material
//! the Agent reads) plus `manifest.yaml` (the machine-managed metadata) — and the
//! **Catalog record** built from it: all fifteen §12.3 fields, exactly (`ATH-SKILL-005`).
//!
//! The Core Agent reads only the Catalog record, never the package body (`ATH-SKILL-002`);
//! the Validator (P5-02) is what turns a package into a record with its provenance checked.

use crate::manifest::{Manifest, ManifestFault};
use athena_types::{Skill, SkillState, Timestamp};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Why a package was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PackageFault {
    /// The directory does not exist.
    #[error("skill package {0:?} does not exist")]
    MissingRoot(String),
    /// `SKILL.md` absent — the Agent-readable material is what a Skill *is* (ATH-SKILL-003).
    #[error("the package carries no SKILL.md")]
    MissingSkillMd,
    /// `manifest.yaml` absent — no metadata, no Catalog entry (ATH-SKILL-003, ATH-SKILL-005).
    #[error("the package carries no manifest.yaml")]
    MissingManifest,
    /// Reading a file failed.
    #[error("reading {0:?} failed: {1}")]
    Io(String, String),
    /// The manifest did not parse / validate.
    #[error(transparent)]
    Manifest(#[from] ManifestFault),
}

/// One Skill Package on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// The package root.
    pub root: PathBuf,
    /// The `SKILL.md` body (the Agent-readable material).
    pub skill_md: String,
    /// The parsed manifest.
    pub manifest: Manifest,
}

impl Package {
    /// Open a package directory. `SKILL.md` and `manifest.yaml` must both exist
    /// (`ATH-SKILL-003`); anything else is not a Skill Package, whatever else it carries.
    ///
    /// # Errors
    /// [`PackageFault`] naming the first missing piece.
    pub fn open(root: &Path) -> Result<Self, PackageFault> {
        if !root.is_dir() {
            return Err(PackageFault::MissingRoot(root.display().to_string()));
        }
        let skill_md = read_file(&root.join("SKILL.md"))
            .map_err(|fault| match fault {
                PackageFault::MissingSkillMd => PackageFault::MissingSkillMd,
                other => other,
            })?
            .1;
        let (_, manifest_text) =
            read_file(&root.join("manifest.yaml")).map_err(|fault| match fault {
                PackageFault::MissingManifest => PackageFault::MissingManifest,
                other => other,
            })?;
        Ok(Self {
            root: root.to_path_buf(),
            skill_md,
            manifest: Manifest::parse(&manifest_text)?,
        })
    }

    /// The content checksum (`ATH-SKILL-035`/`ATH-SKILL-036`'s verified quantity):
    /// SHA-256 over the `SKILL.md` body — the Agent-readable material, exactly the
    /// thing the manifest *cannot* cover (a manifest that hashed itself would change
    /// its own digest when declaring it) — in the Catalog's `sha256:` form. The
    /// manifest's declared checksum, when present, must equal this — the Validator
    /// (P5-02) refuses mismatches.
    ///
    /// # Errors
    /// [`PackageFault::MissingSkillMd`] / [`PackageFault::MissingManifest`] when the
    /// package was constructed through a path that left a file unread.
    pub fn content_checksum(&self) -> Result<String, PackageFault> {
        let (_, skill_md) = read_file_raw(&self.root.join("SKILL.md"))?;
        let mut hasher = Sha256::new();
        hasher.update(skill_md.as_bytes());
        let digest = hasher.finalize();
        Ok(format!(
            "sha256:{}",
            digest.iter().fold(String::new(), |mut text, byte| {
                use std::fmt::Write as _;
                let _ = write!(text, "{byte:02x}");
                text
            })
        ))
    }

    /// Build the Catalog record from this package: all fifteen §12.3 fields
    /// (`ATH-SKILL-005`), with the lifecycle state supplied by the Registry side
    /// (`ATH-STATE-026` — state is `(id, version)`-managed, never manifest content).
    /// A manifest checksum of `None` takes the computed content checksum.
    ///
    /// # Errors
    /// [`PackageFault::Manifest`] when a declared tool or permission is not a known
    /// §11 tool / §26.1 permission shape.
    pub fn to_catalog_record(
        &self,
        state: SkillState,
        registered_at: Timestamp,
        updated_at: Timestamp,
    ) -> Result<Skill, PackageFault> {
        let manifest = &self.manifest;
        Ok(Skill {
            id: athena_types::SkillId::new(manifest.id.clone()),
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            description: manifest.description.clone(),
            capabilities: manifest.capabilities.clone(),
            required_tools: self.manifest.tool_names()?,
            required_permissions: self.manifest.permissions()?,
            supported_agents: manifest
                .supported_agents
                .iter()
                .map(|slug| athena_types::AgentId::new(slug.clone()))
                .collect(),
            dependencies: manifest
                .dependencies
                .iter()
                .map(|dependency| athena_types::SkillRef {
                    id: athena_types::SkillId::new(dependency.id.clone()),
                    version: dependency.version.clone(),
                    checksum: None,
                })
                .collect(),
            runtime_provider: manifest.runtime.provider.clone(),
            source: manifest.source.clone(),
            checksum: manifest
                .checksum
                .clone()
                .unwrap_or_else(|| self.content_checksum().unwrap_or_default()),
            publisher: manifest.publisher.clone(),
            risk_level: risk_of(&manifest.risk.level),
            state,
            hot_reload_safe: manifest.hot_reload_safe,
            registered_at,
            updated_at,
        })
    }
}

fn read_file(path: &Path) -> Result<(Vec<u8>, String), PackageFault> {
    let (bytes, text) = read_file_raw(path)?;
    Ok((bytes, text))
}

fn read_file_raw(path: &Path) -> Result<(Vec<u8>, String), PackageFault> {
    let name = path.display().to_string();
    let bytes = std::fs::read(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => match path.file_name().and_then(|f| f.to_str()) {
            Some("SKILL.md") | Some("skill.md") => PackageFault::MissingSkillMd,
            Some("manifest.yaml") => PackageFault::MissingManifest,
            _ => PackageFault::Io(name.clone(), "not found".into()),
        },
        _ => PackageFault::Io(name.clone(), error.to_string()),
    })?;
    let text = String::from_utf8_lossy(&bytes).to_string();
    Ok((bytes, text))
}

fn risk_of(level: &str) -> athena_types::RiskLevel {
    athena_types::RiskLevel::ALL
        .iter()
        .copied()
        .find(|risk| risk.as_str() == level)
        .unwrap_or(athena_types::RiskLevel::Medium)
}

#[cfg(test)]
mod package_tests;
