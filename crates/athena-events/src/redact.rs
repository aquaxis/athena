//! Payload hygiene (ATH-EVENT-007, R-SEC-10, R-POL-13).
//!
//! Two rules apply to every payload before it is serialized onto the envelope:
//!
//! 1. **Masking.** Known secret shapes are replaced inside every string value, so a value
//!    that reaches a payload through a field whose type is `String` cannot leak the secret
//!    verbatim into the Event store. The same masking shapes apply to log output elsewhere.
//! 2. **No full agent input.** Payload keys are checked against a ban list; the keys that
//!    would embed an agent instruction, a prompt, or a message body verbatim are refused, so
//!    bulk content is referenced instead (Result ID, AI_LOG reference, digest).
//!
//! Both checks run in [`TypedEvent::new`] (module `events`), so an Event violating either
//! cannot reach the store through the typed path.

use regex::Regex;
use std::sync::OnceLock;

/// Result of payload hygiene.
pub type Result<T> = std::result::Result<T, EventError>;

/// Why a payload was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EventError {
    /// Payload structure did not match the §23.2 mandate for its type (ATH-EVENT-017).
    #[error("event payload is missing mandatory field `{0}` (ATH-EVENT-017)")]
    MissingField(&'static str),
    /// Payload embedded content the specification forbids (ATH-EVENT-007).
    #[error("event payload contains forbidden content ({0}): {1}")]
    ForbiddenContent(String, String),
    /// Correlation or envelope text was unusable.
    #[error("event envelope is invalid: {0}")]
    InvalidEnvelope(String),
}

/// Keys whose presence in a payload would embed full agent input verbatim (ATH-EVENT-007).
///
/// Keys are normalized (lowercased, separators removed) before comparison, so
/// `instruction_text` and `fullText` are caught. Bulk content is expressed as a reference
/// instead — a Result ID, an AI_LOG reference, or a digest
/// (`TaskCreated.instruction_digest`, `ClarificationRequested.question_digest`).
pub const FORBIDDEN_KEYS: &[&str] = &[
    "instruction",
    "prompt",
    "input",
    "body",
    "fulltext",
    "agentinput",
    "plaintext",
];

fn normalize_key(key: &str) -> String {
    key.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Whether a key would embed full agent input.
#[must_use]
pub fn key_is_forbidden(key: &str) -> bool {
    FORBIDDEN_KEYS.contains(&normalize_key(key).as_str())
}

/// Validate a JSON value against the forbidden-key rule, recursively.
///
/// # Errors
/// [`EventError::ForbiddenContent`] when any key would embed full agent input.
pub fn forbid_full_input(value: &serde_json::Value) -> Result<()> {
    match value {
        serde_json::Value::Object(map) => {
            for (key, inner) in map {
                if key_is_forbidden(key) {
                    return Err(EventError::ForbiddenContent(
                        key.clone(),
                        "bulk content must be referenced, not embedded (ATH-EVENT-007)".into(),
                    ));
                }
                forbid_full_input(inner)?;
            }
            Ok(())
        }
        serde_json::Value::Array(items) => items.iter().try_for_each(forbid_full_input),
        _ => Ok(()),
    }
}

/// Known secret shapes, compiled once.
fn secret_patterns() -> &'static Vec<Regex> {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            // PEM private keys: the block is never worth keeping verbatim.
            Regex::new(r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----")
                .expect("static pattern"),
            // Bearer auth material.
            Regex::new(r#"(?i)bearer\s+[a-z0-9._~+/=-]{8,}"#).expect("static pattern"),
            // Assigned credential pairs (`password = hunter2`, `"api_key": "…"`): the
            // separator group keeps the label readable in the record.
            Regex::new(
                r#"(?i)((?:ssh-)?(?:api[_-]?key|secret|token|password|passwd|access[_-]?key)(?:["'\s]*[:=]["'\s]*)?)([^\s"',<>\\/]{6,})"#,
            )
            .expect("static pattern"),
            // AWS access key identifiers.
            Regex::new(r"\bAKIA[0-9A-Z]{16}\b").expect("static pattern"),
            // Personal access tokens used by common hosts.
            Regex::new(
                r"(?i)\b(?:ghp|gho|ghu|ghs|ghr|xox[bap]|github_pat_|sk-)[a-z0-9_\-]{8,}",
            )
            .expect("static pattern"),
        ]
    })
}

/// The replacement written over masked material.
pub const REDACTED: &str = "[REDACTED]";

/// Mask known secret shapes inside one string value.
///
/// Labeled credential pairs keep their label and separator so a masked record stays
/// readable (`password: hunter2` → `password: [REDACTED]`); everything else is overwritten
/// whole.
#[must_use]
pub fn mask_text(text: &str) -> String {
    let mut out = text.to_string();
    for pattern in secret_patterns() {
        out = pattern
            .replace_all(&out, |caps: &regex::Captures| {
                // The pair pattern has a single capture group (the label); the others have
                // none and are replaced whole.
                if pattern.captures_len() > 1 && caps.get(1).is_some() {
                    format!("{}{}", &caps[1], REDACTED)
                } else {
                    REDACTED.to_string()
                }
            })
            .into_owned();
    }
    out
}

/// Mask every string value of a JSON value, recursively.
#[must_use]
pub fn mask_value(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => serde_json::Value::String(mask_text(&text)),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(mask_value).collect())
        }
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(key, inner)| (key, mask_value(inner)))
                .collect::<serde_json::Map<String, serde_json::Value>>(),
        ),
        other => other,
    }
}

/// A stable short digest of free-text input, for `instruction_digest` /
/// `question_digest` / `contextual_digest`-style payload fields.
///
/// SHA-256 taken over the trimmed text, hex-encoded, first 16 characters — the same shape
/// the specification prescribes for `ProjectId` derivation (spec section 26.1), applied
/// here as a reference instead of embedding the text (ATH-EVENT-007).
#[must_use]
pub fn digest(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(text.trim().as_bytes());
    let fine_print = hasher.finalize();
    hex16(fine_print.as_ref())
}

/// Lower-case hex of the first 16 bytes.
fn hex16(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(32);
    for byte in bytes.iter().take(16) {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
