//! P2-13: hierarchy routing with the sibling-ACL check at delivery
//! (ATH-MSG-006, ATH-MSG-018).

use super::{RouteNode, RouteWorld, route_denial};
use athena_types::{AgentId, InstanceId};

/// Producer → two Leads, one of the Leads ACL'd to the other, each with an instance.
fn two_lead_world() -> (RouteWorld, InstanceId, InstanceId) {
    let producer = AgentId::core_agent();
    let lead_a = AgentId::new("rtl-lead-a");
    let lead_b = AgentId::new("rtl-lead-b");
    let inst_a = InstanceId::new();
    let inst_b = InstanceId::new();
    let world = RouteWorld::over(vec![
        RouteNode {
            agent_id: producer.clone(),
            instance_id: InstanceId::new(),
            superior: None,
            sibling_acl: vec![],
        },
        RouteNode {
            agent_id: lead_a.clone(),
            instance_id: inst_a.clone(),
            superior: Some(producer.clone()),
            // The grant runs one way only: a → b is listed, b → a is not.
            sibling_acl: vec![lead_b.clone()],
        },
        RouteNode {
            agent_id: lead_b,
            instance_id: inst_b.clone(),
            superior: Some(producer),
            // Not listing lead-a: its own channel was never granted.
            sibling_acl: vec![],
        },
    ]);
    (world, inst_a, inst_b)
}

#[test]
fn a_parent_child_edge_permits_without_any_acl() {
    // ATH-MSG-006: communication follows the hierarchy; the parent-child edge is the
    // default route and needs no recorded grant.
    let (world, _, _) = two_lead_world();

    let producer_instance = world
        .nodes()
        .iter()
        .find(|node| node.agent_id.is_core_agent())
        .map(|node| node.instance_id.clone())
        .expect("producer node");

    let lead = world.nodes()[1].instance_id.clone();
    assert!(
        world.permits(&producer_instance, &lead),
        "producer → lead is the parent-child edge"
    );
    assert!(
        world.permits(&lead, &producer_instance),
        "lead → producer reports upward: the same edge, both directions"
    );
}

#[test]
fn sibling_delivery_requires_the_acl_on_both_sides() {
    // ATH-MSG-018: the ACL is recorded at compose_hierarchy and checked at delivery.
    // The one-way grant refuses: b never listed a.
    let (world, inst_a, inst_b) = two_lead_world();
    assert!(
        !world.permits(&inst_a, &inst_b),
        "a one-side grant is not a granted channel"
    );
    assert!(!world.permits(&inst_b, &inst_a));
}

#[test]
fn a_mutual_acl_grant_permits_the_sibling_pair() {
    // The mutual grant passes: when both nodes record each other the channel exists.
    let producer = AgentId::core_agent();
    let lead_a = AgentId::new("rtl-lead-a");
    let lead_b = AgentId::new("rtl-lead-b");
    let inst_a = InstanceId::new();
    let inst_b = InstanceId::new();
    let world = RouteWorld::over(vec![
        RouteNode {
            agent_id: lead_a.clone(),
            instance_id: inst_a.clone(),
            superior: Some(producer.clone()),
            sibling_acl: vec![lead_b.clone()],
        },
        RouteNode {
            agent_id: lead_b,
            instance_id: inst_b.clone(),
            superior: Some(producer),
            sibling_acl: vec![lead_a],
        },
    ]);
    assert!(world.permits(&inst_a, &inst_b));
    assert!(world.permits(&inst_b, &inst_a));
}

#[test]
fn an_unknown_endpoint_is_refused_by_routing() {
    // Both endpoints must be known to the composition; routing never guesses.
    let (world, inst_a, _) = two_lead_world();
    let stranger = InstanceId::new();
    assert!(!world.permits(&stranger, &inst_a));
    assert!(!world.permits(&inst_a, &stranger));
}

#[test]
fn a_cross_branch_pair_is_neither_edge_nor_acl() {
    // Two siblings under *different* superiors are a cross-branch pair: even a mutual ACL
    // listing must not bridge levels of the tree routing never drew.
    let producer = AgentId::core_agent();
    let lead = AgentId::new("rtl-lead");
    let worker_a = AgentId::new("rtl-worker-a");
    let worker_b = AgentId::new("rtl-worker-b");
    let inst_a = InstanceId::new();
    let inst_b = InstanceId::new();
    let inst_p = InstanceId::new();
    let inst_l = InstanceId::new();
    let world = RouteWorld::over(vec![
        RouteNode {
            agent_id: producer.clone(),
            instance_id: inst_p.clone(),
            superior: None,
            sibling_acl: vec![],
        },
        RouteNode {
            agent_id: lead.clone(),
            instance_id: inst_l.clone(),
            superior: Some(producer.clone()),
            sibling_acl: vec![],
        },
        RouteNode {
            agent_id: worker_a.clone(),
            instance_id: inst_a.clone(),
            superior: Some(producer.clone()),
            sibling_acl: vec![worker_b.clone()],
        },
        RouteNode {
            agent_id: worker_b,
            instance_id: inst_b.clone(),
            superior: Some(lead),
            sibling_acl: vec![worker_a],
        },
    ]);
    // Different superiors (Producer vs Lead): the shape check refuses what the ACL lists,
    // while every *real* edge of the same tree still routes.
    assert!(
        world.permits(&inst_p, &inst_l),
        "the parent-child edge that does exist keeps routing"
    );
    assert!(
        !world.permits(&inst_a, &inst_b),
        "cross-branch is refused although both ACLs record each other"
    );
    assert!(!world.permits(&inst_b, &inst_a));
}

fn producer_instance_of(world: &RouteWorld) -> InstanceId {
    world
        .nodes()
        .iter()
        .find(|node| node.agent_id.is_core_agent())
        .map(|node| node.instance_id.clone())
        .expect("the producer node of the fixture")
}

#[test]
fn a_route_refusal_carries_the_denial_fact() {
    // ATH-MSG-018 at delivery: the denial the caller records.
    let (world, _, inst_b) = two_lead_world();
    let stranger = InstanceId::new();
    let denial = route_denial(&stranger, &inst_b, &world, None, None).expect("denial");
    assert_eq!(denial.code, "ROUTE_SIBLING_DENIED");
    assert_eq!(denial.class, "PermissionError");
    // The unknown sender has no binding; the fact stays `None` (the caller's envelope
    // carries the agent).
    assert_eq!(denial.from_agent, None);
}

#[test]
fn a_known_pair_denial_names_the_sender_agent() {
    // The fixture's a→b one-way grant: a is bound, so the refusal names it.
    let (world, inst_a, inst_b) = two_lead_world();
    let denial = route_denial(&inst_a, &inst_b, &world, None, None).expect("denial");
    assert_eq!(
        denial.from_agent.as_ref().map(AgentId::as_str),
        Some("rtl-lead-a")
    );
    assert!(
        denial.details.contains("rtl-lead-a") && denial.details.contains("rtl-lead-b"),
        "the detail names the pair: {}",
        denial.details
    );
}

#[test]
fn a_permitted_pair_yields_no_denial() {
    let (world, _, _) = two_lead_world();
    let producer_instance = producer_instance_of(&world);
    let lead = world.nodes()[1].instance_id.clone();
    assert_eq!(
        route_denial(&producer_instance, &lead, &world, None, None),
        None,
        "the parent-child route denies nothing"
    );
}
