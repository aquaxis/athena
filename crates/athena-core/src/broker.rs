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
