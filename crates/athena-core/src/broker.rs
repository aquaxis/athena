//! The Broker driver (P2-11/12; ATH-MSG-012/014) — the store-side half that
//! `athena-messaging` (whose dependency list stays `athena-types` only) cannot reach:
//! validate shape (`athena-messaging`'s pure checks), insert the `messages` row, mark the
//! delivery (dedup on `(to_instance_id, message_id)`), and hand back the fact both sides
//! record Events from.

use athena_messaging::{Delivery, DeliveryRefusal};
/// The Broker owned-entity slice: the message rows route through here,
/// permission-checked by athena-messaging and persisted by the repository.
pub const OWNED: &[(crate::ownership::Entity, crate::ownership::ModuleRole)] = &[(
    crate::ownership::Entity::Message,
    crate::ownership::ModuleRole::Broker,
)];

// Re-exports the tests and the Core Host caller cite.
pub use athena_messaging::{Delivery as BrokerDelivery, DeliveryRefusal as BrokerRefusal};
use athena_persistence::error::PersistenceError;
use athena_persistence::repository::{MessageRepository, MessageRow};
use athena_types::InstanceId;

/// Why a brokered send failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BrokerFault {
    /// A route/shape refusal (mapped through `athena-messaging`'s outcome).
    #[error(transparent)]
    Refusal(#[from] DeliveryRefusal),
    /// The store broke; `InternalError`-class for the caller (never retried,
    /// ATH-FAIL-002).
    #[error("broker store failed: {0}")]
    Store(String),
}

/// Deliver one message row: pure route check + persist + dedup, in that order — the
/// insert-before-delivery order is what the `message_deliveries.message_id` FK demands.
///
/// `world` carries the legal `(from instance, to instance)` pairs — parent-child edges
/// always, sibling pairs only via the recorded ACL (ATH-MSG-018).
///
/// # Errors
/// [`BrokerFault::Refusal`] with the mapped class/code pair, or [`BrokerFault::Store`].
pub fn broker_deliver<World>(
    message: MessageRow,
    world: World,
    store: &mut dyn MessageRepository,
) -> Result<Delivery, BrokerFault>
where
    World: Fn(&InstanceId, &InstanceId) -> bool,
{
    let underway = athena_messaging::envelope::MessageUnderway {
        message_id: message.message_id.clone(),
        from: message.from_agent.clone(),
        from_instance: message.from_instance_id.clone(),
        to: message.to_agent.clone(),
        to_instance: message.to_instance_id.clone(),
        thread_id: message.thread_id.clone(),
        priority: message.priority,
        body: message.body.clone(),
        deadline: message.deadline.clone(),
    };
    athena_messaging::route_check(&underway, world)?;

    let to_instance = message
        .to_instance_id
        .clone()
        .ok_or(DeliveryRefusal::Envelope(
            athena_messaging::SendRefusal::MissingInstance,
        ))?;

    // Dedup first (ATH-MSG-014): a repeated send answers the recorded delivery.
    if store
        .was_delivered(&to_instance, &message.message_id)
        .map_err(fault)?
    {
        return Ok(Delivery::AlreadyDelivered {
            message_id: message.message_id.as_str().to_string(),
            to_instance: to_instance.as_str().to_string(),
        });
    }

    // Fresh: the `messages` row lands first (the FK target), then the delivery record —
    // and the ack contract holds because both are persisted when this returns
    // (ATH-MSG-012).
    store.insert(&message).map_err(fault)?;
    store
        .mark_delivered(&to_instance, &message.message_id)
        .map_err(fault)?;

    Ok(Delivery::DeliveredFresh {
        message_id: message.message_id.as_str().to_string(),
        to_instance: to_instance.as_str().to_string(),
        thread_id: message.thread_id.clone(),
    })
}

impl BrokerFault {
    /// The 20.1 class of the refusal when it is a refusal.
    #[must_use]
    pub const fn class(&self) -> &'static str {
        match self {
            Self::Refusal(one) => one.class(),
            Self::Store(_) => "InternalError",
        }
    }

    /// A stable code when it is a refusal.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Refusal(one) => one.code(),
            Self::Store(_) => "STORE_FAULT",
        }
    }
}

fn fault(error: PersistenceError) -> BrokerFault {
    BrokerFault::Store(error.to_string())
}

