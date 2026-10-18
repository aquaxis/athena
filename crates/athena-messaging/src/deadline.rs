//! Reply deadlines (P2-16; ATH-MSG-005/006, ATH-STATE-023).
//!
//! The module is *pure* (the crate's dependency list stays `athena-types`): the caller
//! (Core Host's Broker driver) passes the per-message facts it wants checked.
//!
//! `instruction` / `question` carry a reply deadline (default
//! `timeout.message_reply`), configuration forcing it above `startup + execution`
//! (ATH-CFG-005's cross-constraint). Expiry is a `TimeoutError` — and the treatment
//! splits by receiver: a subordinate's deadline is `WAITING → FAILED` through the State
//! Manager; an expiry observed on the Core Agent's own wait rides
//! `AgentMessageTimeout` and never fails instance #0 (ATH-STATE-023).

use athena_types::{AgentId, MessageId, MessageType, Timestamp};
use thiserror::Error;

/// Why a deadline observation fired.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DeadlineFault {
    /// The deadline passed; where the receiver sits decides the treatment.
    #[error("reply deadline {deadline} passed for message {message_id}")]
    Expired {
        /// The message the deadline attaches to.
        message_id: String,
        /// The thread the deadline rode.
        thread_id: String,
        /// The deadline that passed (RFC 3339).
        deadline: String,
    },
}

impl DeadlineFault {
    /// The `AgentMessageTimeout` payload rows (§23.2's minimum).
    #[must_use]
    pub fn payload(&self) -> TimeoutPayload {
        TimeoutPayload {
            message_id: MessageId::parse(match self {
                Self::Expired { message_id, .. } => message_id.clone(),
            })
            .unwrap_or_default(),
            thread_id: match self {
                Self::Expired { thread_id, .. } => thread_id.clone(),
            },
            deadline: match self {
                Self::Expired { deadline, .. } => deadline.clone(),
            },
        }
    }
}

/// The `AgentMessageTimeout` payload rows, typed (§23.2's minimum).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeoutPayload {
    /// The message that timed out.
    pub message_id: MessageId,
    /// Its thread.
    pub thread_id: String,
    /// The deadline that passed.
    pub deadline: Timestamp,
}

/// Whether the message even carries a reply-deadline duty
/// (`instruction` / `question` only; ATH-MSG-005).
#[must_use]
pub const fn carries_deadline(message_type: &MessageType) -> bool {
    matches!(
        message_type,
        MessageType::Instruction | MessageType::Question
    )
}

/// Whether the deadline's receiver is the Core Agent — the observation treatment's
/// signal (a message to a subordinate rides the State Manager's row; a Core-Agent-side
/// expiry never ends instance #0, ATH-STATE-023).
#[must_use]
pub fn receiver_is_core_agent(receiver: &AgentId) -> bool {
    receiver.is_core_agent()
}

/// The deadline observation: an `instruction`/`question` whose RFC 3339 UTC `deadline`
/// is at or before `now` fires the fault (`message_id`, `thread_id`, `deadline` — the
/// Event's minimum payload rows) with the *receiver* attached for the treatment split.
#[must_use]
pub fn expired_at(deadline: Option<&str>, now_rfc3339: &str) -> bool {
    match deadline {
        Some(text) => text <= now_rfc3339,
        None => false,
    }
}

/// The typed expiry for one waiter-record the caller keeps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitRecord {
    /// The message the deadline attaches to.
    pub message_id: String,
    /// The thread.
    pub thread_id: String,
    /// The deadline (RFC 3339 UTC).
    pub deadline: String,
    /// The receiving agent (the treatment signal reads its kind).
    pub receiver: AgentId,
}

impl WaitRecord {
    /// Expire at `now`: `None` while the deadline holds.
    ///
    /// # Parameters
    /// `now_rfc3339` — same-format RFC 3339 UTC (string order IS time order for
    /// same-format, offset-free values; the Configuration writes 000Z).
    #[must_use]
    pub fn expire_at(&self, now_rfc3339: &str) -> Option<DeadlineFault> {
        if expired_at(Some(&self.deadline), now_rfc3339) {
            Some(DeadlineFault::Expired {
                message_id: self.message_id.clone(),
                thread_id: self.thread_id.clone(),
                deadline: self.deadline.clone(),
            })
        } else {
            None
        }
    }
}
