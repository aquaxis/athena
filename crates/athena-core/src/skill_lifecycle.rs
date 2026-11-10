//! The §12.5 async-half semantics (X-35): the Skill Manager's completion legs for
//! `UPDATING` rows and the Skill Validator's resolution legs for `VALIDATING` rows,
//! in one testable module so the resident cadence (`athena-api`) stays free of
//! §17.3 state-machine knowledge (the same cadence/semantics split D-35 keeps).
//!
//! The defect X-35 closed: the §22.7 `update` / `validate` faces record their request
//! (202) and leave the row in-flight (`UPDATING` / `VALIDATING`), but no Runtime actor
//! owned the completion — `D-39` had already fixed that the API may never re-register
//! over a pin in-flight, and no other verb walks an outbound edge from `UPDATING` /
//! `VALIDATING`, so a request-only settlement rested rows forever (false progress,
//! §17.3's actors assigned the legs to the Skill Manager / the Skill Validator).
//!
//! The v1 completion faces, honestly bounded (`never-invent`):
//!
//! - **The Skill Manager's leg (an `UPDATING` row)** — v1 has no network fetcher
//!   (`ATH-SKILL-036`: no public-key-signed external acceptance; the allow-list
//!   defaults to deny-all external fetches), so "acquisition" cannot mean a download.
//!   The pin's material is located where v1 actually keeps it: the in-project source
//!   tree (`skills.local_dir`, when its manifest still pins the row's version) or the
//!   registered copy (`skills.dir/<id>/<version>/`, the registration's own body). The
//!   located package re-runs the P5-02 disk gate (presence + schema reached through
//!   `Package::open`, checksum per `ATH-SKILL-022`, the verified quantity `ATH-SKILL-035`)
//!   and the §17.3 completion edges walk from there: 更新完了 → `SkillUpdated`
//!   (`UPDATING → AVAILABLE`, the Skill Manager's own completion face — the row's
//!   checksum record refreshes to the computed content checksum, and a source-located
//!   body re-syncs the registered copy, exactly `registry::register`'s copy face);
//!   取得失敗 / 検証失敗 → `SkillValidationFailed` (`UPDATING → FAILED`). The
//!   `UPDATING → VALIDATING` interstice (`SkillValidating`, 新版取得完了) is *not*
//!   exercised: v1's acquisition and gate are one synchronous disk operation with no
//!   download window, so an in-between rest state would be fabricated — the row
//!   completes in one leg with one §17.3 completion Event.
//! - **The Skill Validator's leg (a `VALIDATING` row)** — the revalidation request
//!   (`FAILED → VALIDATING`, 202) and any row found in-flight resolves the same way:
//!   locate the pin's material, re-open it, re-run the gate → 検証成功
//!   `SkillValidated` (`VALIDATING → AVAILABLE`) / 検証失敗 `SkillValidationFailed`
//!   (`VALIDATING → FAILED`).
//!
//! The allow-list face does **not** re-apply here (`is_local = true` for the gate):
//! the rows validated were registered through the instance's own admin faces (the API's
//! manifest face, X-32), and the publisher/source list governs *external fetches*
//! (`ATH-SKILL-035`/`036`) — a fetch the async half never performs. Re-classifying an
//! already-registered pin as external at completion time would fail rows whose
//! registration face the shipped API never gated; the list stays enforced where fetch
//! decisions happen (`registry::register`). This scoping is the D-42 decision.
//!
//! Events ride the shipped §17.3 edge-walk payload face (the X-32 precedent —
//! `id`/`version`/`from`/`to`/`operation`), `actor: system` (§23.1; the §17.3 table's
//! own rows assign the completion legs to the Skill Manager / the Skill Validator,
//! whose catalog actor is `system` — `ATH-STATE-025` keeps Core Agent out of
//! lifecycle requests and `skill_state.rs`'s table never names `core-agent` here).

use athena_skills::package::Package;
use athena_skills::validator::{AllowList, validate};
use athena_types::{EventId, SkillId, SkillState};
use std::path::{Path, PathBuf};

use crate::{EventRepository, NewEvent, SkillRepository};
use athena_persistence::AthenaStore;

/// One cadence tick's outcome: how many in-flight rows resolved, in counts (facts, not
/// prose — the §32.1 log face renders the counts and the fault lines).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LifecycleReport {
    /// `UPDATING` rows the Skill Manager's leg completed (`SkillUpdated`).
    pub updated: u64,
    /// `UPDATING` rows landed `FAILED` (取得失敗 / 検証失敗).
    pub update_failures: u64,
    /// `VALIDATING` rows the Skill Validator resolved (`SkillValidated`).
    pub validated: u64,
    /// `VALIDATING` rows landed `FAILED` (検証失敗).
    pub validation_failures: u64,
    /// The failure reasons, one line per failed row (the log face's prose).
    pub faults: Vec<String>,
}

