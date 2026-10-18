//! The broker-side envelope helpers (spec §10.2): message types, the never-dropped
//! classification (ATH-MSG-012), addressing, the sibling-ACL check helper
//! (ATH-MSG-018), the deadline rule (ATH-MSG-005/006), and the no-plaintext-secrets
//! scan for bodies (ATH-MSG-008).

use athena_types::{AgentId, InstanceId, MessageId, MessageType, Priority, Timestamp};
use thiserror::Error;

/// The never-dropped kinds (ATH-MSG-012): losing a completed agent's report is
/// unrecoverable, so back-pressure must not apply to these.
#[must_use]
pub fn is_undroppable(message_type: &MessageType) -> bool {
    message_type.is_undroppable()
}

/// The Broker-acknowledged kinds must be persisted before the ack (ATH-MSG-012).
#[must_use]
pub fn requires_persist_before_ack(message_type: &MessageType) -> bool {
    is_undroppable(message_type)
}

/// Which body forms are acceptable: `body` must never carry a plaintext secret
/// (ATH-MSG-008) — the check is the same masking shapes the Events use, delegated to the
/// caller via a single spill word.
#[must_use]
pub fn contains_secret_marker(body: &str) -> bool {
    body.contains("-----BEGIN")
        || (!body.contains("[REDACTED]")
            && (body.contains("password =") || body.contains("api_key") || body.contains("AKIA")))
}

/// Why a send was refused by the Broker (never an agent judgment).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SendRefusal {
    /// The thread id was empty (ATH-MSG-013: Core Host issues the thread id per
    /// delegation; the Broker cannot mint one).
    #[error("thread_id is Core-issued per delegation; an empty id cannot route")]
    MissingThread,
    /// The recipient instance was not addressed (ATH-MSG-010: addressing is by
    /// `to_instance_id`; agent-only addressing is allowed solely for Core Agent).
    #[error(
        "the message must address a to_instance; agent-only addressing is Core Agent's alone (ATH-MSG-010)"
    )]
    MissingInstance,
    /// A plaintext secret rode the body (ATH-MSG-008).
    #[error("the message body carries plaintext secrets")]
    PlaintextSecret,
    /// Both from-instance and to-instance address the same value as the to-instance, but
    /// with no hierarchy legality backing it.
    #[error("self-addressed message: from and to share the same instance")]
    SelfAddressed,
}

/// The helper struct for route checks on a message in flight.
pub struct MessageUnderway {
    /// The message id.
    pub message_id: MessageId,
    /// From.
    pub from: AgentId,
    /// From instance.
    pub from_instance: InstanceId,
    /// To.
    pub to: AgentId,
    /// To instance (addressing key).
    pub to_instance: Option<InstanceId>,
    /// The thread.
    pub thread_id: String,
    /// Priority.
    pub priority: Priority,
    /// The body.
    pub body: String,
    /// Reply deadline.
    pub deadline: Option<Timestamp>,
}

impl MessageUnderway {
    /// The route check on the in-flight message.
    ///
    /// # Errors
    /// [`SendRefusal`] variants, whole.
    pub fn check(&self) -> Result<(), SendRefusal> {
        if self.thread_id.trim().is_empty() {
            return Err(SendRefusal::MissingThread);
        }
        if self.to_instance.is_none() {
            return Err(SendRefusal::MissingInstance);
        }
        if self.to_instance.as_ref() == Some(&self.from_instance) {
            return Err(SendRefusal::SelfAddressed);
        }
        if contains_secret_marker(&self.body) {
            return Err(SendRefusal::PlaintextSecret);
        }
        Ok(())
    }
}
