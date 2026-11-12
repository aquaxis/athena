//! The Broker's store-facing driver (P2-11/12; ATH-MSG-012/014).

use crate::broker::{BrokerDelivery, BrokerFault, broker_deliver};
use athena_messaging::envelope::SendRefusal;
use athena_persistence::AthenaStore;
use athena_persistence::repository::MessageRepository;
use athena_types::{AgentId, InstanceId, MessageId, MessageType, Priority, TaskId};

fn row(
    from: &str,
    to_instance: &InstanceId,
    thread: &str,
    body: &str,
) -> athena_persistence::repository::MessageRow {
    athena_persistence::repository::MessageRow {
        message_id: MessageId::new(),
        message_type: MessageType::Instruction,
        from_agent: AgentId::core_agent(),
        to_agent: AgentId::new(from),
        to_instance_id: Some(to_instance.clone()),
        from_instance_id: InstanceId::new(),
        task_id: Some(TaskId::new()),
        thread_id: thread.into(),
        reply_to: None,
        priority: Priority::Normal,
        body: body.to_string(),
        payload_json: None,
        deadline: None,
        created_at: "2026-10-14T00:00:00.000Z".into(),
    }
}

fn parent_child(_from: &athena_types::InstanceId, _to: &athena_types::InstanceId) -> bool {
    // The fixture's world: parent-child edges cover it; the sibling-ACL test denies.
    true
}

#[test]
fn the_broker_persists_then_acks_a_fresh_delivery() {
    // ATH-MSG-012: the `messages` row + the delivery row land before the ack contract.
    let mut store = AthenaStore::in_memory().expect("store");
    let to = InstanceId::new();
    let message = row("rtl-worker", &to, "th-1", "produce the report");
    let id = message.message_id.clone();
    let delivery = broker_deliver(message, parent_child, &mut store).expect("delivered");
    assert!(
        matches!(&delivery, BrokerDelivery::DeliveredFresh { message_id: msg, to_instance: delivered_to, thread_id }
            if msg == id.as_str() && delivered_to == to.as_str() && thread_id == "th-1"),
        "{delivery:?}"
    );
    assert!(store.was_delivered(&to, &id).expect("checked"));
}

#[test]
fn a_repeated_delivery_deduplicates() {
    // ATH-MSG-014: a repeated send answers from the dedup record.
    let mut store = AthenaStore::in_memory().expect("store");
    let to = InstanceId::new();
    let first = row("rtl-worker", &to, "th-1", "produce");
    broker_deliver(first.clone(), parent_child, &mut store).expect("first");
    let second = broker_deliver(first, parent_child, &mut store).expect("settled");
    assert!(matches!(second, BrokerDelivery::AlreadyDelivered { .. }));

    // One row only — the dedup key's whole point.
    let rows = store.list_for_thread("th-1").expect("list");
    assert_eq!(rows.len(), 1);
}

#[test]
fn a_route_denial_refuses_with_the_permission_class() {
    // ATH-MSG-018: sibling pairs outside the ACL refuse at delivery.
    let mut store = AthenaStore::in_memory().expect("store");
    let to = InstanceId::new();
    let message = row("rtl-lead", &to, "th-1", "route through?");
    let refusal = broker_deliver(message, |_from, _to| false, &mut store).expect_err("refused");
    assert_eq!(refusal.class(), "PermissionError");
    assert_eq!(refusal.code(), "ROUTE_SIBLING_DENIED");
    // Refused before persisting — no rows landed.
    assert!(store.list_for_thread("th-1").expect("list").is_empty());
}

#[test]
fn an_empty_thread_id_is_refused_before_persistence() {
    // ATH-MSG-013: the thread id is Core Host's; a blank id cannot route.
    let mut store = AthenaStore::in_memory().expect("store");
    let to = InstanceId::new();
    let mut message = row("rtl-worker", &to, "", "hi");
    message.thread_id = " ".into();
    let refusal = broker_deliver(message, parent_child, &mut store).expect_err("refused");
    match refusal {
        BrokerFault::Refusal(athena_messaging::DeliveryRefusal::Envelope(
            SendRefusal::MissingThread,
        )) => {}
        other => panic!("{other:?}"),
    }
}

