//! The retention-sweep semantics (X-34): every implemented sweep call in one testable
//! module, so the resident cadence (`athena-api`) stays free of state-machine knowledge
//! (the same split `core_loop`'s semantics/cadence keeps, D-35's unit-face).
//!
//! What a sweep *is* (`ATH-PERS-006`/`ATH-EVENT-014`, ATH-HITL-018, ATH-AIPRJ-029, the
//! IdempotencyRepository/MessagingSection retention notes): bounded periodic deletions
//! whose sole authority is the configuration (retention values) and whose facts are
//! *recorded*, never inferred. Nothing here deletes outside its named rule; results are
//! reported as counts, since the catalog has no custom "MaintenanceSwept" Event type
//! (§23.2 — the catalog is the only Event vocabulary, ATH-EVENT-009), and Events carry
//! references only (ATH-EVENT-007).
//!
//! Sweeps owned here:
//! - [`events_prune`] — `EventRepository::prune_before` (ATH-EVENT-014; each Task's final
//!   result survives, the store preserves it).
//! - [`clarification_expiry_sweep`] — `clarifications::expiry_sweep` (ATH-HITL-018:
//!   silence is never an answer).
//! - [`idempotency_prune`] — `IdempotencyRepository::prune_before` (the 7-day default
//!   retention lives on the trait's contract).
//! - [`delivery_prune`] — `MessageRepository::prune_deliveries_before`
//!   (`messaging.dedup_retention`).
//! - [`aiprj_area_step`] — the Instance `.aiprj` work-area deletion (ATH-AIPRJ-029), whose
//!   earlier deferral is lifted by the **recognition fence** described on that function:
//!   an area is deleted only when the store and the disk *together* identify it as one
//!   ATHENA created for a terminal Instance past the retention horizon. A candidate that
//!   fails any fence condition is counted as skipped and left on disk — the deletion never
//!   runs from a path ATHENA merely guessed (the never-invent discipline is kept as a
//!   verification rule rather than as an absent feature).

use athena_persistence::repository::{EventRepository, IdempotencyRepository, MessageRepository};
use athena_persistence::{AthenaStore, NewEvent};
use athena_types::{EventId, TaskId};

/// One sweep cycle's outcome: what each bounded deletion removed, in counts (facts, not
/// prose — the §32.1 log face renders them).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SweepReport {
    /// Events pruned (`prune_before`, ATH-EVENT-014).
    pub events_pruned: u64,
    /// Clarifications whose deadline passed; their Tasks failed (ATH-HITL-018).
    pub clarifications_expired: u64,
    /// Idempotency keys pruned (default retention 7 days).
    pub idempotency_keys_pruned: u64,
    /// Delivery records pruned (`messaging.dedup_retention`).
    pub delivery_records_pruned: u64,
    /// Instance `.aiprj` work areas deleted (ATH-AIPRJ-029).
    pub aiprj_areas_deleted: u64,
    /// Candidate areas the recognition fence refused to delete — a terminal, aged-out
    /// Instance whose area could not be identified as ATHENA's own (the honest residue:
    /// the area stays on disk and the count is reported, never silenced).
    pub aiprj_areas_skipped: u64,
}

/// Why a sweep cycle refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SweepFault {
    /// The store refused; surfaced whole, never renamed.
    #[error("store fault: {0}")]
    Store(String),
    /// A single sweep step's fault, surfaced with the step's name.
    #[error("sweep step {step}: {fault}")]
    Step {
        /// The step's stable name.
        step: &'static str,
        /// The fault text.
        fault: String,
    },
}

/// Run one full sweep cycle at a single `now` stamp: events, clarifications, idempotency
/// keys, delivery records and the Instance `.aiprj` areas. Every step is a bounded
/// deletion; every step's count is reported, and a step's store fault fails the cycle
/// rather than being swallowed.
///
/// `instances_root` is `<ATHENA_HOME>/var/instances` — the one directory under which an
/// Instance work area may legally sit. `None` skips step 5 entirely: a caller that cannot
/// name the root has no business deleting directories (the step is absent, not guessed).
///
/// # Errors
/// [`SweepFault::Step`] with the step's name when one sweep step faulted.
pub fn sweep_once(
    store: &mut AthenaStore,
    now: &str,
    cutoff_events: &str,
    cutoff_idempotency: &str,
    cutoff_deliveries: &str,
    instances_root: Option<&std::path::Path>,
) -> Result<SweepReport, SweepFault> {
    let events_pruned = events_step(store, cutoff_events)?;
    let clarifications_expired = clarifications_step(store, now)?;
    let idempotency_keys_pruned = idempotency_step(store, cutoff_idempotency)?;
    let delivery_records_pruned = deliveries_step(store, cutoff_deliveries)?;
    // Step 5 last: the area deletion reads the instance rows, which no earlier step
    // touches, so the order is the report's own reading order rather than a dependency.
    let (aiprj_areas_deleted, aiprj_areas_skipped) = match instances_root {
        Some(root) => aiprj_area_step(store, cutoff_events, root)?,
        None => (0, 0),
    };
    Ok(SweepReport {
        events_pruned,
        clarifications_expired,
        idempotency_keys_pruned,
        delivery_records_pruned,
        aiprj_areas_deleted,
        aiprj_areas_skipped,
    })
}

