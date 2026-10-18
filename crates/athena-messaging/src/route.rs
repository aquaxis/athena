//! The sibling-ACL route model (P2-13; ATH-MSG-006, ATH-MSG-018).
//!
//! The spec records the sibling permission as a **list of agents** on each hierarchy node
//! (§26.6 `HierarchyNode.sibling_acl`), while the Broker's route decision is **per
//! instance**. The two vocabularies meet here: [`RouteNode`] is a settled node as the
//! Runtime sees it — its agent id and the instance that realizes it — and
//! [`RouteWorld`] is the pure permission model Core Host evaluates with at delivery.
//!
//! The hierarchy is logical (ATH-HIER-010): it exists as message routing plus permission
//! containment, never as process parentage. This module is the routing half, kept free of
//! SQL and of Runtime specifics so the crate's dependency list stays `athena-types` alone
//! (§32.2's allow-list for this surface).
//!
//! Core Host never decides *who* is granted — the ACL is Core Agent's composition record
//! (ATH-POLICY-010, `ATH-CORE-022`). Core Host only *checks* the recorded grants at
//! delivery (ATH-MSG-018): parent-child edges always pass, sibling pairs only by ACL, and
//! both endpoints must be known to the composition at all.

use athena_types::{AgentId, InstanceId};
use std::collections::BTreeMap;

/// One settled node as the route model sees it.
///
/// Composition (`athena-agent::hierarchy::compose`) settles the tree; the caller binds
/// each node to the instance that realizes it (`AgentInstance : Runtime Instance = 1 : 1`,
/// ATH-RT-012), because routing addresses instances (ATH-MSG-010).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteNode {
    /// The agent at this node.
    pub agent_id: AgentId,
    /// The instance realizing the agent at start time.
    pub instance_id: InstanceId,
    /// The superior agent, `None` for the composition root.
    pub superior: Option<AgentId>,
    /// The sibling peers this node may address directly (ATH-MSG-018).
    pub sibling_acl: Vec<AgentId>,
}

/// The pure route permission model (ATH-MSG-006, ATH-MSG-018).
///
/// Built from the composition's settled nodes plus their instance bindings; answers the
/// delivery-time question "is this `(from, to)` pair legal?".
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RouteWorld {
    nodes: Vec<RouteNode>,
}

impl RouteWorld {
    /// A world over the composition's settled nodes, each bound to its live instance.
    #[must_use]
    pub fn over(nodes: Vec<RouteNode>) -> Self {
        Self { nodes }
    }

    /// The node of the instance that sends, `None` when the instance is unknown to the
    /// composition (unaddressable either way; ATH-MSG-018 requires both endpoints known).
    #[must_use]
    fn sender(&self, from: &InstanceId) -> Option<&RouteNode> {
        self.nodes.iter().find(|node| &node.instance_id == from)
    }

    /// The node realizing `agent`, if the composition holds one.
    #[must_use]
    fn node_of(&self, agent: &AgentId) -> Option<&RouteNode> {
        self.nodes.iter().find(|node| &node.agent_id == agent)
    }

    /// Whether delivery from `from` to `to` is permitted.
    ///
    /// - Parent-child edges always pass: communication follows the hierarchy (ATH-MSG-006).
    /// - Same-level pairs pass only when the sender's ACL records the peer, **mutually**
    ///   (ATH-MSG-018: both sides' recorded allows must include each other — a grant one
    ///   side never made is not a granted channel).
    /// - An instance or target agent unknown to the composition is refused: routing never
    ///   guesses (the tree is the recorded composition, nothing more).
    #[must_use]
    pub fn permits(&self, from: &InstanceId, to_instance: &InstanceId) -> bool {
        let Some(sender) = self.sender(from) else {
            return false;
        };
        let Some(receiver) = self
            .nodes
            .iter()
            .find(|node| &node.instance_id == to_instance)
        else {
            return false;
        };

        // The parent-child edge, by agent ids.
        if sender.superior.as_ref() == Some(&receiver.agent_id)
            || receiver.superior.as_ref() == Some(&sender.agent_id)
        {
            return true;
        }

        // Sibling edge: both agents are registered, are at the same level under the same
        // superior, and each names the other in its own ACL. The level-and-superior shape
        // is what makes "sibling" precise; a cross-branch pair is not an ACL matter but a
        // hierarchy one and refuses here exactly as it would in the hierarchy checks.
        sender
            .sibling_acl
            .iter()
            .any(|peer| peer == &receiver.agent_id)
            && receiver
                .sibling_acl
                .iter()
                .any(|peer| peer == &sender.agent_id)
            && sender.superior == receiver.superior
    }