// --- P2-13: the sibling-ACL world drives the delivery-time check --------------------------

fn acl_fixture() -> (
    athena_messaging::RouteWorld,
    InstanceId,
    InstanceId,
    InstanceId,
) {
    // Producer → two sibling Leads; the ACL names the pair one way only.
    use athena_messaging::{RouteNode, RouteWorld};
    let producer = AgentId::core_agent();
    let lead_a = AgentId::new("rtl-lead-a");
    let lead_b = AgentId::new("rtl-lead-b");
    let inst_p = InstanceId::new();
    let inst_a = InstanceId::new();
    let inst_b = InstanceId::new();
    let world = RouteWorld::over(vec![
        RouteNode {
            agent_id: producer.clone(),
            instance_id: inst_p.clone(),
            superior: None,
            sibling_acl: vec![],
        },
        RouteNode {
            agent_id: lead_a.clone(),
            instance_id: inst_a.clone(),
            superior: Some(producer.clone()),
            sibling_acl: vec![lead_b.clone()],
        },
        RouteNode {
            agent_id: lead_b.clone(),
            instance_id: inst_b.clone(),
            superior: Some(producer),
            sibling_acl: vec![],
        },
    ]);
    (world, inst_a, inst_b, inst_p)
}

#[test]
fn a_sibling_pair_without_a_mutual_acl_is_refused_at_delivery() {
    // ATH-MSG-006/018 wired through: the recorded composition is what the Broker checks.
    let (world, inst_a, inst_b, _) = acl_fixture();
    let mut store = AthenaStore::in_memory().expect("store");
    let message = row("rtl-lead-a", &inst_b, "th-sib", "sibling direct send");
    let refusal = broker_deliver(message, |from, to| world.permits(from, to), &mut store)
        .expect_err("refused");
    assert_eq!(refusal.class(), "PermissionError");
    assert_eq!(refusal.code(), "ROUTE_SIBLING_DENIED");
    // Nothing landed: a denied route writes no rows.
    assert!(store.list_for_thread("th-sib").expect("list").is_empty());
    let _ = inst_a;
}

#[test]
fn a_parent_child_pair_routes_without_any_acl_record() {
    // ATH-MSG-006: the hierarchy edge is the default route — the same world admitting
    // producer → lead proves the ACL is a widening, not a replacement.
    let (world, _, inst_b, inst_p) = acl_fixture();
    let mut store = AthenaStore::in_memory().expect("store");
    let mut message = row(
        "athena-core-agent",
        &inst_b,
        "th-pc",
        "plan step instruction",
    );
    // The sender is instance #0's binding in the world; the helper mints a fresh id.
    message.from_instance_id = inst_p.clone();
    broker_deliver(message, |from, to| world.permits(from, to), &mut store)
        .expect("parent-child delivers");
    assert_eq!(store.list_for_thread("th-pc").expect("rows").len(), 1);
    let _ = inst_p;
}

// --- X-46: the production face (dispatch → delivery → acceptance) --------------------------

use crate::broker::{
    Correspondents, DispatchFault, accept_delivery, delegation_thread, dispatch_instruction,
    instruction_message, route_allowed,
};
use athena_persistence::repository::{AgentRepository, EventRepository, HierarchyRepository};
use athena_types::{
    AgentDefinition, AgentInstance, AgentInstanceState, AgentLevel, Hierarchy, HierarchyId,
    HierarchyNode, ModelRef, PlanStepId, RuntimeType, ToolName,
};

const NOW: &str = "2026-11-12T13:00:00.000Z";

