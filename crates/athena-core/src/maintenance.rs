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
//! - [`approval_expiry_step`] — the §17.4 approval expiry (ATH-HITL-005: silence is
//!   rejection, never an implicit approval; `EXPIRED` counts as a refusal,
//!   ATH-STATE-011).
//! - [`aiprj_area_step`] — the Instance `.aiprj` work-area deletion (ATH-AIPRJ-029), whose
//!   earlier deferral is lifted by the **recognition fence** described on that function:
//!   an area is deleted only when the store and the disk *together* identify it as one
//!   ATHENA created for a terminal Instance past the retention horizon. A candidate that
//!   fails any fence condition is counted as skipped and left on disk — the deletion never
//!   runs from a path ATHENA merely guessed (the never-invent discipline is kept as a
//!   verification rule rather than as an absent feature).

use athena_events::Payload;
use athena_persistence::repository::{
    ApprovalRepository, EventRepository, IdempotencyRepository, MessageRepository, TaskRepository,
};
use athena_persistence::{AthenaStore, NewEvent};
use athena_state::{ApprovalTrigger, TaskTrigger, approval_transition, task_transition};
use athena_types::{Approval, ApprovalScope, EventId, TaskId, TaskState};

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
    /// Approvals whose deadline passed, resolved as refusals (ATH-HITL-005,
    /// ATH-STATE-011) — both §17.4 expiry edges counted together.
    pub approvals_expired: u64,
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
/// keys, delivery records, the Instance `.aiprj` areas and the lapsed approvals. Every
/// step is bounded; every step's count is reported, and a step's store fault fails the
/// cycle rather than being swallowed.
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
    // Step 6: the approval deadline. It reads the approval rows alone, which no earlier
    // step touches, so again the order is the report's reading order, not a dependency.
    let approvals_expired = approval_expiry_step(store, now)?;
    Ok(SweepReport {
        events_pruned,
        clarifications_expired,
        idempotency_keys_pruned,
        delivery_records_pruned,
        approvals_expired,
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

/// Step 6 of [`sweep_once`] — the approval deadline (`ATH-HITL-005`: no answer inside the
/// deadline is a **refusal**, never an implicit approval; `ATH-STATE-011`: `EXPIRED` is
/// treated as a rejection). The actor is the Approval Manager, not the Core Agent: §17.4
/// assigns both expiry edges to it (`ATH-STATE-025`), and this step is where that
/// assignment is executed.
///
/// §17.4 holds exactly two expiry edges, and this step walks both:
///
/// 1. `REQUESTED → EXPIRED` — the response deadline (`Approval::expires_at`,
///    `ATH-HITL-004`) passed with no human answer. The candidate set is the store's own
///    [`ApprovalRepository::list_expired`] query.
/// 2. `GRANTED → EXPIRED` — `scope.expires_at` (`ATH-HITL-013`) arrived before the
///    approval was consumed. The candidates come from
///    [`ApprovalRepository::list_granted_unconsumed`], narrowed to the rows that *carry*
///    such a stamp: only `ApprovalScope::Pattern` has a `scope.expires_at`, so an
///    `operation_only` grant has no scope expiry fact and is never dated by this step
///    (never-invent — the edge's trigger is absent, not assumed).
///
/// Each lapsed row is resolved through the §17.4 table (`athena_state`), so the target
/// state, the Event name and the actor are read rather than named here (`ATH-STATE-002`).
/// The row is written first with no uses left and **no responder identity invented** —
/// silence has no approver; `ATH-HITL-006`'s trail is the `ApprovalExpired` Event's own
/// actor and stamp. When the owning Task waits on that approval, the §17.1 row
/// (`WAITING_APPROVAL` + `ApprovalExpired` → `FAILED`) is walked in the same store window
/// with that very Event (the `clarifications` step's precedent), so the default refusal
/// actually reaches the Task instead of leaving it waiting forever.
///
/// Idempotent by the row's own state: an expired row leaves both candidate queries
/// (`REQUESTED`/`GRANTED` are their only inputs), so a second cycle at the same `now`
/// finds nothing and writes no second Event.
///
/// # Errors
/// [`SweepFault::Step`] when the store refused, when §17.4 has no row for a pairing, or
/// when the Event construction refused (the catalog's required keys, `ATH-EVENT-017`).
pub fn approval_expiry_step(store: &mut AthenaStore, now: &str) -> Result<u64, SweepFault> {
    let correlation_id = digest_tag("approval-expiry-sweep", now);
    // Edge 1: the store's query is the candidate set (its own `REQUESTED` + past-deadline
    // reading of ATH-HITL-005).
    let mut lapsed: Vec<(Approval, ApprovalTrigger)> = ApprovalRepository::list_expired(store, now)
        .map_err(step_fault("approvals"))?
        .into_iter()
        .map(|row| (row, ApprovalTrigger::Expired))
        .collect();
    // Edge 2: a granted, unconsumed row whose *scope* stamp passed.
    for row in
        ApprovalRepository::list_granted_unconsumed(store).map_err(step_fault("approvals"))?
    {
        if let ApprovalScope::Pattern { expires_at, .. } = &row.scope
            && crate::approval_manager::staging::deadline_passed(expires_at, now)
        {
            lapsed.push((row, ApprovalTrigger::ScopeExpired));
        }
    }
    let mut expired = 0_u64;
    for (row, trigger) in &lapsed {
        expire_one(store, row, *trigger, now, &correlation_id)?;
        expired += 1;
    }
    Ok(expired)
}

/// One approval's expiry: the §17.4 row, the state write, and the `ApprovalExpired` Event
/// (joined to the waiting Task's §17.1 edge when the Task is waiting on it).
fn expire_one(
    store: &mut AthenaStore,
    row: &Approval,
    trigger: ApprovalTrigger,
    now: &str,
    correlation_id: &str,
) -> Result<(), SweepFault> {
    let transition = approval_transition(row.state, trigger).ok_or_else(|| SweepFault::Step {
        step: "approvals",
        fault: format!(
            "the §17.4 table has no row for {} + {trigger:?}",
            row.state.as_str()
        ),
    })?;
    // The table names the Event; the typed payload must be that very kind, or the step
    // refuses rather than emitting a kind the table did not ask for (ATH-EVENT-009).
    if transition.event != athena_events::events::ApprovalExpired::EVENT_TYPE {
        return Err(SweepFault::Step {
            step: "approvals",
            fault: format!(
                "the §17.4 row emits {}, not ApprovalExpired",
                transition.event
            ),
        });
    }
    ApprovalRepository::update_state(store, &row.id, transition.to, None, 0)
        .map_err(step_fault("approvals"))?;
    let typed = athena_events::TypedEvent::new(
        athena_events::events::ApprovalExpired {
            approval_id: row.id.clone(),
            scope: row.scope.clone(),
        },
        athena_events::EventContext {
            event_id: EventId::new(),
            occurred_at: now.to_string(),
            actor: transition.requester,
            task_id: Some(row.task_id.clone()),
            plan_step_id: None,
            agent_id: Some(row.requested_by.clone()),
            instance_id: Some(row.requested_by_instance_id.clone()),
            skill_id: None,
            correlation_id: correlation_id.to_string(),
            causation_id: None,
        },
    )
    .map_err(|fault| SweepFault::Step {
        step: "approvals",
        fault: fault.to_string(),
    })?;
    let event = NewEvent {
        event_id: typed.event_id.clone(),
        schema_version: typed.schema_version,
        event_type: typed.event_type.to_string(),
        occurred_at: typed.occurred_at.clone(),
        task_id: typed.task_id.clone(),
        plan_step_id: typed.plan_step_id.clone(),
        agent_id: typed.agent_id.clone(),
        instance_id: typed.instance_id.clone(),
        skill_id: typed.skill_id.clone(),
        correlation_id: typed.correlation_id.clone(),
        causation_id: typed.causation_id.clone(),
        actor: typed.actor.as_str().to_string(),
        payload_json: typed.payload_json.clone(),
    };
    // The Task leg: only a Task actually waiting on an approval has the §17.1 edge; any
    // other state keeps the Event as the record alone (the refusal is the approval's
    // fact, and a Task that already moved on is not re-decided here).
    let waiting = TaskRepository::get(store, &row.task_id)
        .map_err(step_fault("approvals"))?
        .is_some_and(|task| task.state == TaskState::WaitingApproval);
    if waiting {
        let task_row = task_transition(TaskState::WaitingApproval, TaskTrigger::ApprovalExpired)
            .ok_or_else(|| SweepFault::Step {
                step: "approvals",
                fault: "the §17.1 table has no WAITING_APPROVAL + ApprovalExpired row".into(),
            })?;
        store
            .transition_task(
                &row.task_id,
                TaskState::WaitingApproval,
                task_row.to,
                &event,
            )
            .map(|_stored| ())
            .map_err(step_fault("approvals"))
    } else {
        EventRepository::append(store, &event)
            .map(|_stored| ())
            .map_err(step_fault("approvals"))
    }
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
                "approvals_expired": report.approvals_expired,
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
