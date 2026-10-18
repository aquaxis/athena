//! Every error response is envelope-shaped, class-mapped, and correlation-carrying
//! (P1-38; ATH-API-007, ATH-API-008, ATH-API-009, R-API-07).
//!
//! `ApiError` is one value from construction to wire: the §22.2 envelope carries
//! `{ class, code, message, details, correlation_id }`, the status derives from the
//! §22.2 class mapping, and the correlation id is the request's own chain id (echoed
//! header, or a minted `cor-…`), stamped into the response *header* as well as the
//! envelope — so log lines, Events and API failures join on one key.

use athena_core::protocol::{ErrorClass, ErrorEnvelope};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, http::HeaderMap};
use serde_json::{Value, json};

/// The correlation header the API speaks (`X-Correlation-Id`).
pub const CORRELATION_HEADER: &str = "x-correlation-id";

/// One error response, ready to serialize.
#[derive(Debug, Clone)]
pub struct ApiError {
    /// The §22.2 envelope.
    pub envelope: ErrorEnvelope,
    /// The §22.2-mapped HTTP status.
    pub status: StatusCode,
}

impl ApiError {
    /// Build with the class's default status (spec §22.2's mapping).
    #[must_use]
    pub fn of_class(
        class: ErrorClass,
        code: impl Into<String>,
        message: impl Into<String>,
        details: Value,
        correlation_id: impl Into<String>,
    ) -> Self {
        Self {
            envelope: ErrorEnvelope {
                class,
                code: code.into(),
                message: message.into(),
                details,
                correlation_id: correlation_id.into(),
            },
            status: StatusCode::from_u16(class.default_status())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        }
    }

    /// A constructor per §22.2 destination, for the handlers' own nomenclature:
    /// the class stays explicit so the wire value is always the taxonomy's.
    #[must_use]
    pub fn of_status(
        class: ErrorClass,
        status: StatusCode,
        code: impl Into<String>,
        message: impl Into<String>,
        details: Value,
        correlation_id: impl Into<String>,
    ) -> Self {
        Self {
            envelope: ErrorEnvelope {
                class,
                code: code.into(),
                message: message.into(),
                details,
                correlation_id: correlation_id.into(),
            },
            status,
        }
    }

    /// 401 (`—`): no class, as the mapping table records ("unauthenticated" carries no
    /// taxonomy value because no inspection happened).
    #[must_use]
    pub fn unauthenticated(correlation_id: impl Into<String>) -> Self {
        Self::of_status(
            ErrorClass::ValidationError,
            StatusCode::UNAUTHORIZED,
            "AUTH_UNAUTHENTICATED",
            "no `Authorization: Bearer <token>` was presented",
            json!({}),
            correlation_id,
        )
    }

    /// 401 unknown token.
    #[must_use]
    pub fn unknown_token(correlation_id: impl Into<String>) -> Self {
        Self::of_status(
            ErrorClass::ValidationError,
            StatusCode::UNAUTHORIZED,
            "AUTH_UNKNOWN_TOKEN",
            "the presented token matches no issued hash",
            json!({}),
            correlation_id,
        )
    }

    /// 403 under-role.
    #[must_use]
    pub fn role_insufficient(required: &str, correlation_id: impl Into<String>) -> Self {
        Self::of_status(
            ErrorClass::PermissionError,
            StatusCode::FORBIDDEN,
            "AUTH_ROLE_INSUFFICIENT",
            format!("this endpoint requires role `{required}`"),
            json!({ "required_role": required }),
            correlation_id,
        )
    }

    /// The correlation id this error carries.
    #[must_use]
    pub fn correlation_id(&self) -> &str {
        &self.envelope.correlation_id
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (self.status, Json(json!({ "error": self.envelope }))).into_response();
        if let Ok(value) = axum::http::HeaderValue::from_str(&self.envelope.correlation_id) {
            response.headers_mut().insert(CORRELATION_HEADER, value);
        }
        response
    }
}

/// Extract or mint the correlation id of a request (ATH-API-009).
///
/// The header echoes so a client that brought a chain id (its invocation's Task link)
/// gets its value back on every response; a request without one is minted a `cor-…`.
#[must_use]
pub fn correlation_of(headers: &HeaderMap, now_seed: u64) -> String {
    headers
        .get(CORRELATION_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.starts_with("cor-") && !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            // Uniqueness per request: the call-time seed mixes with a process-local
            // counter, so two mints in the same invocation never collide.
            static MINTED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let handed = MINTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let hasher = HashSeed::new(now_seed ^ handed);
            format!("cor-{:032}", hasher.next_u128())
        })
}

struct HashSeed(u64);

impl HashSeed {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    #[allow(clippy::missing_panics_doc, reason = "8 of 32 bytes always suffice")]
    fn next_u128(&self) -> u128 {
        // SplitMix64 twice: cheap and enough for the dev mint's uniqueness.
        let mut z = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        let a = z as u128;
        let mut w = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
        w = (w ^ (w >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        w = (w ^ (w >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        a | ((w as u128) << 64)
    }
}

#[cfg(test)]
#[path = "errors_tests.rs"]
mod errors_tests;