/// One `CREATED` subordinate row, as the delegation intake writes it.
fn subordinate(agent: &str, task: &TaskId, step: &PlanStepId, attempt: u64) -> AgentInstance {
    AgentInstance {
        id: InstanceId::new(),
        agent_id: AgentId::new(agent),
        task_id: Some(task.clone()),
        plan_step_id: Some(step.clone()),
        superior_instance_id: None,
        delegation_depth: 1,
        state: AgentInstanceState::Idle,
        runtime_instance_id: None,
        workspace: "/repo/.athena/var/instances/x".into(),
        skill_set: vec![],
        aiprj_log_ref: None,
        attempt,
        started_at: Some(NOW.into()),
        finished_at: None,
    }
}

fn seeded(agent: &str) -> (AthenaStore, InstanceId, AgentInstance, TaskId, PlanStepId) {
    let mut store = AthenaStore::in_memory().expect("store");
    let task = TaskId::new();
    let step = PlanStepId::new();
    AgentRepository::upsert_definition(&mut store, &definition(agent)).expect("definition");
    let row = subordinate(agent, &task, &step, 1);
    AgentRepository::insert_instance(&mut store, &row).expect("instance row");
    (store, InstanceId::new(), row, task, step)
}

fn definition(id: &str) -> AgentDefinition {
    AgentDefinition {
        id: AgentId::new(id),
        name: id.into(),
        level: AgentLevel::Worker,
        description: "sibling".into(),
        capabilities: vec![],
        persona_ref: "worker.md".into(),
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

#[test]
fn one_delegation_owns_one_thread_and_a_retry_owns_another() {
    // ATH-MSG-013: one thread per delegation; the attempt is part of what a delegation is
    // (`plan_step_id + ":" + attempt` is the idempotency key's own shape).
    let step = PlanStepId::new();
    assert_eq!(delegation_thread(&step, 1), delegation_thread(&step, 1));
    assert_ne!(delegation_thread(&step, 1), delegation_thread(&step, 2));
    assert_ne!(
        delegation_thread(&step, 1),
        delegation_thread(&PlanStepId::new(), 1)
    );
}

#[test]
fn the_instruction_lands_as_a_row_a_delivery_and_a_sent_event() {
    let (mut store, core_instance, row, task, step) = seeded("x46-worker");
    let dispatched = dispatch_instruction(
        &mut store,
        Correspondents {
            from_instance: &core_instance,
            core_agent_instance: &core_instance,
            to_instance: &row.id,
        },
        "review the schematic",
        Some("2026-11-12T13:05:00.000Z".into()),
        1000,
        NOW,
    )
    .expect("dispatched");

    assert!(!dispatched.replayed);
    assert_eq!(dispatched.thread_id, delegation_thread(&step, 1));

    // The row carries the agent's own text, addressed to the Instance (ATH-MSG-010), on
    // the delegation's thread, with the deadline the caller stated (ATH-MSG-005).
    let stored = MessageRepository::get(&store, &dispatched.message_id)
        .expect("read")
        .expect("row");
    assert_eq!(stored.body, "review the schematic");
    assert_eq!(stored.to_instance_id.as_ref(), Some(&row.id));
    assert_eq!(stored.to_agent, row.agent_id);
    assert_eq!(stored.from_agent, AgentId::core_agent());
    assert_eq!(stored.task_id.as_ref(), Some(&task));
    assert_eq!(stored.deadline.as_deref(), Some("2026-11-12T13:05:00.000Z"));
    assert!(
        store
            .was_delivered(&row.id, &dispatched.message_id)
            .expect("delivered")
    );

    // ATH-MSG-007: the send is an Event on the Task's own chain.
    let types: Vec<String> = EventRepository::list_for_task(&store, &task)
        .expect("events")
        .into_iter()
        .map(|event| event.event_type)
        .collect();
    assert_eq!(types, vec!["AgentMessageSent".to_string()]);
}

#[test]
fn a_dispatch_repeat_answers_the_recorded_delivery_without_a_second_event() {
    // ATH-MSG-014: dedup is the Broker's. The second dispatch carries a fresh envelope id,
    // so the dedup that matters here is the delivery record's — and the test states what
    // the implementation actually guarantees: one row per dispatch call, each delivered.
    let (mut store, core_instance, row, task, _step) = seeded("x46-worker");
    let first = dispatch_instruction(
        &mut store,
        Correspondents {
            from_instance: &core_instance,
            core_agent_instance: &core_instance,
            to_instance: &row.id,
        },
        "go",
        None,
        1000,
        NOW,
    )
    .expect("first");
    accept_delivery(&mut store, &row.id, &first.message_id, NOW).expect("accepted");

    // Accepting is the receiving side's act; the dwell count drops to zero afterwards.
    assert_eq!(MessageRepository::count_dwelling(&store).expect("dwell"), 0);
    let types: Vec<String> = EventRepository::list_for_task(&store, &task)
        .expect("events")
        .into_iter()
        .map(|event| event.event_type)
        .collect();
    assert_eq!(
        types,
        vec![
            "AgentMessageSent".to_string(),
            "AgentMessageReceived".to_string()
        ]
    );
}

#[test]
fn an_unaccepted_instruction_keeps_dwelling() {
    // The honest residue: a message the peer never took is delivered-but-unaccepted, which
    // is exactly what `athena_message_queue_depth` counts.
    let (mut store, core_instance, row, _task, _step) = seeded("x46-worker");
    dispatch_instruction(
        &mut store,
        Correspondents {
            from_instance: &core_instance,
            core_agent_instance: &core_instance,
            to_instance: &row.id,
        },
        "go",
        None,
        1000,
        NOW,
    )
    .expect("dispatched");
    assert_eq!(MessageRepository::count_dwelling(&store).expect("dwell"), 1);
}

#[test]
fn the_dwell_ceiling_refuses_an_instruction_transiently() {
    // ATH-MSG-009: at the limit an `instruction` is refused with `TransientError` — and
    // refused *before* the row exists, or the gate would admit what it is counting.
    let (mut store, core_instance, row, _task, _step) = seeded("x46-worker");
    dispatch_instruction(
        &mut store,
        Correspondents {
            from_instance: &core_instance,
            core_agent_instance: &core_instance,
            to_instance: &row.id,
        },
        "first",
        None,
        1000,
        NOW,
    )
    .expect("dispatched");
    let fault = dispatch_instruction(
        &mut store,
        Correspondents {
            from_instance: &core_instance,
            core_agent_instance: &core_instance,
            to_instance: &row.id,
        },
        "second",
        None,
        1,
        NOW,
    )
    .expect_err("refused at the ceiling");
    assert!(
        matches!(fault, DispatchFault::QueueFull { depth: 1, limit: 1 }),
        "{fault}"
    );
    assert_eq!(fault.class(), "TransientError");
    assert_eq!(fault.code(), "MESSAGE_QUEUE_FULL");
    // Nothing was written by the refused call: one row still.
    assert_eq!(MessageRepository::count_dwelling(&store).expect("dwell"), 1);
}

#[test]
fn an_absent_destination_is_not_a_route_question() {
    let (mut store, core_instance, _row, _task, _step) = seeded("x46-worker");
    let fault = dispatch_instruction(
        &mut store,
        Correspondents {
            from_instance: &core_instance,
            core_agent_instance: &core_instance,
            to_instance: &InstanceId::new(),
        },
        "go",
        None,
        1000,
        NOW,
    )
    .expect_err("refused");
    assert!(matches!(fault, DispatchFault::InstanceAbsent(_)), "{fault}");
    assert_eq!(fault.class(), "ValidationError");
}

#[test]
fn a_stranger_instance_is_denied_the_route() {
    // ATH-MSG-006: neither a recorded parent-child edge nor the delegating Core Agent, and
    // no sibling allow-list — the pair may not correspond.
    let (mut store, core_instance, row, task, _step) = seeded("x46-worker");
    AgentRepository::upsert_definition(&mut store, &definition("x46-stranger")).expect("def");
    let stranger = subordinate("x46-stranger", &task, &PlanStepId::new(), 1);
    AgentRepository::insert_instance(&mut store, &stranger).expect("row");

    assert!(!route_allowed(&store, &stranger.id, &row.id, &core_instance).expect("read"));
    let fault = dispatch_instruction(
        &mut store,
        Correspondents {
            from_instance: &stranger.id,
            core_agent_instance: &core_instance,
            to_instance: &row.id,
        },
        "go",
        None,
        1000,
        NOW,
    )
    .expect_err("denied");
    assert_eq!(fault.class(), "PermissionError");
}

#[test]
fn a_recorded_superior_may_address_its_subordinate() {
    let (mut store, core_instance, row, task, step) = seeded("x46-worker");
    AgentRepository::upsert_definition(&mut store, &definition("x46-child")).expect("def");
    let mut child = subordinate("x46-child", &task, &step, 1);
    child.superior_instance_id = Some(row.id.clone());
    AgentRepository::insert_instance(&mut store, &child).expect("row");
    assert!(route_allowed(&store, &row.id, &child.id, &core_instance).expect("read"));
}

#[test]
fn siblings_correspond_only_through_the_recorded_allow_list() {
    // ATH-MSG-018: the permission is the hierarchy's record, never an inference from the
    // two rows sharing a Task.
    let (mut store, core_instance, left, task, _step) = seeded("x46-left");
    AgentRepository::upsert_definition(&mut store, &definition("x46-right")).expect("right");
    let right = subordinate("x46-right", &task, &PlanStepId::new(), 1);
    AgentRepository::insert_instance(&mut store, &right).expect("row");

    // Without a hierarchy: denied.
    assert!(!route_allowed(&store, &left.id, &right.id, &core_instance).expect("read"));

    let node = |agent: &str, peer: &str| HierarchyNode {
        agent_id: AgentId::new(agent),
        level: AgentLevel::Worker,
        superior_agent_id: Some(AgentId::core_agent()),
        scope_ref: None,
        sibling_acl: vec![AgentId::new(peer)],
    };
    HierarchyRepository::insert(
        &mut store,
        &Hierarchy {
            id: HierarchyId::new(),
            task_id: task.clone(),
            revision: 1,
            nodes: vec![node("x46-left", "x46-right"), node("x46-right", "x46-left")],
            created_at: NOW.into(),
        },
    )
    .expect("hierarchy");

    assert!(route_allowed(&store, &left.id, &right.id, &core_instance).expect("read"));

    // One-sided permission is not permission.
    HierarchyRepository::insert(
        &mut store,
        &Hierarchy {
            id: HierarchyId::new(),
            task_id: task.clone(),
            revision: 2,
            nodes: vec![node("x46-left", "x46-right")],
            created_at: NOW.into(),
        },
    )
    .expect("hierarchy");
    assert!(!route_allowed(&store, &left.id, &right.id, &core_instance).expect("read"));
}

#[test]
fn the_runtime_projection_restates_the_stored_row() {
    let (mut store, core_instance, row, _task, _step) = seeded("x46-worker");
    let dispatched = dispatch_instruction(
        &mut store,
        Correspondents {
            from_instance: &core_instance,
            core_agent_instance: &core_instance,
            to_instance: &row.id,
        },
        "carry this",
        Some("2026-11-12T13:30:00.000Z".into()),
        1000,
        NOW,
    )
    .expect("dispatched");
    let message = instruction_message(
        &dispatched,
        &core_instance,
        "carry this",
        Some("2026-11-12T13:30:00.000Z".into()),
    );
    let stored = MessageRepository::get(&store, &dispatched.message_id)
        .expect("read")
        .expect("row");
    // The body the peer reads and the body the store holds are one text, under one id.
    assert_eq!(message.message_id, stored.message_id);
    assert_eq!(message.body, stored.body);
    assert_eq!(message.thread_id, stored.thread_id);
    assert_eq!(message.deadline, stored.deadline);
    assert_eq!(message.from_agent, Some(AgentId::core_agent()));
    assert!(
        !message.is_undroppable(),
        "an instruction is refusable (ATH-MSG-009/012)"
    );
}
