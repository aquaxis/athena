//! X-51's tests: the Core Agent's `transition_task` acceptance path.
//!
//! The row's real defect was that a rejected approval left a Task in `WAITING_APPROVAL`
//! with no path to `REPLANNING` — the §17.1 row for that edge names Core Agent as its
//! requester, so the human's endpoint was the wrong actor (D-51). These exercise the path
//! that *is* the right one, including every refusal: a stale `expected_state`, an edge the
//! table does not define, an edge another path owns, and the one whose Event keys are not
//! yet facts.

use crate::state_manager::transition::{TransitionFault, accept_transition};
use athena_persistence::AthenaStore;
use athena_persistence::repository::{EventRepository, TaskRepository};
use athena_types::{
    AgentId, Approval, ApprovalId, ApprovalScope, ApprovalState, PlanStepDraft, Priority,
    RiskLevel, Task, TaskId, TaskState,
};

const NOW: &str = "2026-11-13T00:30:00.000Z";

fn task_row(state: TaskState) -> Task {
    Task {
        id: TaskId::new(),
        parent_id: None,
        title: "x51".into(),
        instruction: "find an alternative after the rejection".into(),
        workspace: "/repo/x51".into(),
        goal: Some("ship the feature another way".into()),
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
        created_by: "test".into(),
        created_at: NOW.into(),
        updated_at: NOW.into(),
        finished_at: None,
    }
}

/// A Task in `state`, with `revisions` plan revisions already issued through the real
/// intake (so the revision chain is the store's own, never hand-written).
fn seeded(state: TaskState, revisions: u64) -> (AthenaStore, Task) {
    let mut store = AthenaStore::in_memory().expect("store");
    let task = task_row(state);
    TaskRepository::insert(&mut store, &task).expect("task");
    for revision in 0..revisions {
        crate::plan_intake::accept_plan(
            &mut store,
            &task.id,
            revision,
            vec![PlanStepDraft {
                order: 1,
                intent: format!("revision {}", revision + 1),
                depends_on_orders: vec![],
                assigned_agent: None,
                skill_set: vec![],
            }],
            NOW,
        )
        .expect("plan lands");
    }
    (store, task)
}

fn event_types(store: &AthenaStore, task: &TaskId) -> Vec<String> {
    EventRepository::list_for_task(store, task)
        .expect("events")
        .into_iter()
        .map(|event| event.event_type)
        .collect()
}

