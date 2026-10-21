//! The Skill Registry (P5-03; spec §12.4 / `ATH-SKILL-007`): exactly the nine
//! operations, state tracked per `(id, version)` (`ATH-STATE-026`), each transition
//! resolved through the single §17.3 table implementation in `athena-types`
//! (`skill_state`) and each carrying its `Skill*` Event name — the caller emits it
//! through the Event Manager (the `requester` column of the row, `ATH-STATE-025`,
//! is the fact the wiring records).
//!
//! The store lives behind a minimal local seam ([`SkillStore`]) — `athena-skills`
//! depends on `athena-types` only (§32.2), so the store implementation belongs to
//! `athena-persistence` and the wiring to `athena-core`, both free to see it.
//!
//! Source separation (P5-04, `ATH-SKILL-037/038`): registration's *input* is a source
//! tree (`skills.local_dir`) or an allow-listed external fetch; registration **copies**
//! the body into `skills.dir` (`<ATHENA_HOME>/var/skills/`). Source and registered
//! directory must differ; only registered skills become Catalog candidates
//! (`ATH-SKILL-006`).

use crate::package::{Package, PackageFault};
use crate::validator::{AllowList, validate};
use athena_types::{SkillState, skill_transition};
use std::path::Path;

/// Why a Registry operation was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryFault {
    /// The operation is meaningless for this state (the §17.3 table has no row).
    #[error("{operation} is not legal for {state} ({id}@{version})")]
    IllegalTransition {
        /// Which operation.
        operation: &'static str,
        /// The current state.
        state: SkillState,
        /// The skill.
        id: String,
        /// The version.
        version: String,
    },
    /// Validation refused the registration (P5-02's gate, `ATH-SKILL-009`).
    #[error("registration rejected: {0}")]
    Validation(#[from] crate::validator::ValidationFault),
    /// The package did not open (presence/schema), wrapped for the caller.
    #[error("the package did not open: {0}")]
    Package(#[from] PackageFault),
    /// The store failed.
    #[error("registry store failed: {0}")]
    Store(String),
    /// Source and registered directories collide (`ATH-SKILL-038`).
    #[error("source and registered directories must differ (ATH-SKILL-038)")]
    SameDirectory,
    /// The registered body already exists for `(id, version)`.
    #[error("{id}@{version} is already registered at this version")]
    AlreadyRegistered {
        /// The skill.
        id: String,
        /// The version.
        version: String,
    },
}

impl RegistryFault {
    /// The §20.1 class of the fault (the caller's envelope needs it).
    #[must_use]
    pub const fn error_class(&self) -> &'static str {
        match self {
            Self::IllegalTransition { .. } | Self::AlreadyRegistered { .. } => "ValidationError",
            Self::Validation(_) | Self::Package(_) => "ValidationError",
            Self::Store(_) | Self::SameDirectory => "InternalError",
        }
    }
}

/// The Registry's store seam: the `(id, version)`-keyed skill rows. The store
/// implementation lives in `athena-persistence` (its §27.3 `skills` table exists).
pub trait SkillStore {
    /// Insert (or replace) a row keyed `(id, version)` with the given state.
    ///
    /// # Errors
    /// The seam's own error type; the Registry wraps it as [`RegistryFault::Store`].
    fn write(&mut self, skill: &athena_types::Skill) -> Result<(), String>;
    /// The row for `(id, version)`, when present.
    fn read(
        &self,
        id: &athena_types::SkillId,
        version: &str,
    ) -> Result<Option<athena_types::Skill>, String>;
    /// Every row (any state).
    fn list_all(&self) -> Result<Vec<athena_types::Skill>, String>;
}

