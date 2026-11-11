//! The Core Agent heartbeat face (X-36 / X-42): the production emission of the
//! `CoreAgentHeartbeat` Event and the Runtime Instance heartbeat receipt, in one testable
//! module the resident cadence calls.
//!
//! Two facts, one tick:
//!
//! - **Emission (ATH-OBS-011, MUST).** The Core Agent emits `CoreAgentHeartbeat` every
//!   `core_agent_restart.heartbeat_interval` (default 20 s) so that "no state transition
//!   for a while" is observable (ATH-OBS-004's absence side). The Event is built by the
//!   typed session builder (`core_agent_session::heartbeat_event` — the catalog struct,
//!   hygiene-checked at construction) and appended through the store, joining no Task's
//!   chain — a heartbeat is instance #0's own fact, so its correlation id is minted per
//!   beat (the `digest` precedent the boot and maintenance faces keep).
//! - **Receipt (ATH-CORE-047).** The backing Runtime Instance row's
//!   `last_heartbeat_at` is refreshed through `RuntimeInstanceRepository::record_heartbeat`
//!   — the lease side of the heartbeat contract. The row is the store's own truth: when no
//!   row holds the resident's `runtime_instance_id` the receipt is refused, and that refusal
//!   is reported (a fact), never swallowed and never fabricated into a heartbeat.
//!
//! The cadence/semantics split (D-35's unit-face): this module owns the rule; the resident
//! (`athena-api`) owns the clock and calls [`beat_once`] on its interval.

use athena_events::envelope::Actor;
use athena_persistence::repository::{
    AgentRepository, EventRepository, RuntimeInstanceRepository, TaskRepository,
};
use athena_persistence::{AthenaStore, NewEvent};
use athena_types::{AgentId, EventId, TaskId};

/// What one beat recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeatReport {
    /// The beat's Event id.
    pub event_id: EventId,
    /// How many `QUEUED` Tasks the beat observed (`pending_requests`' value).
    pub pending_requests: u64,
    /// The Task currently in focus, when one is `RUNNING` (oldest first).
    pub focus_task_id: Option<TaskId>,
    /// Whether the Runtime Instance receipt landed (`record_heartbeat` accepted).
    pub receipt_recorded: bool,
    /// Why the receipt refused, when it did (a reported fact, not an error — the store's
    /// own row set is authoritative).
    pub receipt_note: Option<String>,
}

/// Why a beat refused (the store's own refusals only; a receipt refusal is *not* one —
/// it rides [`BeatReport::receipt_note`]).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BeatFault {
    /// The store refused the append; surfaced whole.
    #[error("store fault: {0}")]
    Store(String),
    /// The typed construction refused — a correlation-less context is invalid
    /// (ATH-OBS-004), so this names the builder's own gate.
    #[error("the heartbeat event was refused by its construction gate: {0}")]
    Construction(String),
}

