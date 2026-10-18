//! Errors crossing the Runtime boundary (spec section 20.1).
//!
//! The Runtime reports what happened to a process or a transport; everything else —
//! validation of agent-level meaning, policy, approval — is Core Host's judgement, not the
//! Runtime's (ATH-ACLI-006, ATH-CORE-020). Unclassifiable faults stay `Internal` and are
//! never retried (ATH-FAIL-002).

use thiserror::Error;

/// Result of a Runtime operation.
pub type Result<T> = std::result::Result<T, RuntimeError>;

/// Why a Runtime operation failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RuntimeError {
    /// The handle refers to no active Runtime Instance (ATH-RT-012 — 1:1, nothing else to
    /// fall back to).
    #[error("runtime instance {0} is unknown to this Runtime")]
    UnknownInstance(String),
    /// The operation is outside this Runtime's declared capabilities
    /// (ATH-RT-005, ATH-SEC-015).
    #[error("operation {0:?} is not offered by this runtime")]
    Unsupported(String),
    /// A startup or execution deadline passed (ATH-RT-006) — `TimeoutError`.
    #[error("runtime deadline passed while {0}")]
    Deadline(String),
    /// Start failed transiently and may be retried without a fresh decision; this is the
    /// only class excluded from the Core Agent restart window count (ATH-CORE-031).
    #[error("transient runtime fault: {0}")]
    Transient(String),
    /// The Runtime received input it cannot act on — `ValidationError`.
    #[error("invalid runtime input: {0}")]
    Validation(String),
    /// The agent process exited abnormally or the transport broke — `RuntimeError`
    /// (ATH-ACLI-005).
    #[error("runtime failure: {0}")]
    Failure(String),
    /// Runtime state inconsistent with the store's record — `InternalError`; never retried
    /// (ATH-FAIL-002).
    #[error("internal runtime error: {0}")]
    Internal(String),
}

impl RuntimeError {
    /// The §20.1 nine-class taxonomy value this error reports as.
    #[must_use]
    pub fn class(&self) -> &'static str {
        match self {
            Self::UnknownInstance(_) | Self::Unsupported(_) | Self::Validation(_) => {
                "ValidationError"
            }
            Self::Deadline(_) => "TimeoutError",
            Self::Transient(_) => "TransientError",
            Self::Failure(_) => "RuntimeError",
            Self::Internal(_) => "InternalError",
        }
    }
}
