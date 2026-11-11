//! The Plan intake path (X-43): acceptance, the confirmation Event, the refusal envelope,
//! and the rule that a submission never settles its Task.

use crate::plan_intake::{IntakeFault, RationaleSource, accept_plan};
use athena_types::{PlanStepDraft, Task, TaskId, TaskState};

/// One Task row, with or without a stated goal (the rationale's source).
fn task_row(goal: Option<&str>) -> Task {
    Task {
        id: TaskId::new(),
        parent_id: None,
        title: "x43".into(),
        instruction: "wire the plan intake".into(),
        workspace: "/repo/x43".into(),
        goal: goal.map(str::to_string),
        constraints: vec![],
        completion_criteria: vec![],
        state: TaskState::Running,
        priority: athena_types::Priority::Normal,
        plan_id: None,
        risk_level: athena_types::RiskLevel::Medium,
        attempt_count: 0,
        max_attempts: 3,
        iteration_count: 0,
        max_iterations: 50,
        deadline: None,
        created_by: "test".into(),
        created_at: "2026-11-12T00:00:00.000Z".into(),
        updated_at: "2026-11-12T00:00:00.000Z".into(),
        finished_at: None,
    }
}

fn store_with(task: &Task) -> crate::AthenaStore {
    let mut store = crate::AthenaStore::in_memory().expect("store");
    crate::TaskRepository::insert(&mut store, task).expect("task row");
    store
}

fn draft(order: u64, intent: &str, depends_on_orders: Vec<u64>) -> PlanStepDraft {
    PlanStepDraft {
        order,
        intent: intent.to_string(),
        depends_on_orders,
        assigned_agent: None,
        skill_set: vec![],
    }
}

const NOW: &str = "2026-11-12T01:00:00.000Z";

#[test]
fn a_first_submission_lands_revision_one_and_records_plan_created() {
    // ATH-PLAN-004/005 + ATH-CORE-042: the first plan starts from revision 0 and becomes
    // revision 1, and its confirmation is `PlanCreated` — the Event a reader finds the
    // plan's standing in.
    let task = task_row(Some("ship the intake"));
    let mut store = store_with(&task);

    let accepted = accept_plan(
        &mut store,
        &task.id,
        0,
        vec![
            draft(1, "read the spec", vec![]),
            draft(2, "write it", vec![1]),
        ],
        NOW,
    )
    .expect("the submission lands");

    assert_eq!(accepted.revision, 1);
    assert_eq!(accepted.step_count, 2);
    assert!(accepted.first_revision);
    assert_eq!(accepted.rationale_source, RationaleSource::TaskGoal);

    // The plan row is readable at the revision the response names, with Core-minted ids.
    let plan = crate::PlanRepository::latest_for_task(&store, &task.id)
        .expect("read")
        .expect("plan row");
    assert_eq!(plan.revision, 1);
    assert_eq!(plan.steps.len(), 2);
    assert_eq!(plan.goal, "ship the intake");
    // The draft's `depends_on_orders` resolved to the minted step ids (D-5).
    let first = plan
        .steps
        .iter()
        .find(|step| step.order == 1)
        .expect("step 1");
    let second = plan
        .steps
        .iter()
        .find(|step| step.order == 2)
        .expect("step 2");
    assert_eq!(second.depends_on, vec![first.id.clone()]);
    assert_eq!(second.state, athena_types::PlanStepState::Pending);

    // The confirmation Event carries the catalog's four mandatory keys.
    let events = crate::EventRepository::list_for_task(&store, &task.id).expect("events");
    let created = events
        .iter()
        .find(|event| event.event_type == "PlanCreated")
        .expect("PlanCreated");
    assert_eq!(created.event_id, accepted.event_id);
    let payload: serde_json::Value =
        serde_json::from_str(&created.payload_json).expect("payload json");
    assert_eq!(payload["plan_id"], serde_json::json!(plan.id.as_str()));
    assert_eq!(payload["revision"], serde_json::json!(1));
    assert_eq!(payload["step_count"], serde_json::json!(2));
    assert_eq!(payload["rationale"], serde_json::json!("ship the intake"));
    assert_eq!(created.actor, "system");
}

#[test]
fn a_second_submission_bumps_the_revision_and_records_plan_revised() {
    // ATH-PLAN-004: the old revision is preserved, never overwritten; the new one's
    // confirmation is `PlanRevised` (ATH-PLAN-005).
    let task = task_row(Some("ship the intake"));
    let mut store = store_with(&task);
    accept_plan(
        &mut store,
        &task.id,
        0,
        vec![draft(1, "first", vec![])],
        NOW,
    )
    .expect("the first lands");

    let revised = accept_plan(
        &mut store,
        &task.id,
        1,
        vec![draft(1, "first", vec![]), draft(2, "second", vec![])],
        NOW,
    )
    .expect("the revision lands");

    assert_eq!(revised.revision, 2);
    assert!(!revised.first_revision);
    assert_eq!(
        crate::PlanRepository::revisions_for_task(&store, &task.id).expect("revisions"),
        vec![1, 2],
        "the earlier revision is kept (ATH-PLAN-004)"
    );
    let events = crate::EventRepository::list_for_task(&store, &task.id).expect("events");
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "PlanRevised")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "PlanCreated")
            .count(),
        1,
        "the first plan's own confirmation stands unchanged"
    );
}

