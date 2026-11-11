//! The Instance `.aiprj` area sweep (X-41, ATH-AIPRJ-029) and its recognition fence, plus
//! the approval expiry step (X-45, ATH-HITL-005 / ATH-STATE-011 / §17.4).
//!
//! Every area test builds a real directory tree in the system temp area — the fence's
//! whole point is that it reads the disk, so a mocked filesystem would verify nothing. The
//! rows are real store rows for the same reason: the fence's first condition is an
//! agreement between a row and a directory. The approval tests are store rows throughout
//! as well: the step's facts are the §17.4 state and the two deadline stamps.

use crate::maintenance::aiprj_area_step;
use athena_types::{
    AgentDefinition, AgentId, AgentInstance, AgentInstanceState, InstanceId, ModelRef, RuntimeType,
    ToolName,
};

/// A scratch instances root, unique per test.
fn scratch_root(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "athena-x41-{tag}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("scratch root");
    root
}

/// The shape `init_workspace` leaves behind: `<root>/<instance>/.aiprj/AI_LOG/` with a log
/// file in it, plus the `.agent-cli` sibling an Instance Workspace also carries.
fn seed_area(root: &std::path::Path, instance: &InstanceId) -> std::path::PathBuf {
    let workspace = root.join(instance.as_str());
    let area = workspace.join(".aiprj");
    std::fs::create_dir_all(area.join("AI_LOG")).expect("area");
    std::fs::write(area.join("AI_PRJ_TASKS.md"), "").expect("tasks file");
    std::fs::write(area.join("AI_LOG").join("2026-11-12_001.md"), "run log\n").expect("log");
    std::fs::create_dir_all(workspace.join(".agent-cli")).expect("agent-cli");
    area
}

fn definition() -> AgentDefinition {
    AgentDefinition {
        id: AgentId::new("x41-worker"),
        name: "X41 Worker".into(),
        level: athena_types::AgentLevel::Worker,
        description: "the definition the area-sweep rows reference".into(),
        capabilities: vec![],
        persona_ref: "worker".into(),
        persona_version: "1.0.0".into(),
        allowed_tools: vec![ToolName::FsRead],
        delegable_tools: vec![],
        default_skills: vec![],
        runtime: RuntimeType::AgentCli,
        model: ModelRef {
            provider: "anthropic".into(),
            name: "claude-opus-5".into(),
        },
        max_delegation_depth: 4,
        dynamic: false,
    }
}

/// One instance row, parameterised on the three facts the sweep consults.
fn instance_row(
    workspace: &std::path::Path,
    state: AgentInstanceState,
    finished_at: Option<&str>,
    aiprj_log_ref: Option<String>,
) -> AgentInstance {
    AgentInstance {
        id: InstanceId::new(),
        agent_id: AgentId::new("x41-worker"),
        task_id: None,
        plan_step_id: None,
        superior_instance_id: None,
        delegation_depth: athena_types::AgentLevel::Worker.depth(),
        state,
        runtime_instance_id: None,
        workspace: workspace.display().to_string(),
        skill_set: vec![],
        aiprj_log_ref,
        attempt: 1,
        started_at: Some("2026-10-01T00:00:00.000Z".into()),
        finished_at: finished_at.map(str::to_string),
    }
}

fn store_with(rows: &[AgentInstance]) -> crate::AthenaStore {
    let mut store = crate::AthenaStore::in_memory().expect("store");
    crate::AgentRepository::upsert_definition(&mut store, &definition()).expect("definition");
    for row in rows {
        crate::AgentRepository::insert_instance(&mut store, row).expect("instance row");
    }
    store
}

/// The cutoff every test shares: rows finished before this are aged out.
const CUTOFF: &str = "2026-11-01T00:00:00.000Z";