/// Step 1 of [`sweep_once`] — Events (ATH-EVENT-014): the store preserves each Task's
/// final result.
fn events_step(store: &mut AthenaStore, cutoff_events: &str) -> Result<u64, SweepFault> {
    EventRepository::prune_before(store, cutoff_events).map_err(step_fault("events"))
}

/// Step 2 of [`sweep_once`] — clarifications (ATH-HITL-018: silence is never an answer;
/// swept Tasks fail).
fn clarifications_step(store: &mut AthenaStore, now: &str) -> Result<u64, SweepFault> {
    crate::clarifications::expiry_sweep(store, now, &digest_tag("clarification-sweep", now))
        .map(|swept| swept.len() as u64)
        .map_err(|fault| SweepFault::Step {
            step: "clarifications",
            fault: fault.to_string(),
        })
}

/// Step 3 of [`sweep_once`] — idempotency keys (ATH-CORE-035's ledger; the repository's
/// 7-day contract).
fn idempotency_step(store: &mut AthenaStore, cutoff_idempotency: &str) -> Result<u64, SweepFault> {
    IdempotencyRepository::prune_before(store, cutoff_idempotency)
        .map_err(step_fault("idempotency"))
}

/// Step 4 of [`sweep_once`] — delivery records (ATH-MSG-014's dedup ledger;
/// `messaging.dedup_retention`).
fn deliveries_step(store: &mut AthenaStore, cutoff_deliveries: &str) -> Result<u64, SweepFault> {
    MessageRepository::prune_deliveries_before(store, cutoff_deliveries)
        .map_err(step_fault("deliveries"))
}

/// Step 5 of [`sweep_once`] — the Instance `.aiprj` work areas (ATH-AIPRJ-029: the areas
/// are kept past terminal states and deleted once the Event retention horizon passes, and
/// the execution is recorded).
///
/// Returns `(deleted, skipped)`.
///
/// **The recognition fence.** A directory is deleted only when the store and the disk
/// together identify it as one ATHENA created for this very Instance. A candidate must be
/// a terminal Instance (`ATH-AIPRJ-026`: the area outlives the terminal state — and
/// `is_terminal` means `STOPPED` alone, since `FAILED` still transitions to `STOPPING` for
/// resource reclamation, `ATH-STATE-007`; a row that can still move keeps its area) whose
/// `finished_at` is readable and older than the Event cutoff, and its area must then pass
/// every condition below:
///
/// 1. the path is **recorded, or exactly reconstructible** — either `aiprj_log_ref` names
///    the `AI_LOG` directory the Runtime wrote (its parent is the area), or the row's
///    `workspace` equals `<instances_root>/<instance id>` *exactly*, which is the layout
///    `init_workspace` creates (`ATH-OPS-009`). A workspace pointing anywhere else is not
///    this Instance's area and is never followed;
/// 2. the area's final component is `.aiprj`;
/// 3. the area sits **under** `instances_root` after both paths are canonicalized (so a
///    symlink or a `..` segment cannot walk out of the instances tree);
/// 4. the area holds an `AI_LOG` directory — the shape `init_workspace` leaves behind.
///
/// A candidate failing any condition is counted as skipped and left untouched: the sweep
/// reports the residue rather than deleting on a guess. A deletion clears the row's area
/// reference, which both records the execution and makes the step idempotent.
///
/// # Errors
/// [`SweepFault::Step`] when the store read or the reference clear faulted. A filesystem
/// fault on one area is counted as skipped, never failing the cycle: the next cadence tick
/// retries, and one unreadable directory must not stop the other retention work.
pub fn aiprj_area_step(
    store: &mut AthenaStore,
    cutoff_events: &str,
    instances_root: &std::path::Path,
) -> Result<(u64, u64), SweepFault> {
    let Ok(root) = instances_root.canonicalize() else {
        // No instances tree yet: nothing was ever created, so nothing is deleted.
        return Ok((0, 0));
    };
    let instances =
        crate::AgentRepository::list_all_instances(store).map_err(step_fault("aiprj_areas"))?;
    let mut deleted = 0_u64;
    let mut skipped = 0_u64;
    let mut cleared = Vec::new();
    for instance in &instances {
        if !instance.state.is_terminal() {
            continue;
        }
        // An unreadable or absent finish stamp is not an aged-out fact — the row is not a
        // candidate at all (never-invent: the sweep does not date it itself).
        let Some(finished) = instance.finished_at.as_deref() else {
            continue;
        };
        if finished >= cutoff_events {
            continue;
        }
        match recognized_area(instance, &root) {
            Some(area) => match std::fs::remove_dir_all(&area) {
                Ok(()) => {
                    deleted += 1;
                    cleared.push(instance.id.clone());
                }
                Err(_) => skipped += 1,
            },
            None => skipped += 1,
        }
    }
    for id in &cleared {
        crate::AgentRepository::set_aiprj_area(store, id, None)
            .map_err(step_fault("aiprj_areas"))?;
    }
    Ok((deleted, skipped))
}

