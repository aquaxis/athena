//! The stall-detection face (X-37): the production wiring of ATH-OBS-012/013.
//!
//! Two detectors have existed in [`crate::observability`] since P1-43 — `silence` (the
//! state-change quiet line) and `wait_cycle` (the wait graph's cycle) — but no production
//! caller fed them. This module is that caller: one resident-tick scan ([`scan_once`])
//! that reads the store's own truth and emits the `TaskStalled` Event the mandate names.
//! Both detectors are called — X-49 closed the silence face's own leg, which the scan had
//! re-decided inline, so the threshold comparison and the wire reason are the detector's
//! alone.
//!
//! The rule, in wire-exact terms:
//!
//! - **ATH-OBS-012 (MUST).** A non-terminal Task whose last state-change Event is older
//!   than `core.iteration_timeout * scheduler.stall_detection_factor` emits `TaskStalled`
//!   (payload `stall_seconds` = the observed quiet span). The escalation face is the
//!   Event chain itself — the record a human reads on the wire (ATH-NFR-010); no
//!   synthesized `CoreAgentEscalated` is invented (never-invent).
//! - **ATH-OBS-013 (MUST).** The wait graph (instance-to-instance edges from
//!   `AgentWaiting.waits_on`, ATH-EVENT-018) closing a cycle emits `TaskStalled` for the
//!   Task the cycle's edge rows belong to — before any timeout (the immediate detection
//!   the cycle face gets by spec).
//! - **State-change vocabulary.** The Task-family events name transitions (`TaskQueued`,
//!   `TaskStarted`, `TaskResultReceived`, `TaskReplanning`, `TaskRetrying`,
//!   `TaskCancelling`, `TaskCancelled`, `TaskCompleted`, `TaskFailed`); `TaskStalled` is
//!   a Scheduler-family observation and is not itself a state change, so a stalled
//!   Task's quiet span keeps growing.

//! - **Waiting is not idling (v1).** A Task in `WAITING_APPROVAL`/`WAITING_INPUT` sits in
//!   the HITL lane with its own deadline faces (ATH-HITL-012); its silence is structural
//!   waiting, so the silence line does not apply to it — approval expiry and the
//!   wait-deadline faces own its progress. The cycle face still applies to the Tasks a
//!   wait-graph cycle's edges belong to.
//! - **Re-state at the line, only at the line.** A Task already stalled keeps stalling;
//!   `TaskStalled` is re-emitted once per threshold window (the last Event still
//!   describes the open window), so a human reading the chain sees each stalled window
//!   without a per-tick storm. A `TaskStalled` from an earlier (closed) window suppresses
//!   nothing — the age test against the last *state-change* Event is the only gate.
//!
//! The cadence/semantics split (D-35's unit-face): this module owns the rule; the
//! resident (`athena-api`) owns the clock and calls [`scan_once`] on its tick with the
//! configured threshold in hand.

use athena_events::events::{AgentWaiting, WaitOn};
use athena_persistence::repository::{EventRepository, TaskRepository};
use athena_persistence::{AthenaStore, NewEvent};
use athena_types::Task;
use athena_types::clock::parse_rfc3339_millis;
use athena_types::{EventId, InstanceId, TaskId, TaskState};

use crate::observability::{Stall, silence, wait_cycle};

/// What one scan recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StallReport {
    /// The Tasks the scan observed stalling, in emission order.
    pub stalled: Vec<StalledTask>,
    /// How many non-terminal Tasks were examined (the scan's own breadth).
    pub examined: u64,
    /// How many wait-graph edges the scan read (zero is the v1 truth while no
    /// `AgentWaiting` row exists yet — reported, not guessed).
    pub wait_edges: u64,
}

/// One stalling Task, with the Event it brought to the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StalledTask {
    /// The stalled Task.
    pub task_id: TaskId,
    /// The detector face that fired: `state_change_silence` or `wait_graph_cycle`.
    pub reason: &'static str,
    /// The `TaskStalled` Event's id.
    pub event_id: EventId,
}

