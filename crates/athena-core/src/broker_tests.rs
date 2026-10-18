//! The Broker's store-facing driver (P2-11/12; ATH-MSG-012/014).

use crate::broker::{BrokerDelivery, BrokerFault, broker_deliver};
use athena_messaging::envelope::SendRefusal;
use athena_persistence::SqliteStore;
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
    let mut store = SqliteStore::in_memory().expect("store");
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
    let mut store = SqliteStore::in_memory().expect("store");
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
    let mut store = SqliteStore::in_memory().expect("store");
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
    let mut store = SqliteStore::in_memory().expect("store");
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
    let mut store = SqliteStore::in_memory().expect("store");
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
    let mut store = SqliteStore::in_memory().expect("store");
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
