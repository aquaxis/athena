//! Token issuance (X-28; `ATH-SEC-004`/`ATH-SEC-005`, `R-SEC-03`/`R-SEC-04`).
//!
//! The API authenticates a `Authorization: Bearer <token>` request against
//! `<ATHENA_HOME>/var/api-tokens.json`, which holds **hashes only** (`ATH-SEC-005`). The
//! registry type lives in `athena-api`, but the CLI may not depend on it (spec §32.2: the
//! UI reaches Core only through the Core API), so this module reads and writes the same
//! file format directly — the same reason [`crate::resolve`] carries its own copy of the
//! `ATHENA_HOME` rule.
//!
//! The wire form is fixed by the API's registry, so a token issued here authenticates
//! there: `{"tokens":[{"name":…,"role":"viewer"|"operator"|"admin","sha256_hex":…}]}`.
//! The plaintext is printed **once**, at issuance, and never stored (`ATH-SEC-005`).

use std::path::{Path, PathBuf};

/// The registry file, under `<ATHENA_HOME>/var/` (`R-OPS-02`).
pub const REGISTRY_FILE: &str = "api-tokens.json";

/// The three roles, in the API's wire spelling (spec §29.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Read-only.
    Viewer,
    /// `viewer` + Task create / run / cancel, approval answer.
    Operator,
    /// `operator` + Skill management, config inspection, forced stop.
    Admin,
}

impl Role {
    /// The wire form the registry stores.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Operator => "operator",
            Self::Admin => "admin",
        }
    }

    /// Parse the wire form.
    ///
    /// # Errors
    /// [`TokenFault::UnknownRole`] for anything outside the three-role set — issuing a
    /// token with a misspelled role would authenticate nothing, so it is refused at the
    /// source rather than discovered at request time.
    pub fn parse(text: &str) -> Result<Self, TokenFault> {
        match text {
            "viewer" => Ok(Self::Viewer),
            "operator" => Ok(Self::Operator),
            "admin" => Ok(Self::Admin),
            other => Err(TokenFault::UnknownRole(other.to_string())),
        }
    }
}

/// Why a token operation failed.
#[derive(Debug, thiserror::Error)]
pub enum TokenFault {
    /// A missing or blank option (`--name`, `--role`).
    #[error("{0}")]
    MissingOption(String),
    /// The role is not one of the three (spec §29.2).
    #[error("unknown role {0:?}; expected viewer, operator, or admin")]
    UnknownRole(String),
    /// A record with this name already exists.
    #[error(
        "a token named {0:?} already exists; names are the audit key, so a second issue would make the trail ambiguous"
    )]
    DuplicateName(String),
    /// `.athena/` could not be resolved (`ATH-CLI-010`).
    #[error("`.athena/` not found; run `athena init` first")]
    NoHome,
    /// The registry file is unreadable or malformed.
    #[error("token registry at {path}: {detail}")]
    Registry {
        /// The file involved.
        path: String,
        /// What went wrong.
        detail: String,
    },
}

/// One issued token in registry form (`ATH-SEC-005`: hash only).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IssuedToken {
    /// The audit-facing name.
    pub name: String,
    /// The single role granted (`ATH-SEC-004`).
    pub role: String,
    /// The SHA-256 hex of the token.
    pub sha256_hex: String,
}

/// The registry file's shape (identical to the API's, so one file serves both).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Registry {
    #[serde(default)]
    tokens: Vec<IssuedToken>,
}

impl Registry {
    /// Read the registry, treating an absent file as empty (the bootstrap state:
    /// every request but `healthz` is refused until a token exists).
    ///
    /// # Errors
    /// [`TokenFault::Registry`] when the file exists but cannot be read or parsed.
    pub fn load(path: &Path) -> Result<Self, TokenFault> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(|error| TokenFault::Registry {
                path: path.display().to_string(),
                detail: error.to_string(),
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(TokenFault::Registry {
                path: path.display().to_string(),
                detail: error.to_string(),
            }),
        }
    }

    /// Publish the registry (parents created; the file under `var/`).
    ///
    /// # Errors
    /// [`TokenFault::Registry`] on any write fault.
    pub fn save(&self, path: &Path) -> Result<(), TokenFault> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| TokenFault::Registry {
                path: parent.display().to_string(),
                detail: error.to_string(),
            })?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|error| TokenFault::Registry {
            path: path.display().to_string(),
            detail: error.to_string(),
        })?;
        std::fs::write(path, text + "\n").map_err(|error| TokenFault::Registry {
            path: path.display().to_string(),
            detail: error.to_string(),
        })
    }

    /// Every record, for the list view.
    #[must_use]
    pub fn records(&self) -> &[IssuedToken] {
        &self.tokens
    }

    /// Whether a name is already taken (names are the audit key).
    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        self.tokens.iter().any(|record| record.name == name)
    }

    /// Append a record.
    pub fn push(&mut self, record: IssuedToken) {
        self.tokens.push(record);
    }
}

/// A freshly issued token: the plaintext (shown once) and the record persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issued {
    /// The plaintext bearer token — printed once, never stored (`ATH-SEC-005`).
    pub plaintext: String,
    /// The hashed record that goes into the registry.
    pub record: IssuedToken,
}