/// Why a scan refused (the store's own refusals and the typed gate only).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScanFault {
    /// The store refused a read or the append; surfaced whole.
    #[error("store fault: {0}")]
    Store(String),
    /// The typed `TaskStalled` construction was refused by its own gate
    /// (ATH-EVENT-017/D-3 hygiene); surfaced whole.
    #[error("the TaskStalled event was refused by its construction gate: {0}")]
    Construction(String),
}

/// The threshold in milliseconds: the iteration timeout times the stall factor.
#[must_use]
pub const fn stall_threshold_ms(iteration_timeout_ms: u64, factor: u64) -> u64 {
    iteration_timeout_ms.saturating_mul(factor)
}

/// The last state-change Event of a Task, from its own event chain.
fn last_state_change(
    events: &[athena_persistence::repository::StoredEvent],
) -> Option<athena_persistence::repository::StoredEvent> {
    events
        .iter()
        .filter(|event| STATE_CHANGE_NAMES.contains(&event.event_type.as_str()))
        .max_by_key(|event| event.sequence)
        .cloned()
}

/// The state-change event names (spec section 23.2, the Task family's transition rows)
/// plus the row that starts a Task's own chain.
pub const STATE_CHANGE_NAMES: [&str; 10] = [
    "TaskCreated",
    "TaskQueued",
    "TaskStarted",
    "TaskResultReceived",
    "TaskReplanning",
    "TaskRetrying",
    "TaskCancelling",
    "TaskCancelled",
    "TaskCompleted",
    "TaskFailed",
];

/// One wait edge as the events hold it: the waiting instance, the instance it waits on,
/// the Task the waiting row joins (for the attribution the cycle face cites), and the
/// row's own stamp (the honest `last_event_at` a cycle stall reports).
#[derive(Debug, Clone)]
struct WaitEdgeRow {
    from: InstanceId,
    to: InstanceId,
    task: Option<TaskId>,
    occurred_at: String,
}

/// The wait-graph edges as the events hold them. The latest `AgentWaiting` per waiting
/// instance is the row that counts (a later wait replaces the earlier — a first-hit walk
/// over the reversed chain), and each `WaitOn::Instance` target of that row is one edge;
/// rows without their own `instance_id` cannot key an edge and are skipped, as are
/// non-instance targets (approvals and messages are leaves, not cycle members) — the
/// wire rows are the only authority (never-invent).
fn wait_edges(store: &AthenaStore) -> Result<Vec<WaitEdgeRow>, ScanFault> {
    let events = EventRepository::list_since(store, 0, u64::MAX)
        .map_err(|fault| ScanFault::Store(fault.to_string()))?;
    // Reversed walk: the first hit per `from` is that instance's latest AgentWaiting.
    let mut latest: Vec<WaitEdgeRow> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for event in events.iter().rev() {
        if event.event_type != "AgentWaiting" {
            continue;
        }
        let Some(from) = event.instance_id.clone() else {
            continue;
        };
        if !seen.insert(from.to_string()) {
            continue;
        }
        let Ok(payload) = serde_json::from_str::<AgentWaiting>(&event.payload_json) else {
            continue;
        };
        for target in payload.waits_on {
            let WaitOn::Instance { instance_id: to } = target else {
                continue;
            };
            latest.push(WaitEdgeRow {
                from: from.clone(),
                to,
                task: event.task_id.clone(),
                occurred_at: event.occurred_at.clone(),
            });
        }
    }
    Ok(latest)
}

