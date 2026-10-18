//! The Core API HTTP skeleton (spec §22, P1-37).
//!
//! One binary surface, three roles: `Authorization: Bearer <token>` per ATH-SEC-027.
//!
//! - [`auth`]: the token registry — tokens that the administrator issues, each carrying
//!   exactly one role and stored **as a hash** (`sha256`); the register thus never holds
//!   plaintext (ATH-SEC-005), lives under `<ATHENA_HOME>/var/` runtime data (ATH-OPS-02),
//!   and answers only what the *caller* (process boundary) presents.
//! - [`roles`]: the three-role hierarchy from spec §29.2 (`viewer` ⊂ `operator` ⊂ `admin`).
//! - The route table: `/api/v1` scope with a per-route required role, `/healthz` as the
//!   single anonymous endpoint (R-API-03, no internal state exposed).
//! - Authentication / authorization denials are surfaced through the §22.2 envelope
//!   already (R-API-07); the audit Events and logs stay the caller's job (ATH-SEC-006 —
//!   wired when the orchestration lands).
//!
//! Framework: `axum` (spec §22.1: HTTP/1.1+, JSON body, Bearer). The skeleton renders the
//! HTTP surface; the request handlers delegate to the Core API contract per-endpoint
//! (P1-40 tasks ahead).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod approvals;
pub mod auth;
pub mod conversations;
pub mod errors;
/// `Idempotency-Key` handling: replay-identical outcomes over the persisted claims (P1-39, R-API-05).
pub mod idempotency;
pub mod memory;
/// Cursor pagination: `?limit=&cursor=`, `next_cursor` in the response (P1-39, R-API-06).
pub mod metrics;
pub mod pagination;
pub mod roles;
pub mod routes;
pub mod skills;
pub mod tasks;

#[cfg(test)]
mod routes_tests;

pub use auth::{ApiCaller, AuthMode, TokenRegistry};
pub use errors::{ApiError, CORRELATION_HEADER};
pub use roles::Role;
pub use routes::{AppState, build_router};

/// The API's config surface, handed in by the orchestrating process (ATH-CFG-006).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ApiBind {
    /// The bind address (`127.0.0.1` only for `AuthMode::None`, ATH-CFG-006).
    pub host: String,
    /// The bind port (default 8080).
    pub port: u16,
    /// Which authentication mode the startup chose.
    pub auth: AuthMode,
    /// Path of the registry file, under `<ATHENA_HOME>/var/` (R-OPS-02).
    pub token_file: std::path::PathBuf,
}