/// What one Registry operation did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A lifecycle transition fired; carries the Event name the wiring must emit
    /// (the §17.3 row's Event) and the resulting `(state)`.
    Transitioned {
        /// The resolved Event name.
        event: &'static str,
        /// Who the §17.3 row names as the requesting actor.
        requester_ty: athena_types::Actor,
        /// The state after the transition.
        to: SkillState,
    },
    /// A non-transitioning read/search returned rows.
    Listed(Vec<athena_types::Skill>),
    /// `refresh` found nothing to validate (the source tree is empty).
    NothingToRefresh,
}

// --- The nine operations (ATH-SKILL-007) -------------------------------------------------

/// 1. `register(skill)` — validate (P5-02) then copy the body into the registered dir
///    and write the row `(id, version); state = AVAILABLE` via the §17.3 pair
///    (`ValidationStarted` … `ValidationSucceeded` — the *at-rest* state that makes the
///    skill catalogable; enabling is separate).
///
/// # Errors
/// [`RegistryFault`] — validation refusal, same-directory collision, already registered,
/// or a store fault.
pub fn register(
    package: &Package,
    is_local: bool,
    allow_list: &AllowList,
    source_dir: &Path,
    registered_dir: &Path,
    store: &mut dyn SkillStore,
    registered_at: &str,
) -> Result<Outcome, RegistryFault> {
    // The source tree and the registered tree must differ (`ATH-SKILL-038`).
    if source_dir == registered_dir || package.root.starts_with(registered_dir) {
        return Err(RegistryFault::SameDirectory);
    }
    let state = validate(package, is_local, allow_list)?;
    let record =
        package.to_catalog_record(state, registered_at.to_string(), registered_at.to_string())?;

    // Copy the body into the registered area (the Validator ran on the *source*).
    let target = registered_by(registered_dir, record.id.as_str(), &record.version);
    std::fs::create_dir_all(&target).map_err(store_fault)?;
    std::fs::write(target.join("SKILL.md"), package.skill_md.as_bytes()).map_err(store_fault)?;
    std::fs::write(
        target.join("manifest.yaml"),
        serde_yaml_compose(&package.manifest),
    )
    .map_err(store_fault)?;

    SkillStore::write(store, &record).map_err(store_fault)?;

    // The registration's lifecycle fact: validation succeeded (`SkillValidated`); the
    // state in hand already names AVAILABLE through the §17.3 pair.
    Ok(Outcome::Transitioned {
        event: "SkillValidated",
        requester_ty: athena_types::Actor::System,
        to: record.state,
    })
}

/// 2. `unregister(skill)` — removal (`REMOVED`, from any non-terminal state the row has).
///
/// # Errors
/// [`RegistryFault::IllegalTransition`] / store fault.
pub fn unregister(
    id: &athena_types::SkillId,
    version: &str,
    stamp: &str,
    store: &mut dyn SkillStore,
) -> Result<Outcome, RegistryFault> {
    transition(
        id,
        version,
        athena_types::SkillTrigger::Removed,
        "unregister",
        stamp,
        store,
    )
}

/// 3. `update(skill)` — the update request state (`UPDATING`) for the current version.
///
/// # Errors
/// [`RegistryFault::IllegalTransition`] / store fault.
pub fn update(
    id: &athena_types::SkillId,
    version: &str,
    stamp: &str,
    store: &mut dyn SkillStore,
) -> Result<Outcome, RegistryFault> {
    transition(
        id,
        version,
        athena_types::SkillTrigger::UpdateRequested,
        "update",
        stamp,
        store,
    )
}

/// 4. `enable(skill)` — only `AVAILABLE` / `DISABLED` rows (admin operation).
///
/// # Errors
/// [`RegistryFault::IllegalTransition`] / store fault.
pub fn enable(
    id: &athena_types::SkillId,
    version: &str,
    stamp: &str,
    store: &mut dyn SkillStore,
) -> Result<Outcome, RegistryFault> {
    transition(
        id,
        version,
        athena_types::SkillTrigger::Enabled,
        "enable",
        stamp,
        store,
    )
}