impl LifecycleReport {
    /// Whether the tick touched any row at all.
    #[must_use]
    pub fn is_quiet(&self) -> bool {
        self.updated + self.update_failures + self.validated + self.validation_failures == 0
    }
}

/// Why one cadence tick refused to run at all (row-level failures ride the report).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LifecycleFault {
    /// The store refused; surfaced whole, never renamed.
    #[error("store fault: {0}")]
    Store(String),
}

/// Run one completion cycle at a single `now` stamp: the Skill Manager's leg over the
/// `UPDATING` rows, then the Skill Validator's leg over the `VALIDATING` rows.
///
/// `skills_local_dir` is the in-project source tree (`skills.local_dir`),
/// `skills_registered_dir` the registered tree (`skills.dir`); `verify_checksum` maps
/// over from `skills.verify_checksum`. Rows walk in `(id, version)` order for
/// deterministic logs and event sequences.
///
/// # Errors
/// [`LifecycleFault::Store`] — the store refused a read or a write; the tick reports
/// the fault and the cadence retries (a faulted tick never halts the resident).
pub fn run_once(
    store: &mut AthenaStore,
    skills_local_dir: &Path,
    skills_registered_dir: &Path,
    verify_checksum: bool,
    now: &str,
    correlation: &str,
) -> Result<LifecycleReport, LifecycleFault> {
    let allow = AllowList::new(Vec::new(), Vec::new(), verify_checksum);
    let mut report = LifecycleReport::default();

    let mut updating = SkillRepository::list_by_state(store, SkillState::Updating)
        .map_err(|error| LifecycleFault::Store(error.to_string()))?;
    updating.sort_by(|a, b| {
        a.id.as_str()
            .cmp(b.id.as_str())
            .then(a.version.cmp(&b.version))
    });
    for row in updating {
        let (completed, fault_line) = manager_leg(
            store,
            &row,
            skills_local_dir,
            skills_registered_dir,
            &allow,
            now,
            correlation,
        );
        if completed {
            report.updated += 1;
        } else {
            report.update_failures += 1;
        }
        if let Some(line) = fault_line {
            report.faults.push(line);
        }
    }

    let mut validating = SkillRepository::list_by_state(store, SkillState::Validating)
        .map_err(|error| LifecycleFault::Store(error.to_string()))?;
    validating.sort_by(|a, b| {
        a.id.as_str()
            .cmp(b.id.as_str())
            .then(a.version.cmp(&b.version))
    });
    for row in validating {
        let (completed, fault_line) = validator_leg(
            store,
            &row,
            skills_local_dir,
            skills_registered_dir,
            &allow,
            now,
            correlation,
        );
        if completed {
            report.validated += 1;
        } else {
            report.validation_failures += 1;
        }
        if let Some(line) = fault_line {
            report.faults.push(line);
        }
    }

    Ok(report)
}

/// Where the pin's material was found, and which completion face follows from it.
enum Located {
    /// The in-project source tree still pins this version: on success the Skill
    /// Manager re-syncs the registered copy (the update's material face).
    Source(PathBuf),
    /// Only the registered copy holds the pin: on success nothing re-syncs (the copy
    /// is already the material), the gate re-affirms it.
    Registered(PathBuf),
}

/// Locate the pinned `(id, version)` material across v1's two trees. Order: the source
/// tree first (whose manifest must still pin the row's version — a tree that moved on
/// past the pin leaves the pin unlocated here), then the registered copy. Nothing is
/// invented: no candidate opens → unlocated, and the caller walks the 取得失敗 edge.
fn locate_pin(
    local_dir: &Path,
    registered_dir: &Path,
    id: &str,
    version: &str,
) -> Result<Located, String> {
    // Candidate 1: the source tree (`<local_dir>/<id>/`) — its manifest names the
    // version it carries; only an exact match counts as the pin's material.
    let source_root = local_dir.join(id);
    if source_root.is_dir() {
        match Package::open(&source_root) {
            Ok(package) if package.manifest.version == version => {
                return Ok(Located::Source(source_root));
            }
            // A different version (the tree moved on), a malformed manifest, or a
            // missing file — fall through to the registered copy.
            Ok(_) | Err(_) => {}
        }
    }
    // Candidate 2: the registered copy (`<registered_dir>/<id>/<version>/`) — the
    // registration's own body, whose layout is the pin itself.
    let registered_root = registered_dir.join(id).join(version);
    if registered_root.is_dir() {
        match Package::open(&registered_root) {
            Ok(_) => return Ok(Located::Registered(registered_root)),
            Err(fault) => {
                return Err(format!(
                    "the registered copy at {} does not open: {fault}",
                    registered_root.display()
                ));
            }
        }
    }
    Err(format!(
        "the pin is unlocatable (no source-tree manifest pins version {version}; no registered copy)"
    ))
}

