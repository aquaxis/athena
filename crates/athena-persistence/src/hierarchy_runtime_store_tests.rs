//! Tests for the `HierarchyRepository` / `RuntimeInstanceRepository` on `SqliteStore`
//! (P1-05's closure): round trips, the revision walk, heartbeats, staleness, and the
//! stopped-marking contract (`ATH-CORE-047`, `ATH-CORE-048`).

use crate::SqliteStore;
use crate::repository::{HierarchyRepository, RuntimeInstanceRepository, TaskRepository};
use athena_types::{
    AgentId, AgentInstanceState, AgentLevel, Hierarchy, HierarchyId, HierarchyNode, InstanceId,
    RuntimeInstance, RuntimeInstanceId, RuntimeType, ScopeRef, SideEffects, StopReason, TaskId,
};

/// The `hierarchies.task_id` FK demands a Task row; seeding it keeps the tests honest.
fn seed_task(store: &mut SqliteStore, task_id: &TaskId) {
    let mut task = task_fixture();
    task.id = task_id.clone();
    TaskRepository::insert(store, &task).expect("task seeded");
}

fn task_fixture() -> athena_types::Task {
    athena_types::Task {
        id: TaskId::new(),
        parent_id: None,
        title: "FPGA timing fix".into(),
        instruction: "Analyse the timing violation and propose a fix.".into(),
        workspace: "/work/fpga-project".into(),
        goal: None,
        constraints: vec!["do not update the production bitstream".into()],
        completion_criteria: vec!["root cause identified".into()],
        state: athena_types::TaskState::Created,
        priority: athena_types::Priority::High,
        plan_id: None,
        risk_level: athena_types::RiskLevel::Medium,
        attempt_count: 0,
        max_attempts: 3,
        iteration_count: 0,
        max_iterations: 50,
        deadline: None,
        created_by: "hidemi".into(),
        created_at: "2026-10-18T00:00:00Z".into(),
        updated_at: "2026-10-18T00:00:00Z".into(),
        finished_at: None,
    }
}

fn hierarchy_fixture(task: &TaskId, revision: u64) -> Hierarchy {
    Hierarchy {
        id: HierarchyId::new(),
        task_id: task.clone(),
        revision,
        nodes: vec![
            HierarchyNode {
                agent_id: AgentId::new("producer"),
                level: AgentLevel::Producer,
                superior_agent_id: None,
                scope_ref: None,
                sibling_acl: vec![],
            },
            HierarchyNode {
                agent_id: AgentId::new("timing-lead"),
                level: AgentLevel::Lead,
                superior_agent_id: Some(AgentId::new("producer")),
                scope_ref: Some(ScopeRef::SubTask {
                    task_id: task.clone(),
                }),
                sibling_acl: vec![AgentId::new("synthesis-lead")],
            },
        ],
        created_at: "2026-10-18T00:00:00Z".into(),
    }
}

#[test]
fn a_hierarchy_round_trips_with_nodes_and_revisions() {
    let mut store = SqliteStore::in_memory().expect("store");
    let task = TaskId::new();
    seed_task(&mut store, &task);
    let fixture = hierarchy_fixture(&task, 1);
    let fixture_id = fixture.id.clone();
    HierarchyRepository::insert(&mut store, &fixture).expect("inserted");

    let row = HierarchyRepository::get(&store, &fixture_id)
        .expect("fetch")
        .expect("present");
    assert_eq!(row.task_id, task);
    assert_eq!(row.revision, 1);
    assert_eq!(row.nodes.len(), 2);
    let lead = &row.nodes[1];
    assert_eq!(
        lead.superior_agent_id.as_ref().map(|id| id.as_str()),
        Some("producer")
    );
    assert_eq!(
        lead.scope_ref,
        Some(ScopeRef::SubTask {
            task_id: task.clone()
        })
    );
    assert_eq!(lead.sibling_acl.len(), 1);

    // A fresh revision replaces the node set (a re-read gets revision 2, one node).
    let mut revised = hierarchy_fixture(&task, 2);
    revised.nodes.truncate(1);
    let revised_id = revised.id.clone();
    HierarchyRepository::insert(&mut store, &revised).expect("re-inserted");
    let latest = HierarchyRepository::latest_for_task(&store, &task)
        .expect("latest")
        .expect("present");
    assert_eq!(latest.id, revised_id);
    assert_eq!(latest.revision, 2);
    assert_eq!(latest.nodes.len(), 1);
}