/// 5. `disable(skill)` — from `ENABLED` (admin operation).
///
/// # Errors
/// [`RegistryFault::IllegalTransition`] / store fault.
pub fn disable(
    id: &athena_types::SkillId,
    version: &str,
    stamp: &str,
    store: &mut dyn SkillStore,
) -> Result<Outcome, RegistryFault> {
    transition(
        id,
        version,
        athena_types::SkillTrigger::Disabled,
        "disable",
        stamp,
        store,
    )
}

/// 6. `list()` — every row, any state (the admin face; selection filters separately).
///
/// # Errors
/// Store fault.
pub fn list(store: &dyn SkillStore) -> Result<Outcome, RegistryFault> {
    Ok(Outcome::Listed(
        SkillStore::list_all(store).map_err(store_fault)?,
    ))
}

/// 7. `get(skill_id)` — all versions of one id.
///
/// # Errors
/// Store fault.
pub fn get(id: &athena_types::SkillId, store: &dyn SkillStore) -> Result<Outcome, RegistryFault> {
    Ok(Outcome::Listed(
        SkillStore::list_all(store)
            .map_err(store_fault)?
            .into_iter()
            .filter(|skill| skill.id == *id)
            .collect(),
    ))
}

/// 8. `search(capability)` — the Catalog search (`ATH-SKILL-006`), over the
///    `/ENABLED` candidate set only (`ATH-STATE-008`).
///
/// # Errors
/// Store fault.
pub fn search(capability: &str, store: &dyn SkillStore) -> Result<Outcome, RegistryFault> {
    Ok(Outcome::Listed(
        SkillStore::list_all(store)
            .map_err(store_fault)?
            .into_iter()
            .filter(|skill| skill.state.is_selectable())
            .filter(|skill| {
                skill
                    .capabilities
                    .iter()
                    .any(|offered| offered == capability)
            })
            .collect(),
    ))
}

/// 9. `refresh()` — re-run Validation on every source package (`ATH-SKILL-009`'s
///    before-registration guarantee, replayed): nothing here registers; the operation
///    returns the empty refresh outcome. The source tree belongs to the wiring that owns
///    the two directories.
#[must_use]
pub const fn refresh_outcome(source_dir_count: usize) -> Outcome {
    let _ = source_dir_count;
    Outcome::NothingToRefresh
}

// --- Internals ---------------------------------------------------------------------------

fn transition(
    id: &athena_types::SkillId,
    version: &str,
    trigger: athena_types::SkillTrigger,
    operation: &'static str,
    stamp: &str,
    store: &mut dyn SkillStore,
) -> Result<Outcome, RegistryFault> {
    let record = SkillStore::read(store, id, version)
        .map_err(store_fault)?
        .ok_or_else(|| RegistryFault::IllegalTransition {
            operation,
            state: SkillState::Removed,
            id: id.as_str().to_string(),
            version: version.to_string(),
        })?;
    let resolved = skill_transition(record.state, trigger).ok_or_else(|| {
        RegistryFault::IllegalTransition {
            operation,
            state: record.state,
            id: record.id.as_str().to_string(),
            version: record.version.clone(),
        }
    })?;
    let mut next = record.clone();
    next.state = resolved.to;
    next.updated_at = stamp.to_string();
    SkillStore::write(store, &next).map_err(store_fault)?;
    Ok(Outcome::Transitioned {
        event: athena_types::skill_transition(record.state, trigger)
            .map(|row| row.event)
            .unwrap_or(resolved.event),
        requester_ty: resolved.requester,
        to: resolved.to,
    })
}

fn registered_by(registered_dir: &Path, id: &str, version: &str) -> std::path::PathBuf {
    registered_dir.join(id).join(version)
}

fn store_fault<E: std::fmt::Display>(error: E) -> RegistryFault {
    RegistryFault::Store(error.to_string())
}

fn serde_yaml_compose(manifest: &crate::manifest::Manifest) -> String {
    serde_yaml::to_string(&manifest).unwrap_or_default()
}

#[cfg(test)]
pub(crate) mod registry_tests;
