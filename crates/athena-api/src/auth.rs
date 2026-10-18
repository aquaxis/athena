//! The token registry (spec §29.2, ATH-SEC-005/027).
//!
//! The administrator issues each token with exactly one role; only the SHA-256 hash of
//! the token is persisted — the plaintext never sees the registry, so the file under
//! `<ATHENA_HOME>/var/` is safe to back up and to audit (R-OPS-02 modifiable, R-POL-13
//! no plaintext secret). The caller presents `Authorization: Bearer <token>`; the
//! registry hashes what arrives and compares.

use crate::roles::Role;
use crate::routes::bearer_of;
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// The authentication mode for this process (spec §28.1's `athena.api.auth.mode`).
///
/// The caller (the orchestrating binary) owns the config read; the API takes the resolved
/// mode and enforces the loopback rule at bind time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthMode {
    /// Static bearer tokens — v1's scheme (ATH-SEC-027).
    #[default]
    Token,
    /// No authentication — permitted only on `127.0.0.1` (`ATH-CFG-006`).
    None,
}

/// One issued token, in registry form: hash + role + a name for the audit trail.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IssuedToken {
    /// Audit-facing name.
    pub name: String,
    /// The role the administrator granted (exactly one, ATH-SEC-027).
    pub role: Role,
    /// The SHA-256 hex of the token (ATH-SEC-005 — hashed-only storage).
    pub sha256_hex: String,
}

/// The registry: every issued record, loaded from the JSON file.
///
/// The file is the only persistence the v1 static scheme needs; the *store* (§27.3) holds
/// the service state and is not the security directory.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct TokenRegistry {
    tokens: Vec<IssuedToken>,
}

impl TokenRegistry {
    /// Load from `<ATHENA_HOME>/var/api-tokens.json` (the bootstrap form the admin
    /// writes; absent file ⇒ an empty registry, and then every request is refused except
    /// `healthz`).
    ///
    /// # Errors
    /// `std::io` failures or a malformed registry; a startup failure rather than a
    /// surprise on the request path (ATH-SEC-006 carries the same spirit).
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let raw = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(err) => return Err(err.to_string()),
        };
        serde_json::from_str::<Self>(&raw).map_err(|error| error.to_string())
    }

    /// Persist the registry (the admin's form command at issuance time).
    ///
    /// # Errors
    /// `std::io` / serialization failures.
    pub fn save(&self, path: &std::path::Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|error| error.to_string())?;
        std::fs::write(path, text).map_err(|error| error.to_string())
    }

    /// Issue one token: hash the plaintext, keep only the hash (ATH-SEC-005), and stage
    /// it.
    ///
    /// Returns the *token value itself* so the caller (the admin side) can distribute it
    /// exactly once.
    pub fn issue(&mut self, name: &str, role: Role) -> String {
        let token = format!("ath-{}-{}", role.as_str(), hex_token_name(name));
        let record = IssuedToken {
            name: name.to_string(),
            role,
            sha256_hex: Self::hash(&token),
        };
        self.tokens.push(record);
        token
    }

    /// Which role the presented token carries.
    #[must_use]
    pub fn authenticate(&self, presented: &str) -> Option<Role> {
        let digest = Self::hash(presented);
        self.tokens
            .iter()
            .find(|record| record.sha256_hex == digest)
            .map(|record| record.role)
    }

    /// Every registered record (audit view).
    #[must_use]
    pub fn records(&self) -> &[IssuedToken] {
        &self.tokens
    }

    /// SHA-256 hex (the registry's own encoding).
    #[must_use]
    pub fn hash(token: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(token);
        let digest = hasher.finalize();
        let mut out = String::with_capacity(64);
        for byte in digest {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }

    /// Share the registry as a state online.
    #[must_use]
    pub fn shared(self) -> Arc<Self> {
        Arc::new(self)
    }
}

/// Derive a stable disorder-free token slug segment from the name audience (the token
/// stays the caller's secret, the slug is the audit aid).
fn hex_token_name(name: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(name);
    let digest = hasher.finalize();
    format!(
        "{:x}",
        u64::from_be_bytes(digest[..8].try_into().expect("8 bytes"))
    )
}

/// The verified caller identity.
#[derive(Debug, Clone)]
pub struct ApiCaller {
    /// Registered name.
    pub name: String,
    /// Role it carries.
    pub role: Role,
}

impl ApiCaller {
    /// Whether the caller satisfies the endpoint's role.
    #[must_use]
    pub const fn satisfies(&self, required: Role) -> bool {
        self.role.satisfies(required)
    }
}

/// The caller identity, resolved from a request's bearer header.
pub fn from_request(
    headers: &axum::http::HeaderMap,
    registry: &TokenRegistry,
    correlation_id: &str,
) -> Result<ApiCaller, crate::errors::ApiError> {
    let Some(token) = bearer_of(headers) else {
        return Err(crate::errors::ApiError::unauthenticated(correlation_id));
    };
    let Some(role) = registry.authenticate(token) else {
        return Err(crate::errors::ApiError::unknown_token(correlation_id));
    };
    Ok(ApiCaller {
        name: "bearer".into(),
        role,
    })
}

/// The bearer header of a request header map (spec §22.1's form).
pub fn bearer_header(headers: &axum::http::HeaderMap) -> Option<&str> {
    crate::routes::bearer_of(headers)
}
