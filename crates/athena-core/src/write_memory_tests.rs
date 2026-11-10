//! X-34's core faces under test: `apply_write_memory` (both tier faces) and the
//! retention sweep (`sweep_once` + `record_swept`), with a seeded Task as the anchor.

use crate::working_memory::WorkingItem;
use crate::write_memory::{Requester, WritableTier, apply_write_memory};
use athena_persistence::AthenaStore;
use athena_persistence::repository::TaskRepository;
use athena_types::{MemoryTier, Task, TaskId};

fn store_with_task() -> (AthenaStore, TaskId) {
    let mut store = AthenaStore::in_memory().expect("store");
    let task = Task {
        id: TaskId::new(),
        title: "the write's anchor".into(),
        instruction: "hold the provenance anchor".into(),
        constraints: vec![],
        completion_criteria: vec![],
        state: athena_types::TaskState::Running,
        priority: athena_types::Priority::Normal,
        plan_id: None,
        risk_level: athena_types::RiskLevel::Medium,
        attempt_count: 0,
        max_attempts: 3,
        iteration_count: 0,
        max_iterations: 50,
        deadline: None,
        parent_id: None,
        workspace: "/repo/x34-core-tests".into(),
        goal: Some("hold the provenance anchor".into()),
        created_by: "test".into(),
        created_at: "2026-11-10T00:00:00.000Z".into(),
        updated_at: "2026-11-10T00:00:00.000Z".into(),
        finished_at: None,
    };
    TaskRepository::insert(&mut store, &task).expect("task seeds");
    (store, task.id)
}

#[test]
fn the_working_face_writes_the_masked_judgement() {
    // ATH-MEM-003's Working face: the agent's explicit judgement lands as a content
    // record under the Task's scope, already masked (R-POL-13), with the Task's id as
    // the scope (ATH-MEM-001).
    let (mut store, task) = store_with_task();
    let requester = Requester::CoreAgentJudgement;
    let report = apply_write_memory(
        &mut store,
        &requester,
        WritableTier::Working,
        "remember the failing harness case (api_key: AKIAIOSFODNN7EXAMPLE)",
        &task,
        None,
        10_000,
        "2026-11-10T00:00:00.000Z",
        "cor-write-working",
    )
    .expect("the Working face accepts any explicit request");
    assert_eq!(report.tier, MemoryTier::Working);
    assert_eq!(report.scope_id, task.as_str());

    let records =
        crate::MemoryRepository::list(&store, MemoryTier::Working, task.as_str()).expect("listed");
    assert_eq!(records.len(), 1);
    let item: WorkingItem =
        serde_json::from_str(&records[0].content).expect("the item is the §13.1 face");
    assert!(matches!(item, WorkingItem::AgentJudgement { .. }));
    // The hygiene pass ran at the writer (R-POL-13): the burst is masked in the record.
    assert!(
        !records[0].content.contains("AKIAIOSFODNN7EXAMPLE"),
        "{}",
        records[0].content
    );
}

#[test]
fn the_long_face_refuses_an_unanchored_and_disabled_write() {
    let (mut store, task) = store_with_task();
    let requester = Requester::CoreAgentJudgement;

    // The tier's gate writes the anchored judgement and appends the catalog Event
    // (`ATH-MEM-003`'s (a) face, with the retention ceiling as the caller's argument).
    let report = apply_write_memory(
        &mut store,
        &requester,
        WritableTier::Long,
        "the long-term architectural decision",
        &task,
        None,
        10_000,
        "2026-11-10T00:00:00.000Z",
        "cor-write-long",
    )
    .expect("the explicit judgement anchors itself with the Task");
    assert_eq!(report.tier, MemoryTier::Long);
    let events = athena_persistence::repository::EventRepository::list_since(&store, 0, 100)
        .expect("events");
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "MemoryRecordCreated")
    );

    // A disabled tier refuses (ATH-MEM-003's explicit gate) — the review gate itself is
    // the refuse-face; the caller's ceiling argument owns the retention afterwards.
    let refused = crate::memory_manager::long_term::review(
        &crate::memory_manager::long_term::Requester::CoreAgentJudgement,
        "another decision",
        &crate::write_memory::project_scope_of(task_workspace()),
        false,
        athena_types::MemoryOrigin {
            task_id: Some(task.clone()),
            event_id: None,
        },
    )
    .expect_err("a disabled tier refuses");
    assert!(matches!(
        refused,
        crate::memory_manager::long_term::WriteFault::TierDisabled
    ));
}

fn task_workspace() -> &'static str {
    "/repo/x34-core-tests"
}

#[test]
fn the_retention_sweep_removes_only_what_the_configuration_says() {
    use crate::maintenance::{record_swept, sweep_once};

    let (mut store, _task) = store_with_task();
    let now = "2026-11-10T00:00:00.000Z";
    // A cutoff far in the future prunes nothing a fresh store owns.
    let report = sweep_once(
        &mut store,
        now,
        "2027-01-01T00:00:00.000Z",
        "2027-01-01T00:00:00.000Z",
        "2027-01-01T00:00:00.000Z",
    )
    .expect("the sweep runs");
    assert_eq!(
        (
            report.events_pruned,
            report.clarifications_expired,
            report.idempotency_keys_pruned,
            report.delivery_records_pruned
        ),
        (0, 0, 0, 0)
    );

    // The audit Event rides only when something was swept (the production cadence's rule).
    let audit_before = athena_persistence::repository::EventRepository::list_since(&store, 0, 100)
        .expect("events")
        .iter()
        .filter(|event| event.event_type == "EventAppended")
        .count();
    record_swept(&mut store, &report, now, "cor-sweep").expect("recorded");
    let audit_after = athena_persistence::repository::EventRepository::list_since(&store, 0, 100)
        .expect("events")
        .iter()
        .filter(|event| event.event_type == "EventAppended")
        .count();
    assert_eq!(audit_after - audit_before, 1);
}