/// The P5-02 disk gate over the located material (`ATH-SKILL-009`'s before-selection
/// guarantee, replayed for a pin already in flight): reopen the package (presence +
/// schema), verify the checksum when configured (`ATH-SKILL-022`), return the computed
/// content checksum (`ATH-SKILL-035`'s verified quantity) on success.
///
/// The allow-list face stays off (`is_local = true`): the D-42 scoping — the async half
/// re-validates pins the instance's own registration faces already accepted; the
/// publisher/source list governs external fetches, which v1 never performs here.
fn gate(located_root: &Path, allow: &AllowList) -> Result<String, String> {
    let package = Package::open(located_root).map_err(|fault| fault.to_string())?;
    match validate(&package, true, allow) {
        Ok(_) => package
            .content_checksum()
            .map_err(|fault| fault.to_string()),
        Err(fault) => Err(fault.to_string()),
    }
}

/// Copy the two package files byte-faithfully from a source root into the registered
/// tree (`registry::register`'s copy face, replayed by the update's completion).
fn sync_registered_copy(
    source_root: &Path,
    registered_dir: &Path,
    id: &str,
    version: &str,
) -> Result<(), String> {
    let target = registered_dir.join(id).join(version);
    std::fs::create_dir_all(&target).map_err(|error| error.to_string())?;
    for file in ["SKILL.md", "manifest.yaml"] {
        let bytes =
            std::fs::read(source_root.join(file)).map_err(|error| format!("{file}: {error}"))?;
        std::fs::write(target.join(file), bytes).map_err(|error| format!("{file}: {error}"))?;
    }
    Ok(())
}

/// Walk one row's state update + §17.3 completion Event under one store access (D-1's
/// transition+Event window): the row write and the Event append ride the same open
/// store; a store fault aborts the tick through the caller.
///
/// The checksum recorded on a *success* face (`to == AVAILABLE`) is the just-computed
/// content checksum — the verified quantity (`ATH-SKILL-035`), exactly what the row's
/// record should name after its material passed the gate. A failure face keeps the
/// record's previous checksum: nothing new was verified.
#[allow(clippy::too_many_arguments)]
fn walk_completion(
    store: &mut AthenaStore,
    id: &SkillId,
    version: &str,
    from: SkillState,
    to: SkillState,
    operation: &str,
    checksum: &str,
    now: &str,
    correlation: &str,
) -> Result<(), LifecycleFault> {
    let mut record = SkillRepository::get(store, id, version)
        .map_err(|error| LifecycleFault::Store(error.to_string()))?
        .ok_or_else(|| LifecycleFault::Store(format!("row {id}@{version} vanished mid-leg")))?;
    record.state = to;
    record.updated_at = now.to_string();
    // The record refresh: the state, the last-touch stamp, and — on any success face —
    // the checksum field carries the computed content checksum it just verified.
    let event_type: &str = match to {
        SkillState::Available => {
            record.checksum = checksum.to_string();
            if from == SkillState::Updating {
                "SkillUpdated"
            } else {
                "SkillValidated"
            }
        }
        _ => "SkillValidationFailed",
    };
    SkillRepository::upsert(store, &record)
        .map_err(|error| LifecycleFault::Store(error.to_string()))?;
    let event = NewEvent {
        event_id: EventId::new(),
        schema_version: 1,
        event_type: event_type.to_string(),
        occurred_at: now.to_string(),
        task_id: None,
        plan_step_id: None,
        agent_id: None,
        instance_id: None,
        skill_id: Some(id.clone()),
        correlation_id: correlation.to_string(),
        causation_id: None,
        actor: "system".into(),
        payload_json: serde_json::json!({
            "id": id.as_str(),
            "version": version,
            "from": from.as_str(),
            "to": to.as_str(),
            "operation": operation,
            "checksum": checksum,
        })
        .to_string(),
    };
    EventRepository::append(store, &event)
        .map_err(|error| LifecycleFault::Store(error.to_string()))?;
    Ok(())
}