#[test]
fn an_aged_out_terminal_instance_loses_its_area_and_the_row_records_it() {
    // ATH-AIPRJ-029's own case: a terminal instance whose finish stamp predates the Event
    // retention horizon, whose workspace is exactly `<instances_root>/<id>` — the layout
    // `init_workspace` creates. The area goes; the sibling `.agent-cli` and the workspace
    // itself stay (the rule names the `aiprj` area alone).
    let root = scratch_root("delete");
    let id = InstanceId::new();
    let area = seed_area(&root, &id);
    let mut row = instance_row(
        &root.join(id.as_str()),
        AgentInstanceState::Stopped,
        Some("2026-10-02T00:00:00.000Z"),
        None,
    );
    row.id = id.clone();
    let mut store = store_with(&[row]);

    let (deleted, skipped) = aiprj_area_step(&mut store, CUTOFF, &root).expect("the step runs");

    assert_eq!(
        (deleted, skipped),
        (1, 0),
        "the area is recognized and gone"
    );
    assert!(!area.exists(), "the .aiprj area is deleted");
    assert!(
        root.join(id.as_str()).join(".agent-cli").is_dir(),
        "only the aiprj area is swept — the rest of the Workspace stays"
    );
    let after = crate::AgentRepository::get_instance(&store, &id)
        .expect("read")
        .expect("row");
    assert_eq!(
        after.aiprj_log_ref, None,
        "the cleared reference is how the execution records itself"
    );

    // Idempotence: a second cycle finds nothing to delete and invents no failure.
    let (deleted, skipped) = aiprj_area_step(&mut store, CUTOFF, &root).expect("the step reruns");
    assert_eq!(
        (deleted, skipped),
        (0, 1),
        "the absent area is skipped, not re-deleted"
    );
}

#[test]
fn a_recorded_area_reference_is_followed_rather_than_reconstructed() {
    // The forward path (ATH-EVENT-016 / ATH-OBS-009): when the row carries the `AI_LOG`
    // directory the Runtime wrote, the sweep uses it — a workspace string that would not
    // reconstruct (here: a relative one) does not block the deletion.
    let root = scratch_root("recorded");
    let id = InstanceId::new();
    let area = seed_area(&root, &id);
    let mut row = instance_row(
        std::path::Path::new("./relative/not-reconstructible"),
        AgentInstanceState::Stopped,
        Some("2026-10-03T00:00:00.000Z"),
        Some(area.join("AI_LOG").display().to_string()),
    );
    row.id = id.clone();
    let mut store = store_with(&[row]);

    let (deleted, skipped) = aiprj_area_step(&mut store, CUTOFF, &root).expect("the step runs");

    assert_eq!(
        (deleted, skipped),
        (1, 0),
        "the recorded path is authoritative"
    );
    assert!(!area.exists());
}

#[test]
fn a_live_or_recent_instance_is_not_a_candidate_at_all() {
    // ATH-AIPRJ-026: the area outlives the terminal state, and the retention horizon is
    // the Event one. A busy row and a terminal-but-recent row are both left whole, and
    // neither counts as skipped — they are not candidates in the first place.
    let root = scratch_root("young");
    let live = InstanceId::new();
    let recent = InstanceId::new();
    let live_area = seed_area(&root, &live);
    let recent_area = seed_area(&root, &recent);
    let mut live_row = instance_row(
        &root.join(live.as_str()),
        AgentInstanceState::Busy,
        None,
        None,
    );
    live_row.id = live.clone();
    let mut recent_row = instance_row(
        &root.join(recent.as_str()),
        AgentInstanceState::Stopped,
        Some("2026-11-11T00:00:00.000Z"),
        None,
    );
    recent_row.id = recent.clone();
    let mut store = store_with(&[live_row, recent_row]);

    let (deleted, skipped) = aiprj_area_step(&mut store, CUTOFF, &root).expect("the step runs");

    assert_eq!((deleted, skipped), (0, 0));
    assert!(live_area.is_dir(), "a live instance keeps its area");
    assert!(
        recent_area.is_dir(),
        "a terminal row inside the horizon keeps its area"
    );
}