/// One heartbeat: build the typed `CoreAgentHeartbeat`, append it, and record the
/// Runtime Instance receipt (ATH-OBS-011 emission face + ATH-CORE-047 lease face).
///
/// `iteration` is the Core Agent Loop's count as the caller observes it, `runtime_instance`
/// the resident's `ri-…` id (the receipt's row key) — pass `None` when instance #0 is not
/// resident (the supervision poll's own face keeps that truth); the Event still emits,
/// because the *Core Agent's* beat is not conditional on a bookkeeping row.
///
/// # Errors
/// [`BeatFault`] when the store or the typed gate refuses; the caller logs and retries at
/// the next tick.
pub fn beat_once(
    store: &mut AthenaStore,
    iteration: u64,
    runtime_instance: Option<&athena_types::RuntimeInstanceId>,
    now: &str,
) -> Result<BeatReport, BeatFault> {
    // The observed faces (facts, not guesses): the queue depth is `QUEUED`, the focus is
    // the oldest `RUNNING` Task — the same reads the execution loop reserves by.
    let queued = TaskRepository::list_by_state(store, athena_types::TaskState::Queued, u64::MAX)
        .map_err(|fault| BeatFault::Store(fault.to_string()))?;
    let running = TaskRepository::list_by_state(store, athena_types::TaskState::Running, u64::MAX)
        .map_err(|fault| BeatFault::Store(fault.to_string()))?;
    let focus = running.first().map(|task| task.id.clone());
    let pending_requests = queued.len() as u64;

    // The typed event (catalog `CoreAgentHeartbeat`, ATH-EVENT-017's mandatory keys
    // enforced at construction). A heartbeat joins no Task's chain: the correlation id is
    // minted per beat — `digest` of the beat's stamp, the boot/maintenance precedent.
    let context = athena_events::EventContext {
        event_id: EventId::new(),
        occurred_at: now.to_string(),
        actor: Actor::CoreAgent,
        task_id: focus.clone(),
        plan_step_id: None,
        agent_id: Some(AgentId::core_agent()),
        instance_id: None,
        skill_id: None,
        correlation_id: athena_events::redact::digest(&format!("heartbeat-{now}")),
        causation_id: None,
    };
    // The builder this module's own documentation names (X-49: the construction used to be
    // inlined here, so `CoreAgentHeartbeat`'s payload shape was assembled in two places).
    let beat = crate::core_agent_session::heartbeat_event(
        iteration,
        focus.clone(),
        pending_requests,
        context,
    )
    .map_err(|fault| BeatFault::Construction(fault.to_string()))?;

    // Fold to the persisted row and append (the Event Manager's store face).
    let event = NewEvent {
        event_id: beat.event_id.clone(),
        schema_version: beat.schema_version,
        event_type: beat.event_type.to_string(),
        occurred_at: beat.occurred_at.clone(),
        task_id: beat.task_id.clone(),
        plan_step_id: beat.plan_step_id.clone(),
        agent_id: beat.agent_id.clone(),
        instance_id: beat.instance_id.clone(),
        skill_id: beat.skill_id.clone(),
        correlation_id: beat.correlation_id.clone(),
        causation_id: beat.causation_id.clone(),
        actor: beat.actor.as_str().to_string(),
        payload_json: beat.payload_json.clone(),
    };
    let stored = EventRepository::append(store, &event)
        .map_err(|fault| BeatFault::Store(fault.to_string()))?;

    // The receipt (ATH-CORE-047): refresh the Runtime Instance row's own heartbeat. The
    // store's row set is the truth — an unknown row is refused by `record_heartbeat`, and
    // that refusal rides the report as a note: the resident's `runtime_instance_id` is a
    // bookkeeping key the store does not yet hold, so nothing is invented to force the
    // write.
    let receipt = match runtime_instance {
        Some(id) => match RuntimeInstanceRepository::record_heartbeat(store, id, now) {
            Ok(()) => (true, None),
            Err(fault) => (false, Some(fault.to_string())),
        },
        None => (false, Some("no resident runtime instance observed".into())),
    };

    Ok(BeatReport {
        event_id: stored.event_id,
        pending_requests,
        focus_task_id: focus,
        receipt_recorded: receipt.0,
        receipt_note: receipt.1,
    })
}

/// The Core Agent Loop's iteration count, as the resident observes it: the store's own
/// truth — the maximum `iteration_count` across all Task rows (the loop the Tasks ran;
/// instance #0 spans Tasks, ATH-STATE-012, so no single Task row owns the count).
///
/// # Errors
/// Store faults surface whole.
pub fn observed_iteration(store: &AthenaStore) -> Result<u64, BeatFault> {
    Ok(known_instances_highest_iteration(store))
}

fn known_instances_highest_iteration(store: &AthenaStore) -> u64 {
    TaskRepository::list_all(store)
        .unwrap_or_default()
        .iter()
        .map(|task| task.iteration_count)
        .max()
        .unwrap_or(0)
}

/// The instance row the resident's runtime id backs, when the store holds one (the
/// receipt-side read the cadence may consult before beating; a diagnostic face only —
/// `beat_once` never requires it).
///
/// # Errors
/// Store faults surface whole.
pub fn instance_for_runtime(
    store: &AthenaStore,
    runtime_instance: &athena_types::RuntimeInstanceId,
) -> Result<Option<athena_types::AgentInstance>, BeatFault> {
    AgentRepository::list_all_instances(store)
        .map(|rows| {
            rows.into_iter().find(|row| {
                row.runtime_instance_id
                    .as_ref()
                    .is_some_and(|backing| backing.as_str() == runtime_instance.as_str())
            })
        })
        .map_err(|fault| BeatFault::Store(fault.to_string()))
}