// --- P2-20: the aggregation observation at the delivery point ------------------------------
//
// A `report` is never dropped or refused (ATH-MSG-012); what the Broker adds is the
// *observation* the evaluator (Lead/Director/Producer) acts on: a verbatim pass-through
// of a subordinate's full output, derived from facts on the store (ATH-HIER-015). The
// consequence — re-requesting a real aggregation, rejecting the evaluation, re-planning —
// is the deciding agent's (ATH-CORE-022), never Core Host's.

use crate::ownership::{Entity, ModuleRole};

/// The `report`-flow entities this module may mutate (the §26.2 slice, restated here).
pub const MESSAGE_OWNED: &[(Entity, ModuleRole)] = OWNED;

/// The aggregation observation when the delivered `report` rides subordinate outputs
/// verbatim: `Some` relay facts, in sorted order, when the structural floor matched.
///
/// The caller (Core Host's orchestration) records the observation as an Event via the
/// Event Manager (D-1) and hands it to the evaluator's own loop. Nothing is refused:
/// `report` stays a never-dropped kind (ATH-MSG-012).
#[must_use]
pub fn report_relay_observations(
    body: &str,
    subordinate_outputs: &[(String, String)],
) -> Vec<athena_messaging::VerbatimRelay> {
    athena_messaging::verbatim_relays(body, subordinate_outputs)
}

// --- X-46: the Broker's production face ----------------------------------------------------
//
// Everything above is the driver the tests have exercised since P2-11; what was missing is
// the path production actually takes. The Core Agent delegates a PlanStep, the Runtime
// Manager starts the Instance, and the delegation *instruction* — the agent's own procedure
// reference, which `CoreRequest::Delegate` carries and the admission decision deliberately
// does not consult — has to reach that Instance. It reaches it here: as a `Message` row
// through the Broker (ATH-MSG-001 — never a direct path outside Core's management), on the
// delegation's own thread (ATH-MSG-013), under a reply deadline (ATH-MSG-005/016), with the
// route checked against the hierarchy (ATH-MSG-006/018) and the dwell limit honoured
// (ATH-MSG-009).
//
// Core Host is the delivery substrate, never a correspondent (ATH-MSG-011): the sender is
// the **Core Agent** (`athena-core-agent`, instance #0) and the recipient is the subordinate
// Instance — the two Agents the hierarchy relates.

use athena_persistence::repository::{AgentRepository, EventRepository, HierarchyRepository};
use athena_persistence::{AthenaStore, NewEvent};
use athena_types::{AgentId, EventId, MessageId, MessageType, Priority, TaskId};

/// Why a dispatch refused before or during delivery.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DispatchFault {
    /// `ATH-MSG-009`: the Broker's dwell limit (`messaging.max_queue_depth`) is reached.
    /// Only `instruction` / `status` / `question` may be refused this way — the
    /// never-dropped kinds (`report` / `complete` / `error` and the control kinds) are
    /// not routed through this gate at all (ATH-MSG-012).
    #[error(
        "the Broker holds {depth} dwelling message(s), at or over the limit {limit} (ATH-MSG-009)"
    )]
    QueueFull {
        /// The measured dwell count.
        depth: u64,
        /// The configured ceiling.
        limit: u64,
    },
    /// The addressed Instance has no row: there is nobody to deliver to.
    #[error("instance {0} is absent")]
    InstanceAbsent(String),
    /// The route is not one the hierarchy permits (`ATH-MSG-006` / `ATH-MSG-018`), or the
    /// envelope itself refused — the Broker's own answer, carried whole.
    #[error(transparent)]
    Broker(#[from] BrokerFault),
    /// The store refused a read or a write.
    #[error("store fault: {0}")]
    Store(String),
    /// An Event construction refused against the catalog.
    #[error("event construction: {0}")]
    Construction(String),
}