#[test]
fn the_fence_refuses_a_workspace_outside_the_instances_root() {
    // The condition X-41's deferral was about: a row whose workspace points somewhere
    // else must never cause a deletion there. The directory survives and the refusal is
    // reported as skipped rather than silently passing as success.
    let root = scratch_root("outside");
    let elsewhere = scratch_root("elsewhere");
    let id = InstanceId::new();
    let foreign_area = elsewhere.join(".aiprj");
    std::fs::create_dir_all(foreign_area.join("AI_LOG")).expect("foreign area");
    let mut row = instance_row(
        &elsewhere,
        AgentInstanceState::Stopped,
        Some("2026-10-04T00:00:00.000Z"),
        None,
    );
    row.id = id;
    let mut store = store_with(&[row]);

    let (deleted, skipped) = aiprj_area_step(&mut store, CUTOFF, &root).expect("the step runs");

    assert_eq!((deleted, skipped), (0, 1), "the foreign area is refused");
    assert!(
        foreign_area.is_dir(),
        "a directory outside the instances root is never deleted"
    );
}

#[test]
fn the_fence_refuses_a_recorded_path_that_escapes_the_root() {
    // The recorded path is trusted for *which* area, never for *where*: a reference
    // pointing out of the instances tree still fails condition 3.
    let root = scratch_root("escape");
    let elsewhere = scratch_root("escape-target");
    let foreign_area = elsewhere.join(".aiprj");
    std::fs::create_dir_all(foreign_area.join("AI_LOG")).expect("foreign area");
    let mut row = instance_row(
        &root.join("unused"),
        AgentInstanceState::Stopped,
        Some("2026-10-05T00:00:00.000Z"),
        Some(foreign_area.join("AI_LOG").display().to_string()),
    );
    row.id = InstanceId::new();
    let mut store = store_with(&[row]);

    let (deleted, skipped) = aiprj_area_step(&mut store, CUTOFF, &root).expect("the step runs");

    assert_eq!((deleted, skipped), (0, 1));
    assert!(
        foreign_area.is_dir(),
        "the escape is refused, the directory stays"
    );
}

#[test]
fn the_fence_refuses_a_directory_that_is_not_the_shape_athena_creates() {
    // Condition 4: an `.aiprj` directory without `AI_LOG/` is not what `init_workspace`
    // leaves, so the sweep does not claim it. (Someone else's `.aiprj` under a path that
    // happens to match the layout is exactly the case this protects.)
    let root = scratch_root("shape");
    let id = InstanceId::new();
    let area = root.join(id.as_str()).join(".aiprj");
    std::fs::create_dir_all(&area).expect("bare area");
    std::fs::write(area.join("instructions.md"), "someone else's\n").expect("file");
    let mut row = instance_row(
        &root.join(id.as_str()),
        AgentInstanceState::Stopped,
        Some("2026-10-06T00:00:00.000Z"),
        None,
    );
    row.id = id;
    let mut store = store_with(&[row]);

    let (deleted, skipped) = aiprj_area_step(&mut store, CUTOFF, &root).expect("the step runs");

    assert_eq!((deleted, skipped), (0, 1));
    assert!(area.is_dir(), "an unrecognized .aiprj is left alone");
}

#[test]
fn an_absent_instances_root_sweeps_nothing() {
    // The resident passes `<ATHENA_HOME>/var/instances` before anything has ever run
    // there: no tree, no deletions, no fault.
    let root = scratch_root("absent").join("never-created");
    let mut store = store_with(&[]);
    let (deleted, skipped) = aiprj_area_step(&mut store, CUTOFF, &root).expect("the step runs");
    assert_eq!((deleted, skipped), (0, 0));
}

#[test]
fn the_full_cycle_reports_the_area_counts_and_records_them() {
    // The step rides `sweep_once` as step 5, and `record_swept` carries both counts into
    // the audit Event's payload (ATH-AIPRJ-029's 実施を記録する face).
    let root = scratch_root("cycle");
    let id = InstanceId::new();
    seed_area(&root, &id);
    let mut row = instance_row(
        &root.join(id.as_str()),
        AgentInstanceState::Stopped,
        Some("2026-10-07T00:00:00.000Z"),
        None,
    );
    row.id = id;
    let mut store = store_with(&[row]);

    let report = crate::maintenance::sweep_once(
        &mut store,
        "2026-11-12T00:00:00.000Z",
        CUTOFF,
        CUTOFF,
        CUTOFF,
        Some(root.as_path()),
    )
    .expect("the cycle runs");
    assert_eq!(report.aiprj_areas_deleted, 1);
    assert_eq!(report.aiprj_areas_skipped, 0);

    crate::maintenance::record_swept(
        &mut store,
        &report,
        "2026-11-12T00:00:00.000Z",
        "cor-x41-cycle",
    )
    .expect("the audit records");
    let events = crate::EventRepository::list_since(&store, 0, u64::MAX).expect("events");
    let audit = events
        .iter()
        .find(|event| event.correlation_id == "cor-x41-cycle")
        .expect("the audit Event");
    assert!(
        audit.payload_json.contains("\"aiprj_areas_deleted\":1"),
        "{}",
        audit.payload_json
    );
    assert!(
        audit.payload_json.contains("\"aiprj_areas_skipped\":0"),
        "{}",
        audit.payload_json
    );
}