    /// The agent of the instance that sends, when the composition knows it.
    #[must_use]
    pub fn sender_agent(&self, from: &InstanceId) -> Option<&AgentId> {
        self.sender(from).map(|node| &node.agent_id)
    }

    /// Whether the addressed target agent is known to the composition and holds an
    /// instance binding — the pre-check the caller applies before the route decision so an
    /// unknown agent refuses as a routing fact rather than fabricating a delivery.
    #[must_use]
    pub fn knows_target(&self, to_agent: &AgentId) -> bool {
        self.node_of(to_agent).is_some()
    }

    /// The instance realizing `to_agent`, when the agent is a registered, bound node —
    /// resolving an agent-only address toward the single-instance exception (ATH-MSG-010).
    #[must_use]
    pub fn instance_of(&self, to_agent: &AgentId) -> Option<&InstanceId> {
        self.node_of(to_agent).map(|node| &node.instance_id)
    }

    /// The nodes, in registration order, for the caller's reconstruction of the wait graph.
    #[must_use]
    pub fn nodes(&self) -> &[RouteNode] {
        &self.nodes
    }
}

/// Index the composition's nodes by agent id — a caller convenience when binding instances
/// to agents read from the store, where duplicate agents are already refused at
/// composition (ATH-HIER-002).
#[must_use]
pub fn by_agent(nodes: &[RouteNode]) -> BTreeMap<AgentId, &RouteNode> {
    nodes
        .iter()
        .map(|node| (node.agent_id.clone(), node))
        .collect()
}

/// A delivery-time route refusal, with the Event/envelope material the caller records
/// (`PrivilegedRequestDenied` on the §20.1 `PermissionError` class; the wire code is the
/// Broker's stable `ROUTE_SIBLING_DENIED`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteRefusal {
    /// The stable wire code.
    pub code: &'static str,
    /// The §20.1 class text.
    pub class: &'static str,
    /// The sending agent, from the caller's envelope or the world's binding.
    pub from_agent: Option<AgentId>,
    /// The addressed agent, known the same way.
    pub to_agent: Option<AgentId>,
    /// The human-readable pair detail.
    pub details: String,
}

/// The delivery-time denial for a `(from, to)` pair, `None` when the route is permitted.
///
/// The agents derive from the world's bindings; the `from_agent` / `to_agent` overrides
/// carry what the caller's envelope already knows (a stale or unbound instance id must
/// still name its agent in the recorded denial — the fact lives on the Message row).
#[must_use]
pub fn route_denial(
    from_instance: &InstanceId,
    to_instance: &InstanceId,
    world: &RouteWorld,
    from_agent: Option<&AgentId>,
    to_agent: Option<&AgentId>,
) -> Option<RouteRefusal> {
    if world.permits(from_instance, to_instance) {
        return None;
    }
    let from = from_agent
        .cloned()
        .or_else(|| world.sender_agent(from_instance).cloned());
    let to = to_agent.cloned().or_else(|| {
        world
            .nodes()
            .iter()
            .find(|node| &node.instance_id == to_instance)
            .map(|node| node.agent_id.clone())
    });
    let pair = match (&from, &to) {
        (Some(f), Some(t)) => format!("{f} → {t}"),
        (Some(f), None) => format!("{f} → an unbound instance"),
        (None, Some(t)) => format!("an unbound instance → {t}"),
        (None, None) => "an unbound instance → an unbound instance".into(),
    };
    Some(RouteRefusal {
        code: "ROUTE_SIBLING_DENIED",
        class: "PermissionError",
        from_agent: from,
        to_agent: to,
        details: format!(
            "the route {pair} is neither a parent-child edge nor a mutually recorded sibling \
             ACL pair (ATH-MSG-006, ATH-MSG-018)"
        ),
    })
}
