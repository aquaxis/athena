//! The Broker's *decision semantics* (design §6.1) — the driver that touches the store
//! lives in `athena-core::broker` (an allowed edge there; the crate's own dependency list
//! stays exactly `athena-types`, spec §32.2's allow-list for this surface).
//!
//! Module flow (text, not executable):
//!
//! 1. shape check: Core Host-issued `thread_id` and addressing (`MessageUnderway::check`)
//! 2. hierarchy route: the parent-child edge and the sibling ACL recorded at composition
//!    (`ATH-MSG-018`, checked through the caller's World closure)
//! 3. dedup and persistence happen in the store's hands (`ATH-MSG-012/014`; the record is
//!    the ack contract) — anchored in `athena-core::broker`
//! 4. the outcome fact carries the Event anchoring for `AgentMessageSent` /
//!    `AgentMessageReceived` (D-1: the caller appends the Events)

use athena_types::InstanceId;
use thiserror::Error;

/// Why a delivery settled the way it did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// A fresh delivery: the caller's route admitted it; the store row is the ack
    /// contract (ATH-MSG-012).
    DeliveredFresh {
        /// The message that went out.
        message_id: String,
        /// The receiving instance.
        to_instance: String,
        /// The thread.
        thread_id: String,
    },
    /// A dedup hit (`to_instance_id, message_id` already delivered): the caller must not
    /// invoke the Runtime again (ATH-MSG-014).
    AlreadyDelivered {
        /// The message that was skipped.
        message_id: String,
        /// The receiving instance.
        to_instance: String,
    },
}

/// Why a send was refused by the Broker (never an agent judgment).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DeliveryRefusal {
    /// The in-flight message's shape was invalid.
    #[error("{0}")]
    Envelope(#[from] crate::envelope::SendRefusal),
    /// The hierarchy route did not permit it — or the sibling ACL ruled this pair out
    /// (ATH-MSG-018).
    #[error("the route was denied: neither parent-child nor an ACL sibling edge")]
    RouteDenied,
}

impl DeliveryRefusal {
    /// The §20.1 class: an ACL denial is `PermissionError` (an attempted route the agent
    /// was not granted); shape refusals are `ValidationError`.
    #[must_use]
    pub const fn class(&self) -> &'static str {
        match self {
            Self::Envelope(_) => "ValidationError",
            Self::RouteDenied => "PermissionError",
        }
    }

    /// A stable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Envelope(envelope) => match envelope {
                crate::envelope::SendRefusal::MissingThread => "ROUTE_MISSING_THREAD",
                crate::envelope::SendRefusal::MissingInstance => "ROUTE_MISSING_INSTANCE",
                crate::envelope::SendRefusal::PlaintextSecret => "ROUTE_PLAINTEXT_SECRET",
                crate::envelope::SendRefusal::SelfAddressed => "ROUTE_SELF_ADDRESSED",
            },
            Self::RouteDenied => "ROUTE_SIBLING_DENIED",
        }
    }
}

/// The pure route decision: shape, then the World's route legality.
///
/// `world` carries the visible legal pairs: `(from instance, to instance)` true when the
/// parent-child edge covers it, or when the sibling ACL recorded the pair
/// (ATH-MSG-018).
///
/// # Errors
/// [`DeliveryRefusal::Envelope`] on a shape refusal,
/// [`DeliveryRefusal::RouteDenied`] when the World denies the route.
pub fn route_check(
    underway: &crate::envelope::MessageUnderway,
    world: impl Fn(&InstanceId, &InstanceId) -> bool,
) -> Result<(), DeliveryRefusal> {
    underway.check()?;
    let Some(to_instance) = &underway.to_instance else {
        return Err(DeliveryRefusal::Envelope(
            crate::envelope::SendRefusal::MissingInstance,
        ));
    };
    if !world(&underway.from_instance, to_instance) {
        return Err(DeliveryRefusal::RouteDenied);
    }
    Ok(())
}

/// The kind-level view of the never-dropped rule (ATH-MSG-012).
#[must_use]
pub fn is_undroppable_of(message_type: &athena_types::MessageType) -> bool {
    crate::envelope::is_undroppable(message_type)
}

/// The Broker's node type: the driver (`athena-core::broker`) validates, dedups and
/// persists with these rules; the `Broker` struct itself carries no state.
#[derive(Debug, Default, Clone, Copy)]
pub struct Broker;

impl Broker {
    /// The pure decision on a message-in-flight (see [`route_check`]).
    ///
    /// # Errors
    /// [`DeliveryRefusal`].
    pub fn deliver_check<World>(
        &self,
        message: &crate::envelope::MessageUnderway,
        world: World,
    ) -> Result<(), DeliveryRefusal>
    where
        World: Fn(&InstanceId, &InstanceId) -> bool,
    {
        route_check(message, world)
    }
}