// --- The approval expiry step (X-45, §17.4's two expiry edges) ----------------------------

/// The approval fixtures' shared clock: the deadline sits between the two stamps, so one
/// `now` is "before the deadline" and the other is "after" without any arithmetic.
const APPROVAL_DEADLINE: &str = "2026-11-12T01:00:00.000Z";
const BEFORE_DEADLINE: &str = "2026-11-12T00:30:00.000Z";
const AFTER_DEADLINE: &str = "2026-11-12T01:30:00.000Z";

/// One Task row in the state the §17.1 approval edge needs.
fn approval_task(state: athena_types::TaskState) -> athena_types::Task {
    athena_types::Task {
        id: athena_types::TaskId::new(),
        parent_id: None,
        title: "X45".into(),
        instruction: "the operation awaiting a human".into(),
        workspace: "/work/x45".into(),
        goal: None,
        constraints: vec![],
        completion_criteria: vec![],
        state,
        priority: athena_types::Priority::Normal,
        plan_id: None,
        risk_level: athena_types::RiskLevel::High,
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

/// A `REQUESTED` approval through the production staging path, with the deadline the
/// caller names (`ATH-HITL-004`).
fn seed_requested(
    store: &mut crate::AthenaStore,
    task: &athena_types::TaskId,
    expires_at: &str,
    scope: athena_types::ApprovalScope,
) -> athena_types::Approval {
    crate::approval_manager::staging::create(
        store,
        &athena_types::ApprovalId::new(),
        task,
        &athena_types::AgentId::core_agent(),
        &athena_types::InstanceId::new(),
        "shell git push",
        "tool=shell target=- args={} risk=high instance=-".into(),
        "agent prose, untrusted",
        "agent prose, untrusted",
        athena_types::RiskLevel::High,
        expires_at.into(),
        scope,
    )
    .expect("the approval stages")
}

/// The same row moved to `GRANTED` with uses left — the state edge 2 starts from.
fn grant(store: &mut crate::AthenaStore, approval: &athena_types::Approval, uses: u64) {
    crate::ApprovalRepository::update_state(
        store,
        &approval.id,
        athena_types::ApprovalState::Granted,
        Some("admin rt-3"),
        uses,
    )
    .expect("the grant lands");
}

fn approval_state(
    store: &crate::AthenaStore,
    id: &athena_types::ApprovalId,
) -> athena_types::ApprovalState {
    crate::ApprovalRepository::get(store, id)
        .expect("read")
        .expect("row")
        .state
}

fn expired_events(
    store: &crate::AthenaStore,
    task: &athena_types::TaskId,
) -> Vec<athena_persistence::StoredEvent> {
    crate::EventRepository::list_for_task(store, task)
        .expect("chain")
        .into_iter()
        .filter(|event| event.event_type == "ApprovalExpired")
        .collect()
}

#[test]
fn an_approval_inside_its_deadline_is_left_alone() {
    // ATH-HITL-005 is about *silence past the deadline*. A row still inside its window is
    // not a candidate: no state change, no Event, no count.
    let mut store = crate::AthenaStore::in_memory().expect("store");
    let task = approval_task(athena_types::TaskState::WaitingApproval);
    crate::TaskRepository::insert(&mut store, &task).expect("task seeds");
    let approval = seed_requested(
        &mut store,
        &task.id,
        APPROVAL_DEADLINE,
        athena_types::ApprovalScope::OperationOnly {
            operation_only: true,
        },
    );

    let expired = crate::maintenance::approval_expiry_step(&mut store, BEFORE_DEADLINE)
        .expect("the step runs");

    assert_eq!(expired, 0);
    assert_eq!(
        approval_state(&store, &approval.id),
        athena_types::ApprovalState::Requested
    );
    assert!(expired_events(&store, &task.id).is_empty());
    let after = crate::TaskRepository::get(&store, &task.id)
        .expect("read")
        .expect("row");
    assert_eq!(after.state, athena_types::TaskState::WaitingApproval);
}

#[test]
fn a_requested_approval_past_its_deadline_expires_as_a_refusal() {
    // §17.4's `REQUESTED → EXPIRED` edge (actor: Approval Manager) with the Task's §17.1
    // `WAITING_APPROVAL + ApprovalExpired → FAILED` row walked by the same Event: the
    // default is refusal (ATH-HITL-005) and `EXPIRED` counts as one (ATH-STATE-011).
    let mut store = crate::AthenaStore::in_memory().expect("store");
    let task = approval_task(athena_types::TaskState::WaitingApproval);
    crate::TaskRepository::insert(&mut store, &task).expect("task seeds");
    let approval = seed_requested(
        &mut store,
        &task.id,
        APPROVAL_DEADLINE,
        athena_types::ApprovalScope::OperationOnly {
            operation_only: true,
        },
    );

    let expired = crate::maintenance::approval_expiry_step(&mut store, AFTER_DEADLINE)
        .expect("the step runs");

    assert_eq!(expired, 1);
    assert_eq!(
        approval_state(&store, &approval.id),
        athena_types::ApprovalState::Expired
    );
    let row = crate::ApprovalRepository::get(&store, &approval.id)
        .expect("read")
        .expect("row");
    assert_eq!(row.uses_remaining, 0, "a refused approval carries no uses");
    assert_eq!(
        row.responded_by, None,
        "silence has no approver — none is invented (ATH-HITL-006)"
    );
    let failed = crate::TaskRepository::get(&store, &task.id)
        .expect("read")
        .expect("row");
    assert_eq!(
        failed.state,
        athena_types::TaskState::Failed,
        "the waiting Task does not wait forever (§17.1)"
    );

    // The Event is the audit record, with the catalog's required keys (ATH-EVENT-017).
    let events = expired_events(&store, &task.id);
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.actor, "system", "the Approval Manager's §17.4 actor");
    assert_eq!(event.occurred_at, AFTER_DEADLINE);
    let wire: serde_json::Value =
        serde_json::from_str(&event.payload_json).expect("payload parses");
    for key in <athena_events::events::ApprovalExpired as athena_events::Payload>::REQUIRED_FIELDS {
        assert!(
            wire.get(*key).is_some_and(|value| !value.is_null()),
            "the catalog key {key} is missing from {}",
            event.payload_json
        );
    }
    let payload: athena_events::events::ApprovalExpired =
        serde_json::from_str(&event.payload_json).expect("wire-exact payload");
    assert_eq!(payload.approval_id, approval.id);
    assert_eq!(payload.scope, row.scope);

    // Idempotence: the row left the `REQUESTED` candidate set, so a second cycle writes
    // no second Event.
    let again = crate::maintenance::approval_expiry_step(&mut store, AFTER_DEADLINE)
        .expect("the step reruns");
    assert_eq!(again, 0);
    assert_eq!(expired_events(&store, &task.id).len(), 1);
}

#[test]
fn a_granted_scope_lapses_before_it_is_consumed() {
    // §17.4's second expiry edge: `GRANTED → EXPIRED` when `scope.expires_at`
    // (ATH-HITL-013) arrives with uses still unconsumed. The Task is `RUNNING` here —
    // the grant already released it — so the Event is the record alone.
    let mut store = crate::AthenaStore::in_memory().expect("store");
    let task = approval_task(athena_types::TaskState::Running);
    crate::TaskRepository::insert(&mut store, &task).expect("task seeds");
    let approval = seed_requested(
        &mut store,
        &task.id,
        APPROVAL_DEADLINE,
        athena_types::ApprovalScope::Pattern {
            pattern: "git_push_to_origin_main".into(),
            expires_at: APPROVAL_DEADLINE.into(),
        },
    );
    grant(&mut store, &approval, 3);

    let expired = crate::maintenance::approval_expiry_step(&mut store, AFTER_DEADLINE)
        .expect("the step runs");

    assert_eq!(expired, 1);
    assert_eq!(
        approval_state(&store, &approval.id),
        athena_types::ApprovalState::Expired
    );
    let events = expired_events(&store, &task.id);
    assert_eq!(events.len(), 1);
    assert_eq!(
        crate::TaskRepository::get(&store, &task.id)
            .expect("read")
            .expect("row")
            .state,
        athena_types::TaskState::Running,
        "a Task that is not waiting on the approval is not re-decided here"
    );

    // Idempotence on this edge too: `EXPIRED` is terminal, so the granted-unconsumed
    // query no longer answers with it.
    let again = crate::maintenance::approval_expiry_step(&mut store, AFTER_DEADLINE)
        .expect("the step reruns");
    assert_eq!(again, 0);
    assert_eq!(expired_events(&store, &task.id).len(), 1);
}

#[test]
fn a_granted_scope_still_in_its_window_keeps_its_uses() {
    // The counterpart of edge 2: a live scope stamp, and an `operation_only` grant, which
    // carries no `scope.expires_at` at all — the step dates neither of them.
    let mut store = crate::AthenaStore::in_memory().expect("store");
    let task = approval_task(athena_types::TaskState::Running);
    crate::TaskRepository::insert(&mut store, &task).expect("task seeds");
    let live = seed_requested(
        &mut store,
        &task.id,
        APPROVAL_DEADLINE,
        athena_types::ApprovalScope::Pattern {
            pattern: "git_push_to_origin_main".into(),
            expires_at: AFTER_DEADLINE.into(),
        },
    );
    grant(&mut store, &live, 2);
    let operation_only = seed_requested(
        &mut store,
        &task.id,
        APPROVAL_DEADLINE,
        athena_types::ApprovalScope::OperationOnly {
            operation_only: true,
        },
    );
    grant(&mut store, &operation_only, 1);

    let expired = crate::maintenance::approval_expiry_step(&mut store, BEFORE_DEADLINE)
        .expect("the step runs");

    assert_eq!(expired, 0);
    assert_eq!(
        approval_state(&store, &live.id),
        athena_types::ApprovalState::Granted
    );
    assert_eq!(
        approval_state(&store, &operation_only.id),
        athena_types::ApprovalState::Granted
    );
    assert!(expired_events(&store, &task.id).is_empty());
}

#[test]
fn the_full_cycle_reports_the_expired_approvals_and_records_them() {
    // The step rides `sweep_once` as step 6, and `record_swept` carries the count into the
    // audit Event's payload (the five earlier steps' own flow).
    let mut store = crate::AthenaStore::in_memory().expect("store");
    let task = approval_task(athena_types::TaskState::WaitingApproval);
    crate::TaskRepository::insert(&mut store, &task).expect("task seeds");
    seed_requested(
        &mut store,
        &task.id,
        APPROVAL_DEADLINE,
        athena_types::ApprovalScope::OperationOnly {
            operation_only: true,
        },
    );

    let report =
        crate::maintenance::sweep_once(&mut store, AFTER_DEADLINE, CUTOFF, CUTOFF, CUTOFF, None)
            .expect("the cycle runs");
    assert_eq!(report.approvals_expired, 1);

    crate::maintenance::record_swept(&mut store, &report, AFTER_DEADLINE, "cor-x45-cycle")
        .expect("the audit records");
    let events = crate::EventRepository::list_since(&store, 0, u64::MAX).expect("events");
    let audit = events
        .iter()
        .find(|event| event.correlation_id == "cor-x45-cycle")
        .expect("the audit Event");
    assert!(
        audit.payload_json.contains("\"approvals_expired\":1"),
        "{}",
        audit.payload_json
    );
}