#[test]
fn a_rejected_approval_reaches_replanning_through_the_core_agents_request() {
    // The defect X-51 names, closed: the Task is `WAITING_APPROVAL` after a rejection and
    // the Core Agent's own request moves it on. The superseding revision exists because the
    // agent submitted it first — the order the Event's keys require.
    let (mut store, task) = seeded(TaskState::WaitingApproval, 2);
    let rejected = Approval {
        id: ApprovalId::new(),
        task_id: task.id.clone(),
        requested_by: AgentId::core_agent(),
        operation: "shell:rm -rf build".into(),
        reason: "clean the tree".into(),
        impact: "build outputs disappear".into(),
        risk_level: RiskLevel::High,
        state: ApprovalState::Rejected,
        operation_descriptor: "shell".into(),
        requested_by_instance_id: athena_types::InstanceId::new(),
        uses_remaining: 0,
        expires_at: NOW.into(),
        responded_by: Some("human".into()),
        responded_at: Some(NOW.into()),
        scope: ApprovalScope::OperationOnly {
            operation_only: true,
        },
    };
    athena_persistence::repository::ApprovalRepository::insert(&mut store, &rejected)
        .expect("approval");

    let moved = accept_transition(
        &mut store,
        &task.id,
        TaskState::Replanning,
        TaskState::WaitingApproval,
        NOW,
        "cor-x51",
    )
    .expect("accepted");

    assert_eq!(moved.from, TaskState::WaitingApproval);
    assert_eq!(moved.to, TaskState::Replanning);
    assert_eq!(moved.event_type, "TaskReplanning");

    let row = TaskRepository::get(&store, &task.id)
        .expect("fetch")
        .expect("row");
    assert_eq!(row.state, TaskState::Replanning);

    // The Event's mandatory keys are the Task's own revision facts, and the actor is the
    // table's requester (Core Agent), not the API's.
    let stored = EventRepository::get(&store, &moved.event_id)
        .expect("fetch")
        .expect("event");
    assert_eq!(stored.actor, "core-agent");
    let payload: serde_json::Value =
        serde_json::from_str(&stored.payload_json).expect("payload json");
    assert_eq!(payload["from_revision"], 1);
    assert_eq!(payload["to_revision"], 2);
    assert!(
        payload["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("revision 2 supersedes 1")),
        "{payload}"
    );
}

#[test]
fn replanning_is_refused_until_a_superseding_revision_exists() {
    // The ordering question X-51 left open, answered by refusal rather than invention: at
    // revision 1 there is nothing that supersedes anything, so no `to_revision` is a fact.
    let (mut store, task) = seeded(TaskState::WaitingApproval, 1);
    let fault = accept_transition(
        &mut store,
        &task.id,
        TaskState::Replanning,
        TaskState::WaitingApproval,
        NOW,
        "cor-x51",
    )
    .expect_err("refused");
    assert!(
        matches!(
            fault,
            TransitionFault::SupersedingRevisionAbsent { current: 1, .. }
        ),
        "{fault}"
    );
    assert_eq!(fault.code(), "PLAN_REVISION_ABSENT");
    assert!(
        fault.to_string().contains("submit_plan"),
        "the refusal names what would supply the fact: {fault}"
    );
    // The Task did not move, and no Event was written.
    let row = TaskRepository::get(&store, &task.id)
        .expect("fetch")
        .expect("row");
    assert_eq!(row.state, TaskState::WaitingApproval);
    assert!(
        !event_types(&store, &task.id).contains(&"TaskReplanning".to_string()),
        "no Event names a replacement that did not happen"
    );
}

#[test]
fn a_task_with_no_plan_at_all_is_refused_the_same_way() {
    let (mut store, task) = seeded(TaskState::Evaluating, 0);
    let fault = accept_transition(
        &mut store,
        &task.id,
        TaskState::Replanning,
        TaskState::Evaluating,
        NOW,
        "cor-x51",
    )
    .expect_err("refused");
    assert!(
        matches!(
            fault,
            TransitionFault::SupersedingRevisionAbsent { current: 0, .. }
        ),
        "{fault}"
    );
}

#[test]
fn a_stale_expected_state_is_refused_optimistically() {
    // ATH-CORE-036 / ATH-STATE-014: the key exists to catch exactly this.
    let (mut store, task) = seeded(TaskState::Evaluating, 2);
    let fault = accept_transition(
        &mut store,
        &task.id,
        TaskState::Replanning,
        TaskState::WaitingApproval,
        NOW,
        "cor-x51",
    )
    .expect_err("refused");
    assert!(
        matches!(
            fault,
            TransitionFault::StateMismatch {
                actual: "EVALUATING",
                expected: "WAITING_APPROVAL",
                ..
            }
        ),
        "{fault}"
    );
    assert_eq!(fault.code(), "TASK_STATE_MISMATCH");
    assert_eq!(fault.class(), crate::protocol::ErrorClass::ValidationError);
}

#[test]
fn the_evaluating_and_waiting_input_rows_reach_replanning_too() {
    for from in [TaskState::Evaluating, TaskState::WaitingInput] {
        let (mut store, task) = seeded(from, 2);
        let moved = accept_transition(
            &mut store,
            &task.id,
            TaskState::Replanning,
            from,
            NOW,
            "cor-x51",
        )
        .unwrap_or_else(|fault| panic!("{from:?} should reach REPLANNING: {fault}"));
        assert_eq!(moved.event_type, "TaskReplanning");
    }
}

#[test]
fn replanning_returns_to_running_once_the_plan_settled() {
    // The other half of the cycle: `TaskStarted`'s keys are the latest plan's own, so the
    // leg out of REPLANNING is derivable without any agent input.
    let (mut store, task) = seeded(TaskState::Replanning, 2);
    let moved = accept_transition(
        &mut store,
        &task.id,
        TaskState::Running,
        TaskState::Replanning,
        NOW,
        "cor-x51",
    )
    .expect("accepted");
    assert_eq!(moved.event_type, "TaskStarted");

    let stored = EventRepository::get(&store, &moved.event_id)
        .expect("fetch")
        .expect("event");
    let payload: serde_json::Value =
        serde_json::from_str(&stored.payload_json).expect("payload json");
    assert_eq!(payload["revision"], 2);
    assert!(payload["plan_id"].as_str().is_some_and(|id| !id.is_empty()));
}

#[test]
fn the_settlement_edges_are_refused_by_name() {
    // A second path to `SUCCESS` would be a hole in X-48's gate, so the refusal points at
    // the settlement instead of walking it.
    let (mut store, task) = seeded(TaskState::Evaluating, 2);
    for to in [TaskState::Success, TaskState::Failed] {
        let fault = accept_transition(
            &mut store,
            &task.id,
            to,
            TaskState::Evaluating,
            NOW,
            "cor-x51",
        )
        .expect_err("refused");
        assert!(
            matches!(fault, TransitionFault::EdgeOwnedElsewhere { .. }),
            "{fault}"
        );
        assert_eq!(fault.code(), "TASK_TRANSITION_NOT_OWNED");
        assert_eq!(fault.class(), crate::protocol::ErrorClass::PolicyError);
        assert!(
            fault.to_string().contains("settle_task"),
            "the refusal names the owner: {fault}"
        );
    }
}

#[test]
fn an_undefined_edge_is_refused_as_undefined() {
    // ATH-STATE-002: the table defines no `QUEUED → EVALUATING`, so there is nothing to
    // walk and nothing to name as an owner.
    let (mut store, task) = seeded(TaskState::Queued, 2);
    let fault = accept_transition(
        &mut store,
        &task.id,
        TaskState::Evaluating,
        TaskState::Queued,
        NOW,
        "cor-x51",
    )
    .expect_err("refused");
    assert!(
        matches!(fault, TransitionFault::NoSuchEdge { .. }),
        "{fault}"
    );
    assert_eq!(fault.code(), "TASK_TRANSITION_UNDEFINED");
}

#[test]
fn an_absent_task_is_refused_before_anything_is_read() {
    let mut store = AthenaStore::in_memory().expect("store");
    let fault = accept_transition(
        &mut store,
        &TaskId::new(),
        TaskState::Replanning,
        TaskState::WaitingApproval,
        NOW,
        "cor-x51",
    )
    .expect_err("refused");
    assert!(matches!(fault, TransitionFault::TaskAbsent(_)), "{fault}");
    assert_eq!(fault.code(), "TASK_NOT_FOUND");
}