impl DispatchFault {
    /// The §20.1 class of this refusal.
    #[must_use]
    pub fn class(&self) -> &'static str {
        match self {
            // The queue drains as the receiving sides accept: a retry is the right answer
            // (ATH-MSG-009's own `TransientError`).
            Self::QueueFull { .. } => "TransientError",
            Self::InstanceAbsent(_) => "ValidationError",
            Self::Broker(fault) => fault.class(),
            Self::Store(_) | Self::Construction(_) => "InternalError",
        }
    }

    /// The stable machine code within the class.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::QueueFull { .. } => "MESSAGE_QUEUE_FULL",
            Self::InstanceAbsent(_) => "INSTANCE_NOT_FOUND",
            Self::Broker(fault) => fault.code(),
            Self::Store(_) | Self::Construction(_) => "MESSAGE_DISPATCH_FAILED",
        }
    }
}

/// What one dispatch recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dispatched {
    /// The message row's identifier.
    pub message_id: MessageId,
    /// The delegation's thread (`ATH-MSG-013`).
    pub thread_id: String,
    /// Whether the row was written now or the recorded delivery answered a repeat
    /// (`ATH-MSG-014`: dedup is the Broker's, never the agent's memory).
    pub replayed: bool,
}

/// The thread one delegation owns (`ATH-MSG-013`: one thread per delegation, and every
/// round trip underneath rides it). Derived from the step and the attempt, so a retry of
/// the same step is a *new* delegation with its own thread while a replay of the same
/// attempt re-derives the same one.
#[must_use]
pub fn delegation_thread(plan_step_id: &athena_types::PlanStepId, attempt: u64) -> String {
    format!("thr-{}-{attempt}", plan_step_id.as_str())
}

/// Whether the hierarchy permits this pair to correspond (`ATH-MSG-006`, `ATH-MSG-018`).
///
/// Three grounds, in the order the spec states them, and nothing else:
/// 1. the recorded parent-child edge — the addressed row names `from` as its superior;
/// 2. the delegating Core Agent — in v1 every delegated Instance is the Core Agent's own
///    subordinate (`ATH-CORE-022`: it is the single decision-maker that delegates), and
///    instance #0 keeps no Instance row of its own, so the caller states the identity it
///    holds rather than the store inventing one;
/// 3. the sibling allow-list the Core Agent recorded through `compose_hierarchy`
///    (`ATH-MSG-018`) — read from the addressed row's Task hierarchy, by Agent id.
///
/// Anything else is denied. An absent destination row is *not* a route question — the
/// caller gets [`DispatchFault::InstanceAbsent`] instead.
///
/// # Errors
/// [`DispatchFault::Store`] when a read faulted; [`DispatchFault::InstanceAbsent`] when the
/// addressed Instance has no row.
pub fn route_allowed(
    store: &AthenaStore,
    from: &InstanceId,
    to: &InstanceId,
    core_agent_instance: &InstanceId,
) -> Result<bool, DispatchFault> {
    let destination = AgentRepository::get_instance(store, to)
        .map_err(dispatch_store_fault)?
        .ok_or_else(|| DispatchFault::InstanceAbsent(to.as_str().to_string()))?;
    if destination.superior_instance_id.as_ref() == Some(from) {
        return Ok(true);
    }
    if from == core_agent_instance {
        return Ok(true);
    }
    // Siblings: both rows' Agents must appear in each other's recorded allow-list on the
    // Task's own hierarchy. An unrecorded pair is denied — the permission is a record, not
    // an inference (ATH-MSG-018).
    let Some(origin) = AgentRepository::get_instance(store, from).map_err(dispatch_store_fault)?
    else {
        return Ok(false);
    };
    let Some(task) = destination.task_id.as_ref() else {
        return Ok(false);
    };
    if origin.task_id.as_ref() != Some(task) {
        return Ok(false);
    }
    let Some(hierarchy) =
        HierarchyRepository::latest_for_task(store, task).map_err(dispatch_store_fault)?
    else {
        return Ok(false);
    };
    let permits = |holder: &AgentId, peer: &AgentId| {
        hierarchy
            .nodes
            .iter()
            .any(|node| &node.agent_id == holder && node.sibling_acl.contains(peer))
    };
    Ok(permits(&origin.agent_id, &destination.agent_id)
        && permits(&destination.agent_id, &origin.agent_id))
}

/// The three Instances one dispatch names: who sends, who the hierarchy's own delegating
/// agent is (instance #0, which keeps no Instance row of its own — the caller states the
/// identity it holds), and who receives.
#[derive(Debug, Clone, Copy)]
pub struct Correspondents<'a> {
    /// The sending Instance (`ATH-MSG-011`: never Core Host itself).
    pub from_instance: &'a InstanceId,
    /// The Core Agent's resident Instance — the superior of every delegated row in v1.
    pub core_agent_instance: &'a InstanceId,
    /// The addressed Instance (`ATH-MSG-010`).
    pub to_instance: &'a InstanceId,
}

