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
//!
//! ATH-AIPRJ-029 (Instance `.aiprj` area deletion after `events.retention_days`) is NOT
//! implemented in the sweep cadence: v1 keeps the deletion deferred — the *instance rows*
//! the sweep would consult must outlive terminal states (they do — `AgentRepository`'s
//! `list_all_instances` names that face) but the areas' path derivation is not stored
//! (a workspace string + an attempt, not a durable area path), so an area deletion
//! implemented from a reconstructed path could delete wrong directories. Deferring a
//! MUST until its facts are durable is the never-invent discipline's own face; the deferral
//! itself is recorded (requirements §3.34.18+, tasks ledger) rather than hidden.

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
/// keys and delivery records. Every step is a bounded deletion; every step's count is
/// reported, and a step's store fault fails the cycle rather than being swallowed.
///
/// # Errors
/// [`SweepFault::Step`] with the step's name when one sweep step faulted.
pub fn sweep_once(
    store: &mut AthenaStore,
    now: &str,
    cutoff_events: &str,
    cutoff_idempotency: &str,
    cutoff_deliveries: &str,
) -> Result<SweepReport, SweepFault> {
    Ok(SweepReport {
        events_pruned: events_step(store, cutoff_events)?,
        clarifications_expired: clarifications_step(store, now)?,
        idempotency_keys_pruned: idempotency_step(store, cutoff_idempotency)?,
        delivery_records_pruned: deliveries_step(store, cutoff_deliveries)?,
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
