//! The Skill Validator (P5-02; spec §12.9 / `ATH-SKILL-009`, `ATH-SKILL-022`,
//! `ATH-SKILL-035`, `ATH-SKILL-036`): the gate a Skill Package must pass **before**
//! registration. A package that fails here never becomes a Catalog candidate.
//!
//! The checks, in spec order:
//!
//! 1. **Presence** — both required files exist (`ATH-SKILL-003`; P5-01's `Package::open`
//!    already refuses otherwise).
//! 2. **Schema** — the manifest parsed and validated structurally (P5-01's strict
//!    shape: every required field, SemVer version, known tools, well-shaped deps).
//! 3. **Checksum** — when the manifest *declares* one, it must equal the computed
//!    content checksum (`ATH-SKILL-022` for external skills; `skills.verify_checksum`
//!    gates the check).
//! 4. **Publisher / source allow-list** — an *external* skill (non-local origin)
//!    requires both its publisher and source on the allow-list; the local in-project
//!    `skills.local_dir` packages skip the list, per `ATH-SKILL-037` vs `ATH-SKILL-036`
//!    (external-only rule). **An empty allow-list denies every external fetch** — the
//!    safe v1 starting point, and public-key-signature-free acceptance is not
//!    implemented at all (`ATH-SKILL-036` MUST NOT until the spec is revised).

use crate::package::{Package, PackageFault};
use athena_types::SkillState;

/// Why validation was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationFault {
    /// The package did not open (presence + schema layers).
    #[error(transparent)]
    Package(#[from] PackageFault),
    /// The declared checksum does not match the computed content checksum.
    #[error("the declared checksum {declared} does not match the computed {computed}")]
    ChecksumMismatch {
        /// What the manifest declared.
        declared: String,
        /// What the content hashes to.
        computed: String,
    },
    /// The checksum should have been verified but could not be computed.
    #[error("the content checksum failed: {0}")]
    ChecksumFailed(String),
    /// The publisher is not on the allow-list (external skills, `ATH-SKILL-036`).
    #[error("publisher {0:?} is not on the allow-list")]
    PublisherNotAllowed(String),
    /// The source is not on the allow-list (external skills, `ATH-SKILL-036`).
    #[error("source {0:?} is not on the allow-list")]
    SourceNotAllowed(String),
}

/// The validator's configuration, drawn from `skills.*` (§28.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowList {
    /// Publishers whose externally fetched skills may register.
    pub publishers: Vec<String>,
    /// Origins from which skills may be fetched.
    pub sources: Vec<String>,
    /// Whether the checksum must be verified (config `skills.verify_checksum`, default true).
    pub verify_checksum: bool,
}

impl AllowList {
    /// Build from configuration values (the wiring maps `skills.allowed_publishers` /
    /// `skills.allowed_sources` / `skills.verify_checksum` over — `athena-skills`
    /// depends on `athena-types` only, §32.2).
    #[must_use]
    pub fn new(publishers: Vec<String>, sources: Vec<String>, verify_checksum: bool) -> Self {
        Self {
            publishers,
            sources,
            verify_checksum,
        }
    }

    /// Whether this publisher may register (exact-or-suffix-entity match, case-stable).
    fn publisher_allowed(&self, publisher: &str) -> bool {
        self.publishers
            .iter()
            .any(|allowed| allowed == publisher || allowed == "*")
    }

    /// Whether this source may be fetched (exact match, or a directory/domain prefix).
    fn source_allowed(&self, source: &str) -> bool {
        self.sources.iter().any(|allowed| {
            allowed == source || allowed == "*" || source.starts_with(allowed.as_str())
        })
    }
}

/// The register gate (P5-02): run every check, return the record state
/// (`VALIDATING → AVAILABLE` on success — the caller's Registry write names it) or the
/// refusal that lands the package `FAILED` (`ATH-SKILL-009`: the failure *is* the
/// lifecycle Event's cause, Core never retries registration on its own).
///
/// `is_local` marks the *in-project* source (`skills.local_dir` tree): local skills skip
/// the publisher/source allow-list (`ATH-SKILL-037`), but the checksum check still holds
/// when configured — a corrupted local fetch is exactly what a checksum protects against.
///
/// # Errors
/// [`ValidationFault`] with the first failing check, in spec order.
pub fn validate(
    package: &Package,
    is_local: bool,
    allow_list: &AllowList,
) -> Result<SkillState, ValidationFault> {
    // 1. Presence + 2. Schema: `Package::open` already enforced both — a package reference
    //    in hand means they passed. The decision point lives where the files are read —
    //    there is no schema re-check here to duplicate, only to name the layer.

    // 3. Checksum (`ATH-SKILL-022` / `skills.verify_checksum`).
    let declared = package.manifest.checksum.clone();
    if allow_list.verify_checksum {
        let computed = package
            .content_checksum()
            .map_err(|fault| ValidationFault::ChecksumFailed(fault.to_string()))?;
        if let Some(declared) = declared.filter(|declared| *declared != computed) {
            return Err(ValidationFault::ChecksumMismatch { declared, computed });
        }
    }

    // 4. Publisher / source allow-list — external skills only (`ATH-SKILL-036`).
    if !is_local {
        if !allow_list.publisher_allowed(&package.manifest.publisher) {
            return Err(ValidationFault::PublisherNotAllowed(
                package.manifest.publisher.clone(),
            ));
        }
        if !allow_list.source_allowed(&package.manifest.source) {
            return Err(ValidationFault::SourceNotAllowed(
                package.manifest.source.clone(),
            ));
        }
    }

    // All checks passed: the package may register as `AVAILABLE`
    // (the Registry decides the enable step, `ATH-STATE-008`).
    Ok(SkillState::Available)
}

#[cfg(test)]
mod validator_tests;

#[allow(dead_code)]
fn set_failed() {}

impl Default for AllowList {
    /// The config defaults' face: empty lists (deny every external fetch) and the
    /// checksum check **on** (its spec default, `ATH-SKILL-022`).
    fn default() -> Self {
        Self {
            publishers: Vec::new(),
            sources: Vec::new(),
            verify_checksum: true,
        }
    }
}