#[test]
fn a_submission_never_settles_its_task() {
    // The conflation X-43 ends: before this path, a plan submission was ingested as the
    // Task's answer and settled it. The intake touches no Task state.
    let task = task_row(Some("ship the intake"));
    let mut store = store_with(&task);
    accept_plan(&mut store, &task.id, 0, vec![draft(1, "only", vec![])], NOW).expect("lands");

    let after = crate::TaskRepository::get(&store, &task.id)
        .expect("read")
        .expect("row");
    assert_eq!(
        after.state,
        TaskState::Running,
        "the Task is still running — a plan is the work's shape, not its product"
    );
    assert!(after.finished_at.is_none());
    let events = crate::EventRepository::list_for_task(&store, &task.id).expect("events");
    assert!(
        !events
            .iter()
            .any(|event| event.event_type == "TaskCompleted" || event.event_type == "TaskFailed"),
        "no settlement Event rides a plan submission"
    );
}

#[test]
fn a_task_without_a_goal_takes_its_rationale_from_the_instruction() {
    // The catalog makes `rationale` mandatory while `submit_plan` carries none, so it is
    // read from the Task row — the goal when stated, else the instruction — and which one
    // was used is reported rather than left to guesswork.
    let task = task_row(None);
    let mut store = store_with(&task);

    let accepted =
        accept_plan(&mut store, &task.id, 0, vec![draft(1, "only", vec![])], NOW).expect("lands");

    assert_eq!(accepted.rationale_source, RationaleSource::TaskInstruction);
    let events = crate::EventRepository::list_for_task(&store, &task.id).expect("events");
    let created = events
        .iter()
        .find(|event| event.event_type == "PlanCreated")
        .expect("PlanCreated");
    let payload: serde_json::Value =
        serde_json::from_str(&created.payload_json).expect("payload json");
    assert_eq!(
        payload["rationale"],
        serde_json::json!("wire the plan intake"),
        "the instruction stood in for the absent goal"
    );
}

#[test]
fn a_whitespace_goal_is_not_a_goal() {
    // A goal of blanks states nothing; the instruction carries the rationale instead.
    let task = task_row(Some("   "));
    let mut store = store_with(&task);
    let accepted =
        accept_plan(&mut store, &task.id, 0, vec![draft(1, "only", vec![])], NOW).expect("lands");
    assert_eq!(accepted.rationale_source, RationaleSource::TaskInstruction);
}

#[test]
fn a_stale_base_revision_is_refused_and_records_nothing() {
    // ATH-CORE-042: the revision Core Host holds is the only base a submission may edit
    // against. The refusal carries the current revision so the agent can re-base, and no
    // Event is appended for a submission that did not land.
    let task = task_row(Some("ship the intake"));
    let mut store = store_with(&task);
    accept_plan(
        &mut store,
        &task.id,
        0,
        vec![draft(1, "first", vec![])],
        NOW,
    )
    .expect("lands");
    let events_before = crate::EventRepository::list_for_task(&store, &task.id)
        .expect("events")
        .len();

    let fault = accept_plan(
        &mut store,
        &task.id,
        0,
        vec![draft(1, "again", vec![])],
        NOW,
    )
    .expect_err("a stale base is refused");

    assert_eq!(fault.class(), crate::protocol::ErrorClass::ValidationError);
    assert_eq!(fault.code(), "PLAN_BASE_REVISION_CONFLICT");
    let response = crate::plan_intake::refusal_response("req-1", "cor-x43", &fault);
    match response {
        crate::protocol::CoreResponse::Err { error, .. } => {
            assert_eq!(error.code, "PLAN_BASE_REVISION_CONFLICT");
            assert_eq!(error.correlation_id, "cor-x43");
            assert_eq!(
                error.details["current_revision"],
                serde_json::json!(1),
                "the envelope hands back the revision to re-base on"
            );
        }
        other => panic!("a refusal must answer Err: {other:?}"),
    }
    assert_eq!(
        crate::EventRepository::list_for_task(&store, &task.id)
            .expect("events")
            .len(),
        events_before,
        "a refused submission appends no confirmation"
    );
    assert_eq!(
        crate::PlanRepository::revisions_for_task(&store, &task.id).expect("revisions"),
        vec![1],
        "and writes no plan row"
    );
}