/// The recognition fence's decision (see [`aiprj_area_step`]): the area path when the row
/// and the disk agree it is ATHENA's own, `None` otherwise.
fn recognized_area(
    instance: &athena_types::AgentInstance,
    canonical_root: &std::path::Path,
) -> Option<std::path::PathBuf> {
    // Condition 1: recorded path first, exact layout reconstruction second.
    let area = match instance.aiprj_log_ref.as_deref() {
        Some(log_ref) => std::path::Path::new(log_ref).parent()?.to_path_buf(),
        None => {
            let expected_workspace = canonical_root.join(instance.id.as_str());
            let workspace = std::path::Path::new(&instance.workspace);
            let same = workspace == expected_workspace
                || workspace
                    .canonicalize()
                    .is_ok_and(|resolved| resolved == expected_workspace);
            if !same {
                return None;
            }
            expected_workspace.join(".aiprj")
        }
    };
    // Condition 2: the area is an `.aiprj` directory, by name.
    if area.file_name()? != std::ffi::OsStr::new(".aiprj") {
        return None;
    }
    // Condition 3: it resolves to a directory under the instances root.
    let resolved = area.canonicalize().ok()?;
    if !resolved.starts_with(canonical_root) || !resolved.is_dir() {
        return None;
    }
    // Condition 4: it carries the shape `init_workspace` leaves — an `AI_LOG` directory.
    if !resolved.join("AI_LOG").is_dir() {
        return None;
    }
    Some(resolved)
}

/// The Event that records a sweep's execution (the sweep's *audit*, per ATH-AIPRJ-029's
/// "実施を記録する" face, generalized to every sweep a cadence runs). The catalog has no
/// bespoke kind; §23.2's `EventAppended` — the store's own append audit — is where such
/// counts live, keeping the Event vocabulary to the catalog (ATH-EVENT-009) and the
/// payload reference-shaped (ATH-EVENT-007): counts, never content.
///
/// # Errors
/// Store faults, surfaced whole.
pub fn record_swept(
    store: &mut AthenaStore,
    report: &SweepReport,
    at: &str,
    correlation_id: &str,
) -> Result<(), SweepFault> {
    let event = NewEvent {
        event_id: EventId::new(),
        schema_version: 1,
        event_type: "EventAppended".into(),
        occurred_at: at.to_string(),
        task_id: None,
        plan_step_id: None,
        agent_id: None,
        instance_id: None,
        skill_id: None,
        correlation_id: correlation_id.to_string(),
        causation_id: None,
        actor: "system".into(),
        payload_json: serde_json::json!({
            "sweep": {
                "events_pruned": report.events_pruned,
                "clarifications_expired": report.clarifications_expired,
                "idempotency_keys_pruned": report.idempotency_keys_pruned,
                "delivery_records_pruned": report.delivery_records_pruned,
                "aiprj_areas_deleted": report.aiprj_areas_deleted,
                "aiprj_areas_skipped": report.aiprj_areas_skipped,
            }
        })
        .to_string(),
    };
    EventRepository::append(store, &event)
        .map(|_stored| ())
        .map_err(|fault| SweepFault::Store(fault.to_string()))
}

/// Whether a Task is terminal (the state the sweep consults for the retention consults:
/// a terminal Task's Events age out; a live Task's do not — the retention's own face).
#[must_use]
pub fn task_is_terminal(store: &AthenaStore, task: &TaskId) -> bool {
    crate::TaskRepository::get(store, task)
        .map(|row| row.is_some_and(|task_row| task_row.state.is_terminal()))
        .unwrap_or(false)
}

fn step_fault(
    step: &'static str,
) -> impl Fn(athena_persistence::error::PersistenceError) -> SweepFault {
    move |fault| SweepFault::Step {
        step,
        fault: fault.to_string(),
    }
}

fn digest_tag(tag: &str, at: &str) -> String {
    athena_events::redact::digest(&format!("{tag}-{at}"))
}