/// Issue a token: mint the plaintext, keep only its hash.
///
/// The token's spelling is the API registry's own (`ath-<role>-<slug>`), so the value
/// printed here is exactly what `Authorization: Bearer` accepts there.
#[must_use]
pub fn issue(name: &str, role: Role) -> Issued {
    let plaintext = format!("ath-{}-{}", role.as_str(), token_slug(name));
    let record = IssuedToken {
        name: name.to_string(),
        role: role.as_str().to_string(),
        sha256_hex: sha256_hex(&plaintext),
    };
    Issued { plaintext, record }
}

/// The registry path under a resolved home.
#[must_use]
pub fn registry_path(home: &Path) -> PathBuf {
    home.join("var").join(REGISTRY_FILE)
}

/// The stable slug segment of the token (the audit aid; the token stays the caller's
/// secret). Identical to the API's derivation so both sides mint one spelling.
fn token_slug(name: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(name.as_bytes());
    let digest = hasher.finalize();
    format!(
        "{:x}",
        u64::from_be_bytes(digest[..8].try_into().expect("8 bytes"))
    )
}

/// Lower-case hex SHA-256 (the registry's encoding).
#[must_use]
pub fn sha256_hex(token: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// `athena token issue --name <name> --role <role>`.
///
/// # Errors
/// [`TokenFault`] for a missing option, an unknown role, a duplicate name, or a registry
/// fault.
pub fn run_issue(
    home: &Path,
    name: Option<&str>,
    role: Option<&str>,
) -> Result<Issued, TokenFault> {
    let name = name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            TokenFault::MissingOption(
                "usage: athena token issue --name <name> --role <viewer|operator|admin>".into(),
            )
        })?;
    let role = role.ok_or_else(|| {
        TokenFault::MissingOption(
            "usage: athena token issue --name <name> --role <viewer|operator|admin>".into(),
        )
    })?;
    let role = Role::parse(role)?;

    let path = registry_path(home);
    let mut registry = Registry::load(&path)?;
    if registry.has(name) {
        return Err(TokenFault::DuplicateName(name.to_string()));
    }
    let issued = issue(name, role);
    registry.push(issued.record.clone());
    registry.save(&path)?;
    Ok(issued)
}

/// `athena token list` — the issued records (names, roles, hashes; never plaintext).
///
/// # Errors
/// [`TokenFault::Registry`] when the file cannot be read.
pub fn run_list(home: &Path) -> Result<Vec<IssuedToken>, TokenFault> {
    Ok(Registry::load(&registry_path(home))?.records().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp");
        std::fs::create_dir_all(dir.path().join(".athena/var")).expect("var");
        dir
    }

    #[test]
    fn an_issued_token_is_stored_as_a_hash_only() {
        let dir = temp_home();
        let home = dir.path().join(".athena");
        let issued = run_issue(&home, Some("admin-1"), Some("admin")).expect("issued");
        // The plaintext carries the role, exactly the API's spelling.
        assert!(
            issued.plaintext.starts_with("ath-admin-"),
            "{}",
            issued.plaintext
        );
        // The persisted record is the hash, never the plaintext (ATH-SEC-005).
        let raw = std::fs::read_to_string(registry_path(&home)).expect("registry");
        assert!(!raw.contains(&issued.plaintext), "plaintext never stored");
        assert!(raw.contains(&issued.record.sha256_hex), "hash stored");
        assert_eq!(issued.record.sha256_hex, sha256_hex(&issued.plaintext));
    }

    #[test]
    fn the_registry_shape_matches_the_apis_reader() {
        let dir = temp_home();
        let home = dir.path().join(".athena");
        run_issue(&home, Some("viewer-1"), Some("viewer")).expect("issued");
        let raw = std::fs::read_to_string(registry_path(&home)).expect("registry");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("json");
        let record = &value["tokens"][0];
        assert_eq!(record["name"], "viewer-1");
        assert_eq!(record["role"], "viewer");
        assert_eq!(
            record["sha256_hex"].as_str().map(str::len),
            Some(64),
            "a SHA-256 hex"
        );
    }

    #[test]
    fn a_duplicate_name_is_refused() {
        let dir = temp_home();
        let home = dir.path().join(".athena");
        run_issue(&home, Some("dup"), Some("viewer")).expect("first");
        let fault = run_issue(&home, Some("dup"), Some("operator")).expect_err("duplicate");
        assert!(matches!(fault, TokenFault::DuplicateName(_)), "{fault}");
    }

    #[test]
    fn an_unknown_role_is_refused_before_anything_is_written() {
        let dir = temp_home();
        let home = dir.path().join(".athena");
        let fault = run_issue(&home, Some("x"), Some("superuser")).expect_err("bad role");
        assert!(matches!(fault, TokenFault::UnknownRole(_)), "{fault}");
        assert!(
            !registry_path(&home).exists(),
            "a refused issue writes nothing"
        );
    }

    #[test]
    fn the_list_view_carries_no_plaintext() {
        let dir = temp_home();
        let home = dir.path().join(".athena");
        let issued = run_issue(&home, Some("op-1"), Some("operator")).expect("issued");
        let records = run_list(&home).expect("list");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "op-1");
        assert_eq!(records[0].role, "operator");
        assert!(
            !records.iter().any(|r| r.sha256_hex == issued.plaintext),
            "the list never carries the plaintext"
        );
    }

    #[test]
    fn an_absent_registry_is_empty_not_an_error() {
        let dir = temp_home();
        let home = dir.path().join(".athena");
        assert!(run_list(&home).expect("empty").is_empty());
    }
}