/// Dispatch the delegation instruction to a started Instance (`X-46`).
///
/// The order is the one the contracts demand: the dwell gate first (`ATH-MSG-009` — an
/// `instruction` is refusable, and refusing before the row exists is what keeps the gate
/// meaningful), then the route check and the persistence inside [`broker_deliver`]
/// (insert → delivery record, so the ack contract of `ATH-MSG-012` holds when this
/// returns), then the `AgentMessageSent` Event on the Task's own correlation chain
/// (`ATH-MSG-007`).
///
/// The reply deadline is the caller's (`timeout.message_reply`, `ATH-MSG-005`/`016`): the
/// configuration owns the clock, this module owns the rule.
///
/// # Errors
/// [`DispatchFault`], whole.
pub fn dispatch_instruction(
    store: &mut AthenaStore,
    who: Correspondents<'_>,
    instruction: &str,
    deadline: Option<String>,
    max_queue_depth: u64,
    now: &str,
) -> Result<Dispatched, DispatchFault> {
    let Correspondents {
        from_instance,
        core_agent_instance,
        to_instance,
    } = who;
    let destination = AgentRepository::get_instance(store, to_instance)
        .map_err(dispatch_store_fault)?
        .ok_or_else(|| DispatchFault::InstanceAbsent(to_instance.as_str().to_string()))?;

    // ATH-MSG-009: the ceiling is a dwell count, measured on the store, never estimated.
    let depth = MessageRepository::count_dwelling(store).map_err(dispatch_store_fault)?;
    if depth >= max_queue_depth {
        return Err(DispatchFault::QueueFull {
            depth,
            limit: max_queue_depth,
        });
    }

    let allowed = route_allowed(store, from_instance, to_instance, core_agent_instance)?;
    let thread_id = match (destination.plan_step_id.as_ref(), destination.attempt) {
        (Some(step), attempt) => delegation_thread(step, attempt),
        // A row with no step is not a delegation; its thread is the instance's own, which
        // keeps the one-thread-per-correspondence rule without inventing a step.
        (None, _) => format!("thr-{}", to_instance.as_str()),
    };
    let message = MessageRow {
        message_id: MessageId::new(),
        message_type: MessageType::Instruction,
        from_agent: AgentId::core_agent(),
        to_agent: destination.agent_id.clone(),
        to_instance_id: Some(to_instance.clone()),
        from_instance_id: from_instance.clone(),
        task_id: destination.task_id.clone(),
        thread_id: thread_id.clone(),
        reply_to: None,
        priority: Priority::Normal,
        body: instruction.to_string(),
        payload_json: None,
        deadline,
        created_at: now.to_string(),
    };
    let message_id = message.message_id.clone();
    let message_type = message.message_type;
    let task_id = message.task_id.clone();
    let delivery = broker_deliver(message, |_, _| allowed, store)?;
    let replayed = matches!(delivery, Delivery::AlreadyDelivered { .. });

    if !replayed {
        record_message_event(
            store,
            athena_events::events::AgentMessageSent {
                message_id: message_id.clone(),
                message_type,
                thread_id: thread_id.clone(),
            },
            task_id.as_ref(),
            Some(to_instance),
            now,
        )?;
    }

    Ok(Dispatched {
        message_id,
        thread_id,
        replayed,
    })
}