fn wait_graph_cycles(store: &mut AthenaStore) -> Result<(u64, Vec<(TaskId, String)>), ScanFault> {
    let edges = wait_edges(store)?;
    let count = u64::try_from(edges.len()).unwrap_or(u64::MAX);
    if edges.is_empty() {
        return Ok((count, Vec::new()));
    }
    let mut pairs: Vec<(InstanceId, InstanceId)> = Vec::with_capacity(edges.len());
    for edge in &edges {
        pairs.push((edge.from.clone(), edge.to.clone()));
    }
    let Some(Stall::WaitCycle { cycle }) = wait_cycle(&pairs) else {
        return Ok((count, Vec::new()));
    };
    // The cycle exists: attribute it to the Tasks the cycle's own edge rows belong to.
    // An edge row without an owning Task cannot be attributed and is skipped, and a Task
    // is named once, in edge order — the members are a set (never-invent).
    let members: std::collections::BTreeSet<InstanceId> = cycle.iter().cloned().collect();
    let mut named: std::collections::BTreeSet<TaskId> = std::collections::BTreeSet::new();
    let mut stalled: Vec<(TaskId, String)> = Vec::new();
    for edge in &edges {
        if !(members.contains(&edge.from) && members.contains(&edge.to)) {
            continue;
        }
        let Some(task_id) = edge.task.clone() else {
            continue;
        };
        if named.insert(task_id.clone()) {
            stalled.push((task_id, edge.occurred_at.clone()));
        }
    }
    Ok((count, stalled))
}

// --- the emit face: the wire-exact TaskStalled Event (ATH-EVENT-017/D-3 hygiene) ---
/// Emit one `TaskStalled` Event for a Task: the typed construction, then the append.
///
/// The payload is the catalog's exact triple; the Event joins the Task's own chain — the
/// correlation id is read from the chain, and when the chain holds no key yet the id is
/// minted per Event from the tick (`stall-{now}`, the boot/maintenance digest precedent)
/// so the Event chain stays joinable either way (ATH-OBS-004/009); no id is invented
/// from nothing.
fn emit_stalled(
    store: &mut AthenaStore,
    task_id: &TaskId,
    reason: &'static str,
    stall_ms: u64,
    last_state_change_at: Option<&str>,
    now: &str,
) -> Result<EventId, ScanFault> {
    // The Task's own correlation key from its chain; the digest mint (boot/maintenance
    // precedent) covers the empty-chain face honestly rather than manufacturing one.
    let correlation = EventRepository::list_for_task(store, task_id)
        .map_err(|fault| ScanFault::Store(fault.to_string()))?
        .into_iter()
        .rev()
        .map(|event| event.correlation_id)
        .find(|key| !key.is_empty())
        .unwrap_or_else(|| athena_events::redact::digest(&format!("stall-{now}")));

    let last_event_at = last_state_change_at.unwrap_or(now);
    let stalled = athena_events::TypedEvent::new(
        athena_events::events::TaskStalled {
            task_id: task_id.clone(),
            last_event_at: last_event_at.to_string(),
            stall_seconds: stall_ms / 1000,
        },
        athena_events::EventContext {
            event_id: EventId::new(),
            occurred_at: now.to_string(),
            actor: athena_events::envelope::Actor::System,
            task_id: Some(task_id.clone()),
            plan_step_id: None,
            agent_id: None,
            instance_id: None,
            skill_id: None,
            correlation_id: correlation,
            causation_id: None,
        },
    )
    .map_err(|fault| ScanFault::Construction(format!("{reason}: {fault}")))?;

    let stored = EventRepository::append(
        store,
        &NewEvent {
            event_id: stalled.event_id.clone(),
            schema_version: stalled.schema_version,
            event_type: stalled.event_type.to_string(),
            occurred_at: stalled.occurred_at.clone(),
            task_id: stalled.task_id.clone(),
            plan_step_id: stalled.plan_step_id.clone(),
            agent_id: stalled.agent_id.clone(),
            instance_id: stalled.instance_id.clone(),
            skill_id: stalled.skill_id.clone(),
            correlation_id: stalled.correlation_id.clone(),
            causation_id: stalled.causation_id.clone(),
            actor: stalled.actor.as_str().to_string(),
            payload_json: stalled.payload_json.clone(),
        },
    )
    .map_err(|fault| ScanFault::Store(fault.to_string()))?;
    Ok(stored.event_id)
}

