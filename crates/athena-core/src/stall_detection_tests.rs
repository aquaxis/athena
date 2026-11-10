//! X-37's tests: the stall-detection faces — the silence line (ATH-OBS-012), the wait
//! graph's closed cycle (ATH-OBS-013), the HITL exemption (waiting is not idling), the
//! re-state window (re-stated once per threshold window, at the line only), and the
//! wire-exact `TaskStalled` payload triple.

use crate::stall_detection::{scan_once, stall_threshold_ms};
use athena_persistence::repository::{EventRepository, TaskRepository};
use athena_persistence::{AthenaStore, NewEvent};
use athena_types::{EventId, InstanceId, PlanId, Priority, RiskLevel, Task, TaskId, TaskState};

/// The epoch every silence seed is stamped at.
const SEED_STAMP: &str = "2026-11-12T00:00:00.000Z";
/// The test line, held as a constant so the const-eval face is itself under test:
/// `core.iteration_timeout` 1 s × `scheduler.stall_detection_factor` 2.
const THRESHOLD: u64 = stall_threshold_ms(1_000, 2);

fn store() -> AthenaStore {
    AthenaStore::in_memory().expect("store")
}

/// A Task row in the state under test; the scan reads the chain, not this row's clock.
fn task_row(state: TaskState) -> Task {
    Task {
        id: TaskId::new(),
        title: "the scan's observation".into(),
        instruction: "hold the scan's observation".into(),
        constraints: vec![],
        completion_criteria: vec![],
        state,
        priority: Priority::Normal,
        plan_id: None,
        risk_level: RiskLevel::Medium,
        attempt_count: 0,
        max_attempts: 3,
        iteration_count: 0,
        max_iterations: 50,
        deadline: None,
        parent_id: None,
        workspace: "/repo/x37-scan".into(),
        goal: Some("hold the scan's observation".into()),
        created_by: "test".into(),
        created_at: SEED_STAMP.into(),
        updated_at: SEED_STAMP.into(),
        finished_at: None,
    }
}

/// One state-change row joined to the Task's own chain: the seeded epoch line.
fn seed_started(store: &mut AthenaStore, task: &TaskId, occurred_at: &str) {
    append_row(
        store,
        "TaskStarted",
        Some(task),
        None,
        occurred_at,
        serde_json::json!({
            "plan_id": PlanId::new().as_str(),
            "revision": 0
        }),
    );
}

/// One waiting row (ATH-EVENT-018): `waits_on` in the wire's tagged form.
fn seed_wait(
    store: &mut AthenaStore,
    from: &InstanceId,
    wait_task: &TaskId,
    to: &InstanceId,
    occurred_at: &str,
) {
    append_row(
        store,
        "AgentWaiting",
        Some(wait_task),
        Some(from),
        occurred_at,
        serde_json::json!({
            "waits_on": [{"kind": "instance", "instance_id": to.as_str()}],
            "deadline": "2026-11-12T01:00:00.000Z"
        }),
    );
}

/// The store-side generic: one Event of `event_type`, joined to `task` (and `instance`
/// when the row carries its own), at an explicit stamp.
fn append_row(
    store: &mut AthenaStore,
    event_type: &str,
    task: Option<&TaskId>,
    instance: Option<&InstanceId>,
    occurred_at: &str,
    payload: serde_json::Value,
) {
    let event = NewEvent {
        event_id: EventId::new(),
        schema_version: 1,
        event_type: event_type.into(),
        occurred_at: occurred_at.into(),
        task_id: task.cloned(),
        plan_step_id: None,
        agent_id: None,
        instance_id: instance.cloned(),
        skill_id: None,
        correlation_id: "cor-x37".into(),
        causation_id: None,
        actor: "core-agent".into(),
        payload_json: payload.to_string(),
    };
    EventRepository::append(store, &event).expect("seed append");
}

/// The stalled rows a Task's chain holds, as (stamp, payload) pairs, chain order.
fn stalled_rows(store: &AthenaStore, task: &TaskId) -> Vec<(String, serde_json::Value)> {
    EventRepository::list_for_task(store, task)
        .expect("chain reads")
        .into_iter()
        .filter(|event| event.event_type == "TaskStalled")
        .map(|event| {
            (
                event.occurred_at,
                serde_json::from_str(&event.payload_json).expect("payload"),
            )
        })
        .collect()
}

/// The tick stamp the tests read: SEED_STAMP + `ms`.
fn stamp(ms: u64) -> String {
    format!("2026-11-12T00:00:{:02}.{:03}Z", ms / 1_000, ms % 1_000)
}
#[test]
fn the_silence_line_fires_at_the_threshold() {
    let mut store = store();
    let task = task_row(TaskState::Running);
    TaskRepository::insert(&mut store, &task).expect("task seeds");
    seed_started(&mut store, &task.id, SEED_STAMP);

    // Quiet since the seed (2,001 ms), past the line (1 s x 2): the scan stalls it.
    let report = scan_once(&mut store, THRESHOLD, "2026-11-12T00:00:02.001Z").expect("scan");
    assert_eq!(report.examined, 1);
    assert_eq!(report.stalled.len(), 1);
    let stalled = &report.stalled[0];
    assert_eq!(stalled.task_id, task.id);
    assert_eq!(stalled.reason, "state_change_silence");

    // The wire-exact payload (ATH-EVENT-017/D-3): the recorded Event is the mandate's
    // record, the wire's exact triple.
    let events = EventRepository::list_for_task(&store, &task.id).expect("chain");
    let stalled_row = events
        .iter()
        .find(|e| e.event_type == "TaskStalled")
        .expect("the stalled Event joins the Task's own chain");
    assert_eq!(stalled_row.task_id, Some(task.id.clone()));
    let payload: athena_events::events::TaskStalled =
        serde_json::from_str(&stalled_row.payload_json).expect("wire-exact payload");
    assert_eq!(payload.task_id, task.id);
    assert_eq!(payload.last_event_at, SEED_STAMP);
    assert_eq!(payload.stall_seconds, 2);
    assert_eq!(stalled_row.correlation_id, "cor-x37");
    assert!(stalled_row.actor == "system");
}