/// Record that the addressed side took the message (`ATH-MSG-014`'s acceptance half, and
/// `ATH-MSG-007`'s `AgentMessageReceived`).
///
/// Acceptance is the *receiving* side's act, so the caller invokes this only when the
/// Runtime reported the message reached the peer. A message the peer never took stays
/// delivered-but-unaccepted, which is precisely what the dwell gauge counts
/// (`athena_message_queue_depth`) — the honest residue, never a write that claims more
/// than happened.
///
/// # Errors
/// [`DispatchFault::Store`] on a store fault; [`DispatchFault::Construction`] when the
/// Event refuses against the catalog.
pub fn accept_delivery(
    store: &mut AthenaStore,
    to_instance: &InstanceId,
    message_id: &MessageId,
    now: &str,
) -> Result<(), DispatchFault> {
    MessageRepository::mark_accepted(store, to_instance, message_id)
        .map_err(dispatch_store_fault)?;
    let row = MessageRepository::get(store, message_id).map_err(dispatch_store_fault)?;
    let (message_type, thread_id, task_id) = match row {
        Some(row) => (row.message_type, row.thread_id, row.task_id),
        // The acceptance is recorded either way; the Event states only what the store
        // holds, so a vanished row yields no fabricated payload.
        None => return Ok(()),
    };
    record_message_event(
        store,
        athena_events::events::AgentMessageReceived {
            message_id: message_id.clone(),
            message_type,
            thread_id,
        },
        task_id.as_ref(),
        Some(to_instance),
        now,
    )
}

/// Fold one message Event onto the Task's own correlation chain (the delegation leg's
/// precedent: the last non-empty chain, else a per-face mint).
fn record_message_event<P: athena_events::events::Payload>(
    store: &mut AthenaStore,
    payload: P,
    task_id: Option<&TaskId>,
    instance_id: Option<&InstanceId>,
    now: &str,
) -> Result<(), DispatchFault> {
    let correlation = match task_id {
        Some(task) => EventRepository::list_for_task(store, task)
            .map_err(dispatch_store_fault)?
            .into_iter()
            .rev()
            .map(|event| event.correlation_id)
            .find(|chain| !chain.is_empty())
            .unwrap_or_else(|| athena_events::redact::digest(&format!("message-{now}"))),
        None => athena_events::redact::digest(&format!("message-{now}")),
    };
    let typed = athena_events::TypedEvent::new(
        payload,
        athena_events::EventContext {
            event_id: EventId::new(),
            occurred_at: now.to_string(),
            // The Broker is Core Host's own face (ATH-MSG-011): the actor of the record is
            // the system, while the correspondents named in the payload are the Agents.
            actor: athena_events::envelope::Actor::System,
            task_id: task_id.cloned(),
            plan_step_id: None,
            agent_id: None,
            instance_id: instance_id.cloned(),
            skill_id: None,
            correlation_id: correlation,
            causation_id: None,
        },
    )
    .map_err(|fault| DispatchFault::Construction(fault.to_string()))?;
    EventRepository::append(
        store,
        &NewEvent {
            event_id: typed.event_id.clone(),
            schema_version: typed.schema_version,
            event_type: typed.event_type.to_string(),
            occurred_at: typed.occurred_at.clone(),
            task_id: typed.task_id.clone(),
            plan_step_id: typed.plan_step_id.clone(),
            agent_id: typed.agent_id.clone(),
            instance_id: typed.instance_id.clone(),
            skill_id: typed.skill_id.clone(),
            correlation_id: typed.correlation_id.clone(),
            causation_id: typed.causation_id.clone(),
            actor: typed.actor.as_str().to_string(),
            payload_json: typed.payload_json.clone(),
        },
    )
    .map_err(dispatch_store_fault)?;
    Ok(())
}

fn dispatch_store_fault(error: PersistenceError) -> DispatchFault {
    DispatchFault::Store(error.to_string())
}

/// The Runtime-boundary projection of a dispatched instruction (§10.2's envelope as the
/// Runtime carries it, `plan_intake::control_response_message`'s precedent): the caller
/// hands this to `Runtime::send`, so the composition root needs no `athena-runtime` edge
/// of its own (spec §32.2).
///
/// Every field restates what the stored row already says — the identity, the thread and
/// the deadline the Broker recorded — so the body the peer reads and the body the store
/// holds are one text.
#[must_use]
pub fn instruction_message(
    dispatched: &Dispatched,
    from_instance: &InstanceId,
    body: &str,
    deadline: Option<String>,
) -> athena_runtime::Message {
    athena_runtime::Message {
        message_id: dispatched.message_id.clone(),
        message_type: MessageType::Instruction,
        from_agent: Some(AgentId::core_agent()),
        from_instance_id: Some(from_instance.clone()),
        thread_id: dispatched.thread_id.clone(),
        reply_to: None,
        priority: Priority::Normal,
        body: body.to_string(),
        payload_json: None,
        deadline,
    }
}