// The scan body: the cycle face fires immediately (before any timeout), then the
// silence face walks the non-terminal rows (the record a human reads is the Event).
/// One resident tick of stall detection (ATH-OBS-012/013): the cycle face first —
/// closed `AgentWaiting` wait-graph cycles escalate immediately, before any timeout —
/// then the silence face over non-terminal Tasks whose last state-change Event is
/// older than `threshold_ms`. Each stalled Task is emitted once per threshold window
/// and `TaskStalled` itself is the escalation record a human reads (never-invent).
pub fn scan_once(
    store: &mut AthenaStore,
    threshold_ms: u64,
    now: &str,
) -> Result<StallReport, ScanFault> {
    // The store's own truth: every Task row; the scan examines the non-terminal set
    // (the state the mandate names). A read fault surfaces whole (ScanFault::Store).
    let tasks =
        TaskRepository::list_all(store).map_err(|fault| ScanFault::Store(fault.to_string()))?;
    let non_terminal: Vec<Task> = tasks
        .into_iter()
        .filter(|task| !task.state.is_terminal())
        .collect();
    let examined = u64::try_from(non_terminal.len()).unwrap_or(u64::MAX);

    // --- ATH-OBS-013: the wait graph's cycles stall immediately (before any timeout).
    //     The edges are rebuilt from the store's own Event rows (the `AgentWaiting`
    //     face, ATH-EVENT-018); a Task the cycle's edge rows belong to is named.
    let (edges_count, cycle_stalls) = wait_graph_cycles(store)?;
    let mut stalled: Vec<StalledTask> = Vec::new();
    let mut seen: std::collections::BTreeSet<TaskId> = std::collections::BTreeSet::new();
    for (task_id, last_event_at) in cycle_stalls {
        if !seen.insert(task_id.clone()) {
            continue; // one Event per Task per tick; the next tick re-states as needed
        }
        let event_id = emit_stalled(
            store,
            &task_id,
            "wait_graph_cycle",
            0,
            Some(last_event_at.as_str()),
            now,
        )?;
        stalled.push(StalledTask {
            task_id,
            reason: "wait_graph_cycle",
            event_id,
        });
    }

    // --- ATH-OBS-012: the state-change silence line, over the non-terminal set. The
    //     scan reads each Task's own Event chain (the Task-family rows above).
    let now_ms = parse_rfc3339_millis(now).unwrap_or(0);
    for task in &non_terminal {
        if matches!(
            task.state,
            TaskState::WaitingApproval | TaskState::WaitingInput
        ) {
            continue; // waiting is not idling (v1): the HITL deadline faces own progress
        }
        let events = EventRepository::list_for_task(store, &task.id)
            .map_err(|fault| ScanFault::Store(fault.to_string()))?;
        let last = match last_state_change(&events) {
            Some(event) => event,
            None => continue, // no state-change line yet: the gate is the line, not a guess
        };
        let Some(last_ms) = parse_rfc3339_millis(&last.occurred_at) else {
            continue; // the store's stamp is its truth; an unreadable one is skipped,
            // never rewritten and never guessed into an age
        };
        // The ATH-OBS-012 comparison itself belongs to the detector (X-49: the scan used
        // to re-decide the line here, so the threshold rule lived in two places). The
        // signal carries the observed span and names its own wire reason.
        let quiet_ms = now_ms.saturating_sub(last_ms);
        let Some(signal) = silence(quiet_ms, threshold_ms) else {
            continue;
        };
        let reason = signal.kind();
        // Re-state at the line, only at the line: a TaskStalled stamped inside this
        // window (newer than `now - threshold`) suppresses the re-emission — no
        // per-tick storm — while a stalemated row from an earlier (closed) window
        // suppresses nothing.
        let now_ms_floor = now_ms.saturating_sub(threshold_ms);
        let in_window = events.iter().any(|event| {
            event.event_type == "TaskStalled"
                && parse_rfc3339_millis(&event.occurred_at)
                    .is_some_and(|stamped| stamped > now_ms_floor)
        });
        if in_window {
            continue;
        }
        let event_id = emit_stalled(
            store,
            &task.id,
            reason,
            quiet_ms,
            Some(last.occurred_at.as_str()),
            now,
        )?;
        stalled.push(StalledTask {
            task_id: task.id.clone(),
            reason,
            event_id,
        });
    }

    Ok(StallReport {
        stalled,
        examined,
        wait_edges: edges_count,
    })
}
