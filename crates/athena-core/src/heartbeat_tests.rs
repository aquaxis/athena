//! X-36/X-42's tests: the heartbeat beat face — typed emission (ATH-OBS-011) with the
//! receipt (ATH-CORE-047). Seeded Runtime Instance rows prove the receipt writes; the
//! no-row face proves the store's truth is reported, not forced (never-invent); the
//! idle face proves a beat without a RUNNING Task still emits (no fabricated focus).

use crate::heartbeat::{beat_once, observed_iteration};
use athena_persistence::AthenaStore;
use athena_persistence::repository::{EventRepository, RuntimeInstanceRepository, TaskRepository};
use athena_types::{
    AgentInstanceState, InstanceId, Priority, RiskLevel, RuntimeInstance, RuntimeInstanceId,
    RuntimeType, SideEffects, Task, TaskId, TaskState,
};

fn store() -> AthenaStore {
    AthenaStore::in_memory().expect("store")
}

fn task_row(state: TaskState, iterations: u64) -> Task {
    Task {
        id: TaskId::new(),
        title: "the beat's observation".into(),
        instruction: "hold the beat's observation".into(),
        constraints: vec![],
        completion_criteria: vec![],
        state,
        priority: Priority::Normal,
        plan_id: None,
        risk_level: RiskLevel::Medium,
        attempt_count: 0,
        max_attempts: 3,
        iteration_count: iterations,
        max_iterations: 50,
        deadline: None,
        parent_id: None,
        workspace: "/repo/x36-beat".into(),
        goal: Some("hold the beat's observation".into()),
        created_by: "test".into(),
        created_at: "2026-11-12T00:00:00.000Z".into(),
        updated_at: "2026-11-12T00:00:00.000Z".into(),
        finished_at: None,
    }
}

fn runtime_fixture() -> RuntimeInstance {
    RuntimeInstance {
        id: RuntimeInstanceId::new(),
        runtime_type: RuntimeType::AgentCli,
        instance_id: InstanceId::new(),
        state: AgentInstanceState::Busy,
        pid: Some(4242),
        last_heartbeat_at: None,
        started_at: "2026-11-12T00:00:00Z".into(),
        stopped_at: None,
        stop_reason: None,
        exit_code: None,
        side_effects: SideEffects::None,
    }
}

#[test]
fn the_beat_appends_the_typed_heartbeat_with_the_observed_faces() {
    let mut store = store();
    let running = task_row(TaskState::Running, 5);
    let _queued = task_row(TaskState::Queued, 3);
    TaskRepository::insert(&mut store, &running).expect("task seeds");
    TaskRepository::insert(&mut store, &_queued).expect("task seeds");

    let iteration = observed_iteration(&store).expect("iteration reads");
    assert_eq!(iteration, 5);

    let report =
        beat_once(&mut store, iteration, None, "2026-11-12T00:00:04.000Z").expect("the beat emits");
    assert_eq!(report.pending_requests, 1);
    assert_eq!(report.focus_task_id, Some(running.id.clone()));
    // No bookkeeping row was named: the refusal is reported, not forced (never-invent).
    assert!(!report.receipt_recorded);
    assert_eq!(
        report.receipt_note.as_deref(),
        Some("no resident runtime instance observed")
    );

    let events = EventRepository::list_since(&store, 0, 100).expect("events");
    let beat = events
        .iter()
        .find(|event| event.event_type == "CoreAgentHeartbeat")
        .expect("the typed heartbeat was appended");
    assert_eq!(beat.actor, "core-agent");
    let payload: serde_json::Value =
        serde_json::from_str(&beat.payload_json).expect("payload parses");
    assert_eq!(payload["iteration"], 5);
    assert_eq!(payload["pending_requests"], 1);
    assert_eq!(payload["focus_task_id"], running.id.as_str());
}

#[test]
fn the_idle_beat_emits_without_a_focus() {
    // The idle floor of ATH-OBS-011: a beat fires with nothing running and nothing
    // queued, carrying no fabricated focus item — and the typed gate accepts it
    // (a mandatory item is a non-`Option` member, D-3).
    let mut store = store();
    let report =
        beat_once(&mut store, 0, None, "2026-11-12T00:00:04.000Z").expect("the idle beat emits");
    assert_eq!(report.pending_requests, 0);
    assert_eq!(report.focus_task_id, None);

    let events = EventRepository::list_since(&store, 0, 100).expect("events");
    let beat = events
        .iter()
        .find(|event| event.event_type == "CoreAgentHeartbeat")
        .expect("the typed heartbeat was appended");
    let payload: serde_json::Value =
        serde_json::from_str(&beat.payload_json).expect("payload parses");
    assert_eq!(payload["iteration"], 0);
    assert_eq!(payload["pending_requests"], 0);
    assert!(
        payload.get("focus_task_id").is_none(),
        "{}",
        beat.payload_json
    );
}

#[test]
fn the_receipt_refreshes_the_runtime_row() {
    let mut store = store();
    let fixture = runtime_fixture();
    RuntimeInstanceRepository::insert(&mut store, &fixture).expect("runtime row seeds");

    let report = beat_once(&mut store, 0, Some(&fixture.id), "2026-11-12T00:00:04.000Z")
        .expect("the beat emits");
    assert!(report.receipt_recorded, "{:?}", report.receipt_note);
    assert_eq!(report.receipt_note, None);

    let row = RuntimeInstanceRepository::get(&store, &fixture.id)
        .expect("fetch")
        .expect("present");
    assert_eq!(
        row.last_heartbeat_at.as_deref(),
        Some("2026-11-12T00:00:04.000Z")
    );
}

#[test]
fn an_unknown_runtime_row_is_reported_not_forced() {
    // The store's row set is the truth: an unknown `runtime_instance_id` is refused by
    // `record_heartbeat`, the refusal rides the report as a note, and the beat's *event*
    // still lands — nothing is invented to force the receipt (X-42's never-invent face).
    let mut store = store();
    let report = beat_once(
        &mut store,
        0,
        Some(&RuntimeInstanceId::new()),
        "2026-11-12T00:00:04.000Z",
    )
    .expect("the beat emits");
    assert!(!report.receipt_recorded);
    assert!(
        report.receipt_note.is_some(),
        "the refusal must be reported: {:?}",
        report.receipt_note
    );

    let events = EventRepository::list_since(&store, 0, 100).expect("events");
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "CoreAgentHeartbeat"),
        "the beat's event is not conditional on the receipt row"
    );
}

#[test]
fn observed_iteration_reports_the_store_maximum() {
    // The Core Agent spans Tasks (ATH-STATE-012), so its observed iteration is the
    // highest count the store's Task rows carry — and an empty store observes 0.
    let empty = store();
    assert_eq!(observed_iteration(&empty).expect("iterates"), 0);

    let mut store = store();
    for iterations in [3_u64, 5_u64] {
        let row = task_row(TaskState::Failed, iterations);
        TaskRepository::insert(&mut store, &row).expect("task seeds");
    }
    assert_eq!(observed_iteration(&store).expect("iterates"), 5);
}