#[test]
fn latest_for_task_answers_by_revision_even_out_of_insertion_order_and_absents() {
    let mut store = SqliteStore::in_memory().expect("store");
    let task = TaskId::new();
    seed_task(&mut store, &task);
    assert!(
        HierarchyRepository::latest_for_task(&store, &task)
            .expect("absent task, task present")
            .is_none()
    );
    HierarchyRepository::insert(&mut store, &hierarchy_fixture(&task, 1)).expect("inserted");
    HierarchyRepository::insert(&mut store, &hierarchy_fixture(&task, 3)).expect("inserted");
    HierarchyRepository::insert(&mut store, &hierarchy_fixture(&task, 2)).expect("inserted");
    let latest = HierarchyRepository::latest_for_task(&store, &task)
        .expect("latest")
        .expect("present");
    assert_eq!(latest.revision, 3);
}

fn runtime_fixture() -> RuntimeInstance {
    RuntimeInstance {
        id: RuntimeInstanceId::new(),
        runtime_type: RuntimeType::AgentCli,
        instance_id: InstanceId::new(),
        state: AgentInstanceState::Starting,
        pid: Some(4242),
        last_heartbeat_at: None,
        started_at: "2026-10-18T00:00:00Z".into(),
        stopped_at: None,
        stop_reason: None,
        exit_code: None,
        side_effects: SideEffects::None,
    }
}

#[test]
fn a_runtime_instance_round_trips_and_heartbeats_and_stops() {
    let mut store = SqliteStore::in_memory().expect("store");
    let fixture = runtime_fixture();
    RuntimeInstanceRepository::insert(&mut store, &fixture).expect("inserted");

    let row = RuntimeInstanceRepository::get(&store, &fixture.id)
        .expect("fetch")
        .expect("present");
    assert_eq!(row.instance_id, fixture.instance_id);
    assert_eq!(row.runtime_type, RuntimeType::AgentCli);
    assert_eq!(row.state, AgentInstanceState::Starting);
    assert_eq!(row.pid, Some(4242));
    assert_eq!(row.side_effects, SideEffects::None);

    RuntimeInstanceRepository::record_heartbeat(&mut store, &fixture.id, "2026-10-18T00:00:20Z")
        .expect("heartbeat");
    RuntimeInstanceRepository::mark_stopped(
        &mut store,
        &fixture.id,
        StopReason::Cancelled,
        Some(-9),
        SideEffects::Unknown,
    )
    .expect("marked");
    let row = RuntimeInstanceRepository::get(&store, &fixture.id)
        .expect("fetch")
        .expect("present");
    assert_eq!(row.state, AgentInstanceState::Stopped);
    assert_eq!(row.stop_reason, Some(StopReason::Cancelled));
    assert_eq!(row.exit_code, Some(-9));
    assert_eq!(row.side_effects, SideEffects::Unknown);
    assert!(row.stopped_at.is_some());
    // A stopped row accepts no more heartbeats (`stopped_at IS NULL` gate).
    assert!(
        RuntimeInstanceRepository::record_heartbeat(
            &mut store,
            &fixture.id,
            "2026-10-18T00:00:40Z"
        )
        .is_err()
    );
}

#[test]
fn staleness_names_live_rows_only() {
    let mut store = SqliteStore::in_memory().expect("store");
    let beat = runtime_fixture();
    RuntimeInstanceRepository::insert(&mut store, &beat).expect("inserted");
    RuntimeInstanceRepository::record_heartbeat(&mut store, &beat.id, "2026-10-18T00:00:00Z")
        .expect("heartbeat");
    let beat_id = beat.id.clone();

    let silent = runtime_fixture();
    let silent_id = silent.id.clone();
    RuntimeInstanceRepository::insert(&mut store, &silent).expect("inserted (no heartbeat)");

    let mut finished = runtime_fixture();
    finished.last_heartbeat_at = Some("2026-10-18T00:00:00Z".into());
    RuntimeInstanceRepository::insert(&mut store, &finished).expect("inserted");
    RuntimeInstanceRepository::mark_stopped(
        &mut store,
        &finished.id,
        StopReason::Completed,
        Some(0),
        SideEffects::Applied,
    )
    .expect("marked");

    // Rows whose last beat is before the cutoff, and never-beating live rows, are
    // stale; a stopped row is out of the set no matter its stamps.
    let stale =
        RuntimeInstanceRepository::list_stale(&store, "2026-10-18T00:00:40Z").expect("stale list");
    let stale_ids: Vec<String> = stale
        .iter()
        .map(|instance| instance.id.as_str().into())
        .collect();
    assert!(stale_ids.contains(&beat_id.as_str().to_owned()));
    assert!(stale_ids.contains(&silent_id.as_str().to_owned()));
    assert!(!stale.iter().any(|instance| instance.stopped_at.is_some()));
    // A beat not older than the cutoff is not stale (the lapse check is `<`).
    let fresh =
        RuntimeInstanceRepository::list_stale(&store, "2026-10-18T00:00:00Z").expect("fresh list");
    assert!(
        !fresh
            .iter()
            .any(|instance| instance.id.as_str() == beat_id.as_str() || {
                let _ = &silent_id;
                false
            })
    );
}
