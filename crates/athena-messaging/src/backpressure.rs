//! Queue-depth back-pressure (P2-15; ATH-MSG-009).
//!
//! Back-pressure applies **only** to `instruction` / `status` / `question` — beyond
//! `messaging.max_queue_depth` these answer a `TransientError`: the *sender* retries
//! with backoff (ATH-MSG-009's sender-side rule). The never-dropped kinds inbound
//! regardless: losing a completed agent's report is unrecoverable (ATH-MSG-012).

use athena_types::MessageType;
use thiserror::Error;

/// What back-pressure produced for one inbound send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackPressure {
    /// Admitted under the ceiling.
    Admit,
    /// The kind may be refused: the depth is over the ceiling, and only the three
    /// back-pressurable kinds land here.
    Refuse,
}

impl BackPressure {
    /// Whether the refused send answers `TransientError` (ATH-FAIL-006's retryable class).
    #[must_use]
    pub fn is_transient(self, kind: &MessageType) -> bool {
        matches!(self, Self::Refuse) && kind.is_back_pressurable()
    }
}

/// The rule: one inbound message to an instance's queue at `depth`, against the
/// configured `max_queue_depth`.
#[must_use]
pub fn decide(kind: &MessageType, depth: u64, max_queue_depth: u64) -> BackPressure {
    if depth > max_queue_depth && kind.is_back_pressurable() {
        BackPressure::Refuse
    } else {
        BackPressure::Admit
    }
}

/// Whether the inbound kind is even askable for back-pressure
/// (only instruction/status/question; the never-dropped kinds never hit this).
#[must_use]
pub fn is_back_pressurable(kind: &MessageType) -> bool {
    kind.is_back_pressurable()
}

/// Why a send was refused (the §20.1 surface).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum QueueFull {
    /// Beyond the ceiling, back-pressurable: the sender retries with backoff
    /// (`TransientError`, ATH-MSG-009).
    #[error("queue depth {depth} is over the ceiling {ceiling}")]
    Transient {
        /// The depth at refusal.
        depth: u64,
        /// The configured ceiling.
        ceiling: u64,
    },
    /// Never-dropped kinds do not refuse here (the schema is the guard; the caller
    /// should not have asked): surfaced as intra-broker `InternalError`.
    #[error("the never-dropped kind {0:?} must never receive back-pressure")]
    ForbiddenDrop(MessageType),
}