/// The Skill Manager's leg over one `UPDATING` row (§17.3: the manager owns the
/// row's completion). Returns (completed, fault-line): a *completed* row walked
/// 更新完了 (`SkillUpdated`); every other disposition lands `FAILED`
/// (`SkillValidationFailed`) with the reason as the fault line — except a store
/// fault, which surfaces through [`LifecycleFault`] and leaves the row in flight
/// (the cadence retries; a retried row re-runs the leg from the same rest state).
#[allow(clippy::too_many_arguments)]
fn manager_leg(
    store: &mut AthenaStore,
    row: &athena_types::Skill,
    local_dir: &Path,
    registered_dir: &Path,
    allow: &AllowList,
    now: &str,
    correlation: &str,
) -> (bool, Option<String>) {
    let (id, version) = (row.id.clone(), row.version.clone());
    let pin = format!("{}@{version}", id.as_str());
    let located = match locate_pin(local_dir, registered_dir, id.as_str(), version.as_str()) {
        Ok(located) => located,
        Err(reason) => {
            return match walk_completion(
                store,
                &id,
                &version,
                SkillState::Updating,
                SkillState::Failed,
                "update",
                row.checksum.as_str(),
                now,
                correlation,
            ) {
                Ok(()) => (false, Some(format!("update {pin}: 取得失敗 — {reason}"))),
                Err(fault) => (false, Some(format!("update {pin}: {fault}"))),
            };
        }
    };
    let located_root = match &located {
        Located::Source(root) | Located::Registered(root) => root.clone(),
    };
    match gate(&located_root, allow) {
        Ok(checksum) => {
            // 更新完了 (`SkillUpdated`, UPDATING → AVAILABLE): a source-located pin
            // re-syncs the registered copy first — a copy fault leaves the row in
            // flight (the tick reports it; the cadence retries).
            if matches!(located, Located::Source(_))
                && let Err(reason) = sync_registered_copy(
                    &located_root,
                    registered_dir,
                    id.as_str(),
                    version.as_str(),
                )
            {
                return (
                    false,
                    Some(format!(
                        "update {pin}: the registered copy did not refresh — {reason}"
                    )),
                );
            }
            match walk_completion(
                store,
                &id,
                &version,
                SkillState::Updating,
                SkillState::Available,
                "update",
                checksum.as_str(),
                now,
                correlation,
            ) {
                Ok(()) => (true, None),
                Err(fault) => (false, Some(format!("update {pin}: {fault}"))),
            }
        }
        Err(reason) => {
            // 検証失敗 (`SkillValidationFailed`, UPDATING → FAILED): the registered
            // copy stays untouched — a failed gate never destroys the previous body.
            match walk_completion(
                store,
                &id,
                &version,
                SkillState::Updating,
                SkillState::Failed,
                "update",
                row.checksum.as_str(),
                now,
                correlation,
            ) {
                Ok(()) => (false, Some(format!("update {pin}: 検証失敗 — {reason}"))),
                Err(fault) => (false, Some(format!("update {pin}: {fault}"))),
            }
        }
    }
}

/// The Skill Validator's leg over one `VALIDATING` row (§17.3 rows 検証成功 /
/// 検証失敗): the same located material, the same gate — 検証成功 → `SkillValidated`
/// (`VALIDATING → AVAILABLE`), 検証失敗 / unlocatable → `SkillValidationFailed`
/// (`VALIDATING → FAILED`). The revalidation face's 202 (`FAILED → VALIDATING`) is
/// what put the row here; this leg is what the request was recorded *for*.
#[allow(clippy::too_many_arguments)]
fn validator_leg(
    store: &mut AthenaStore,
    row: &athena_types::Skill,
    local_dir: &Path,
    registered_dir: &Path,
    allow: &AllowList,
    now: &str,
    correlation: &str,
) -> (bool, Option<String>) {
    let (id, version) = (row.id.clone(), row.version.clone());
    let pin = format!("{}@{version}", id.as_str());
    let located = locate_pin(local_dir, registered_dir, id.as_str(), version.as_str());
    let located_root = match &located {
        Ok(Located::Source(root) | Located::Registered(root)) => root.clone(),
        Err(reason) => {
            return match walk_completion(
                store,
                &id,
                &version,
                SkillState::Validating,
                SkillState::Failed,
                "revalidate",
                row.checksum.as_str(),
                now,
                correlation,
            ) {
                Ok(()) => (
                    false,
                    Some(format!("revalidate {pin}: 検証失敗 — {reason}")),
                ),
                Err(fault) => (false, Some(format!("revalidate {pin}: {fault}"))),
            };
        }
    };
    match gate(&located_root, allow) {
        Ok(checksum) => match walk_completion(
            store,
            &id,
            &version,
            SkillState::Validating,
            SkillState::Available,
            "revalidate",
            checksum.as_str(),
            now,
            correlation,
        ) {
            Ok(()) => (true, None),
            Err(fault) => (false, Some(format!("revalidate {pin}: {fault}"))),
        },
        Err(reason) => match walk_completion(
            store,
            &id,
            &version,
            SkillState::Validating,
            SkillState::Failed,
            "revalidate",
            row.checksum.as_str(),
            now,
            correlation,
        ) {
            Ok(()) => (
                false,
                Some(format!("revalidate {pin}: 検証失敗 — {reason}")),
            ),
            Err(fault) => (false, Some(format!("revalidate {pin}: {fault}"))),
        },
    }
}
