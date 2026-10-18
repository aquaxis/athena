//! The CLI exit codes (P6-05; `ATH-CLI-008` MUST): success 0, request-shape violation 2,
//! permission refusal 3, execution failure 4, internal error 70.
//!
//! The central contract: every CLI verb's failure rides one of these codes, and the
//! routing face maps the API's error classes onto them (`ATH-API-007`'s class field —
//! the `ValidationError` / `PermissionError` / `PolicyError` faces are the CLI's
//! request/permission/execution faces).

use crate::Outcome;

/// The §20.1 class, as the envelope's wire-form name the API answers with
/// (`ATH-API-007`'s `class` field; a local wire-face copy — the CLI rides the Core
/// API only, no `athena-core` edge, §32.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// `validation_error`.
    ValidationError,
    /// `permission_error`.
    PermissionError,
    /// `policy_error`.
    PolicyError,
    /// `transient_error`.
    TransientError,
    /// `timeout_error`.
    TimeoutError,
    /// `agent_error`.
    AgentError,
    /// `skill_error`.
    SkillError,
    /// `runtime_error`.
    RuntimeError,
    /// `internal_error`.
    InternalError,
}

impl ErrorClass {
    /// Parse the envelope's wire name.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "validation_error" => Self::ValidationError,
            "permission_error" => Self::PermissionError,
            "policy_error" => Self::PolicyError,
            "transient_error" => Self::TransientError,
            "timeout_error" => Self::TimeoutError,
            "agent_error" => Self::AgentError,
            "skill_error" => Self::SkillError,
            "runtime_error" => Self::RuntimeError,
            "internal_error" => Self::InternalError,
            _ => return None,
        })
    }
}

/// The exit codes (ATH-CLI-008).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ExitCode {
    /// Success.
    Success = 0,
    /// The request's shape violated the contract (a missing pin, an unknown verb).
    RequestShapeViolation = 2,
    /// The caller's role refused the operation.
    PermissionRefusal = 3,
    /// The request was shapable but the run failed (the API's 409-family / 5xx faces).
    ExecutionFailure = 4,
    /// An internal error (the store broken, the transport unreachable).
    InternalError = 70,
}

impl ExitCode {
    /// The code as the process's face.
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self as i32
    }

    /// Map the protocol's error class face onto the CLI's exit code (the wire face the
    /// `ErrorEnvelope` carries; `ATH-API-007`'s `class` field).
    #[must_use]
    pub const fn from_error_class(class: &ErrorClass) -> Self {
        match class {
            ErrorClass::ValidationError => Self::RequestShapeViolation,
            ErrorClass::PermissionError | ErrorClass::PolicyError => Self::PermissionRefusal,
            ErrorClass::InternalError => Self::InternalError,
            _ => Self::ExecutionFailure,
        }
    }
}

/// Build a failing outcome with the mapped code, from one error envelope's class.
///
/// # Errors
/// Never — the constructor is total.
#[must_use]
pub fn outcome_of_class(class: &ErrorClass, message: impl Into<String>) -> Outcome {
    Outcome::fail(ExitCode::from_error_class(class).as_i32(), message)
}

#[cfg(test)]
mod exit_code_tests;