#[test]
fn below_the_line_the_scan_does_not_stall() {
    let mut store = store();
    let task = task_row(TaskState::Running);
    TaskRepository::insert(&mut store, &task).expect("task seeds");
    seed_started(&mut store, &task.id, SEED_STAMP);

    // Quiet 1,999 ms, under the 2,000 ms line: not stalled, nothing emitted.
    let report = scan_once(&mut store, THRESHOLD, "2026-11-12T00:00:01.999Z").expect("scan");
    assert!(report.stalled.is_empty());
    let events = EventRepository::list_for_task(&store, &task.id).expect("chain");
    assert!(events.iter().all(|e| e.event_type != "TaskStalled"));
}

#[test]
fn stalled_task_is_restated_once_per_threshold_window_then_suppressed() {
    let mut store = store();
    let task = task_row(TaskState::Running);
    TaskRepository::insert(&mut store, &task).expect("task seeds");
    seed_started(&mut store, &task.id, SEED_STAMP);

    // First tick 2,500 ms past the seeded line: the scan fires once.
    let first = scan_once(&mut store, THRESHOLD, &stamp(2_500)).expect("the line fires");
    assert_eq!(first.stalled.len(), 1);
    assert_eq!(first.stalled[0].reason, "state_change_silence");

    // A second tick at 3,000 ms — inside the same window (window = [2_500 - 2_000,
    // 2_500 + 2_000]; the first re-statement rides at 4_500) — re-emits nothing.
    let again = scan_once(&mut store, THRESHOLD, &stamp(3_000)).expect("the storm is held");
    assert!(again.stalled.is_empty());

    // At the next window's line the scan re-states, once.
    let restated = scan_once(&mut store, THRESHOLD, &stamp(4_501)).expect("the re-state");
    assert_eq!(restated.stalled.len(), 1);
    assert_eq!(restated.stalled[0].reason, "state_change_silence");
    assert_eq!(restated.stalled[0].task_id, task.id);

    // The wire carries exactly the two TaskStalled rows — one per window, no storm.
    let rows = stalled_rows(&store, &task.id);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "2026-11-12T00:00:02.500Z");
    assert_eq!(rows[1].0, "2026-11-12T00:00:04.501Z");
}

#[test]
fn a_task_in_the_hitl_lane_is_exempt_from_the_silence_line() {
    let mut store = store();
    let waiting = task_row(TaskState::WaitingApproval);
    TaskRepository::insert(&mut store, &waiting).expect("task seeds");
    seed_started(&mut store, &waiting.id, SEED_STAMP);

    // Far past the line (10 minutes, a valid stamp): the HITL lane's structural
    // waiting is not idling — the approval expiry and wait-deadline faces own its
    // progress (ATH-HITL-012), so the silence line does not apply.
    let report = scan_once(&mut store, THRESHOLD, "2026-11-12T00:10:00.000Z").expect("scan");
    assert_eq!(report.examined, 1);
    assert!(report.stalled.is_empty());
    let rows = stalled_rows(&store, &waiting.id);
    assert!(rows.is_empty());
}

#[test]
fn a_closed_wait_graph_cycle_stalls_before_any_timeout() {
    let mut store = store();
    let task = task_row(TaskState::Running);
    TaskRepository::insert(&mut store, &task).expect("task seeds");
    let a = InstanceId::new();
    let b = InstanceId::new();
    // a waits on b at T0; b waits on a at T0: the graph closes (ATH-EVENT-018).
    seed_wait(&mut store, &a, &task.id, &b, SEED_STAMP);
    seed_wait(&mut store, &b, &task.id, &a, SEED_STAMP);

    // The cycle face fires immediately — the stamp stays on the seed line.
    let report = scan_once(&mut store, THRESHOLD, SEED_STAMP).expect("the scan runs");
    assert_eq!(report.wait_edges, 2);
    assert_eq!(report.examined, 1);
    assert_eq!(report.stalled.len(), 1);
    let stalled = &report.stalled[0];
    assert_eq!(stalled.task_id, task.id);
    assert_eq!(stalled.reason, "wait_graph_cycle");
}

#[test]
fn the_scan_report_counts_the_faces_it_examined() {
    let mut store = store();
    let running = task_row(TaskState::Running);
    let waiting = task_row(TaskState::WaitingInput);
    let done = task_row(TaskState::Failed);
    for row in [&running, &waiting, &done] {
        TaskRepository::insert(&mut store, row).expect("task seeds");
        seed_started(&mut store, &row.id, SEED_STAMP);
    }
    // 2,100 ms quiet: the Running row is past the line; the WaitingInput row is in the
    // HITL lane (exempt); the terminal row is not examined at all.
    let report = scan_once(&mut store, THRESHOLD, &stamp(2_100)).expect("the scan runs");
    assert_eq!(report.examined, 2);
    assert_eq!(report.wait_edges, 0);
    assert_eq!(report.stalled.len(), 1);
    assert_eq!(report.stalled[0].task_id, running.id);
    assert_eq!(report.stalled[0].reason, "state_change_silence");
    // The terminal row's chain carries no TaskStalled — the scan left it alone.
    let done_chain = EventRepository::list_for_task(&store, &done.id).expect("chain reads");
    assert!(done_chain.iter().all(|e| e.event_type != "TaskStalled"));
}
