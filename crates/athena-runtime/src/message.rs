//! The Runtime-boundary view of a cross-boundary message (spec §10.2 envelope).
//!
//! The full envelope is `athena-messaging`'s (Phase 2); this is the minimal projection the
//! Runtime trait carries. It names the message, its kind, who sent it, and the natural
//! body; the Runtime never interprets any of it (ATH-ACLI-006).

use athena_types::{AgentId, InstanceId, MessageId, MessageType, Priority, Timestamp};
use serde::{Deserialize, Serialize};

/// A message delivered to, from, or between agent processes through the Core.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// Identifier, Core-issued (ATH-CORE-041).
    pub message_id: MessageId,
    /// Kind, from the §10.1 set of twelve.
    pub message_type: MessageType,
    /// Sending agent, when the sender is an agent; Core is transport, never an endpoint
    /// (ATH-MSG-011).
    pub from_agent: Option<AgentId>,
    /// Sending instance; `None` only for system-injected items.
    pub from_instance_id: Option<InstanceId>,
    /// The thread this belongs to, one thread per delegation (ATH-MSG-004).
    pub thread_id: String,
    /// The message being replied to.
    pub reply_to: Option<MessageId>,
    /// Queue priority.
    pub priority: Priority,
    /// Natural-language body; carries no plaintext secret (ATH-MSG-008).
    pub body: String,
    /// Structured payload, when the kind carries one (e.g. `control_request`).
    pub payload_json: Option<String>,
    /// Reply deadline for `instruction` / `question` (ATH-MSG-005).
    pub deadline: Option<Timestamp>,
}

impl Message {
    /// Ask the Runtime whether this message kind must never be dropped or refused
    /// (ATH-MSG-012).
    #[must_use]
    pub fn is_undroppable(&self) -> bool {
        self.message_type.is_undroppable()
    }
}