#[test]
fn a_cyclic_plan_is_refused_before_anything_is_written() {
    // ATH-PLAN-006: the cycle is caught at creation. Untangling it is the agent's call,
    // so the refusal is a validation fault, not a Core-side repair.
    let task = task_row(Some("ship the intake"));
    let mut store = store_with(&task);

    let fault = accept_plan(
        &mut store,
        &task.id,
        0,
        vec![draft(1, "a", vec![2]), draft(2, "b", vec![1])],
        NOW,
    )
    .expect_err("a cycle is refused");

    assert_eq!(fault.code(), "PLAN_DEPENDENCY_CYCLE");
    assert_eq!(fault.class(), crate::protocol::ErrorClass::ValidationError);
    assert!(
        crate::PlanRepository::latest_for_task(&store, &task.id)
            .expect("read")
            .is_none(),
        "nothing was written"
    );
}

#[test]
fn a_submission_for_an_absent_task_is_refused() {
    // The Task is the plan's owner and the rationale's source; without the row there is
    // neither, so the intake refuses instead of inventing either.
    let mut store = crate::AthenaStore::in_memory().expect("store");
    let fault = accept_plan(
        &mut store,
        &TaskId::new(),
        0,
        vec![draft(1, "only", vec![])],
        NOW,
    )
    .expect_err("an absent task is refused");
    assert!(matches!(fault, IntakeFault::TaskAbsent(_)));
    assert_eq!(fault.code(), "TASK_NOT_FOUND");
}

#[test]
fn the_acceptance_answer_names_what_core_host_minted() {
    // ATH-CORE-034's answer leg: the agent receives the identifiers it may reference and
    // the revision its next edit must base on, wrapped as control traffic on the Task's
    // own delegation thread (ATH-MSG-004/011).
    let task = task_row(Some("ship the intake"));
    let mut store = store_with(&task);
    let accepted =
        accept_plan(&mut store, &task.id, 0, vec![draft(1, "only", vec![])], NOW).expect("lands");

    let response = crate::plan_intake::acceptance_response(accepted.event_id.as_str(), &accepted);
    match &response {
        crate::protocol::CoreResponse::Ok { request_id, data } => {
            assert_eq!(request_id, accepted.event_id.as_str());
            assert_eq!(
                data["plan_id"],
                serde_json::json!(accepted.plan_id.as_str())
            );
            assert_eq!(data["revision"], serde_json::json!(1));
            assert_eq!(data["first_revision"], serde_json::json!(true));
        }
        other => panic!("an acceptance must answer Ok: {other:?}"),
    }

    let message =
        crate::plan_intake::control_response_message(&task.id, &response).expect("message");
    assert_eq!(
        message.message_type,
        athena_types::MessageType::ControlResponse
    );
    assert_eq!(message.thread_id, crate::core_loop::core_thread(&task.id));
    assert!(
        message.from_agent.is_none(),
        "Core is transport, not an endpoint"
    );
    // `CoreResponse` carries no serde tag attribute, so its wire form is the externally
    // tagged default: the variant name keys the object. The assertion reads it as it is
    // rather than assuming a flattened shape.
    let payload: serde_json::Value =
        serde_json::from_str(message.payload_json.as_deref().expect("payload")).expect("json");
    assert_eq!(
        payload["Ok"]["request_id"],
        serde_json::json!(accepted.event_id.as_str()),
        "{payload}"
    );
    assert_eq!(
        payload["Ok"]["data"]["plan_id"],
        serde_json::json!(accepted.plan_id.as_str())
    );
}

#[test]
fn the_confirmation_rides_the_tasks_own_correlation_chain() {
    // ATH-OBS-009: the plan's record joins the Task's chain, so one key walks the Task,
    // its Events and its logs. The chain's last non-empty correlation is adopted.
    let task = task_row(Some("ship the intake"));
    let mut store = store_with(&task);
    crate::EventRepository::append(
        &mut store,
        &crate::NewEvent {
            event_id: athena_types::EventId::new(),
            schema_version: 1,
            event_type: "TaskStarted".into(),
            occurred_at: "2026-11-12T00:30:00.000Z".into(),
            task_id: Some(task.id.clone()),
            plan_step_id: None,
            agent_id: None,
            instance_id: None,
            skill_id: None,
            correlation_id: "cor-x43-chain".into(),
            causation_id: None,
            actor: "system".into(),
            payload_json: "{}".into(),
        },
    )
    .expect("chain row");

    accept_plan(&mut store, &task.id, 0, vec![draft(1, "only", vec![])], NOW).expect("lands");

    let events = crate::EventRepository::list_for_task(&store, &task.id).expect("events");
    let created = events
        .iter()
        .find(|event| event.event_type == "PlanCreated")
        .expect("PlanCreated");
    assert_eq!(created.correlation_id, "cor-x43-chain");
}
