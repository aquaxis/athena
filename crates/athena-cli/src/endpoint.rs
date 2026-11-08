//! Where the Core API listens, and how the CLI authenticates to it (X-27).
//!
//! The CLI reaches Core **only** through the Core API (`ATH-CLI-001`, `ATH-POLICY-007`),
//! so the one thing it must know is the address that API bound. That address is the
//! project's own configuration — `<ATHENA_HOME>/config.toml`'s `athena.api.host` / `.port`
//! and the authentication mode. The CLI reads the file directly: the dependency graph
//! does not permit an `athena-cli -> athena-config` edge (spec §32.2), the same reason
//! [`crate::resolve`] carries its own copy of the `ATHENA_HOME` rule.
//!
//! **The token is never read from the registry.** The registry stores hashes only
//! (`ATH-SEC-005`), so the plaintext cannot be recovered from it — the holder supplies it
//! through `ATHENA_TOKEN` or `--token`. Only `auth.mode = "none"` (loopback-only,
//! `ATH-CFG-006`) needs no token at all.

use std::path::Path;

/// The configuration file's name inside `ATHENA_HOME` (`R-CFG-08`).
pub const CONFIG_FILE: &str = "config.toml";

/// The environment variable that carries the bearer token.
///
/// The secret rides the environment, never the configuration file (`ATH-CFG-005`) and
/// never the registry, which holds hashes.
pub const TOKEN_ENV: &str = "ATHENA_TOKEN";

/// How the CLI authenticates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Auth {
    /// No credential is presented: the API runs with `auth.mode = "none"`, permitted only
    /// on loopback (`ATH-CFG-006`).
    None,
    /// A bearer token is presented (`ATH-SEC-004`).
    Bearer(String),
}

/// The resolved Core API endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// Base URL, `http://<host>:<port>`.
    pub base: String,
    /// How to authenticate.
    pub auth: Auth,
}

impl Endpoint {
    /// The absolute URL of a request path (the path always begins `/`).
    #[must_use]
    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }
}

/// Why the endpoint could not be resolved.
#[derive(Debug, thiserror::Error)]
pub enum EndpointFault {
    /// `config.toml` is unreadable or not TOML (`ATH-CFG-012`: no silent fall-back).
    #[error("reading {path}: {detail}")]
    Config {
        /// The file involved.
        path: String,
        /// What went wrong.
        detail: String,
    },
    /// The configuration does not state where the API listens.
    #[error("`athena.api.host`/`athena.api.port` missing from {0}")]
    NoAddress(String),
    /// `auth.mode = "token"` but no token was supplied.
    #[error(
        "this project authenticates with bearer tokens (auth.mode = \"token\"); provide one \
         with ${TOKEN_ENV} or --token (the registry stores hashes only, ATH-SEC-005)"
    )]
    TokenRequired,
}

/// Read the API address from the project's configuration.
///
/// `token_override` (the `--token` flag) wins over `ATHENA_TOKEN`; both are ignored when
/// the configuration says `auth.mode = "none"`.
///
/// # Errors
/// [`EndpointFault::Config`] when the file cannot be read or parsed as TOML,
/// [`EndpointFault::NoAddress`] when the address keys are absent, and
/// [`EndpointFault::TokenRequired`] when token auth is configured but no token is given.
pub fn resolve(home: &Path, token_override: Option<&str>) -> Result<Endpoint, EndpointFault> {
    let path = home.join(CONFIG_FILE);
    let raw = std::fs::read_to_string(&path).map_err(|error| EndpointFault::Config {
        path: path.display().to_string(),
        detail: error.to_string(),
    })?;
    let value: toml::Value = toml::from_str(&raw).map_err(|error| EndpointFault::Config {
        path: path.display().to_string(),
        detail: format!("not TOML: {error} (ATH-CFG-012)"),
    })?;

    let api = value.get("athena").and_then(|athena| athena.get("api"));
    let host = api
        .and_then(|api| api.get("host"))
        .and_then(toml::Value::as_str)
        .ok_or_else(|| EndpointFault::NoAddress(path.display().to_string()))?;
    let port = api
        .and_then(|api| api.get("port"))
        .and_then(toml::Value::as_integer)
        .ok_or_else(|| EndpointFault::NoAddress(path.display().to_string()))?;
    let mode = api
        .and_then(|api| api.get("auth"))
        .and_then(|auth| auth.get("mode"))
        .and_then(toml::Value::as_str)
        .unwrap_or("token");

    let auth = if mode == "none" {
        Auth::None
    } else {
        let token = token_override
            .map(str::to_string)
            .or_else(|| std::env::var(TOKEN_ENV).ok())
            .filter(|token| !token.trim().is_empty())
            .ok_or(EndpointFault::TokenRequired)?;
        Auth::Bearer(token)
    };

    Ok(Endpoint {
        base: format!("http://{host}:{port}"),
        auth,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(config: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp");
        let home = dir.path().join(".athena");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::write(home.join(CONFIG_FILE), config).expect("config");
        dir
    }

    const NONE_MODE: &str = r#"
[athena.api]
host = "127.0.0.1"
port = 8080

[athena.api.auth]
mode = "none"
"#;

    const TOKEN_MODE: &str = r#"
[athena.api]
host = "127.0.0.1"
port = 9099

[athena.api.auth]
mode = "token"
"#;

    #[test]
    fn the_none_mode_resolves_without_a_token() {
        let dir = project(NONE_MODE);
        let endpoint =
            resolve(&dir.path().join(".athena"), None).expect("none mode needs no token");
        assert_eq!(endpoint.base, "http://127.0.0.1:8080");
        assert_eq!(endpoint.auth, Auth::None);
        assert_eq!(
            endpoint.url("/api/v1/healthz"),
            "http://127.0.0.1:8080/api/v1/healthz"
        );
    }

    #[test]
    fn the_token_mode_carries_the_override_and_the_configured_port() {
        let dir = project(TOKEN_MODE);
        let endpoint = resolve(&dir.path().join(".athena"), Some("ath-admin-x"))
            .expect("the override is the credential");
        assert_eq!(endpoint.base, "http://127.0.0.1:9099");
        assert_eq!(endpoint.auth, Auth::Bearer("ath-admin-x".into()));
    }

    #[test]
    fn the_token_mode_without_a_token_refuses() {
        let dir = project(TOKEN_MODE);
        // Ensure the environment cannot satisfy this row.
        unsafe { std::env::remove_var(TOKEN_ENV) };
        let fault = resolve(&dir.path().join(".athena"), None).expect_err("no credential");
        assert!(matches!(fault, EndpointFault::TokenRequired), "{fault}");
        assert!(fault.to_string().contains(TOKEN_ENV), "{fault}");
    }

    #[test]
    fn a_yaml_file_at_the_toml_path_is_a_parse_fault_not_a_fallback() {
        let dir = project("[athena.api]\nhost: 127.0.0.1\nport: 8080\n");
        let fault = resolve(&dir.path().join(".athena"), None).expect_err("not TOML");
        assert!(matches!(fault, EndpointFault::Config { .. }), "{fault}");
    }

    #[test]
    fn a_config_without_the_address_refuses() {
        let dir = project("[athena.api.auth]\nmode = \"none\"\n");
        let fault = resolve(&dir.path().join(".athena"), None).expect_err("no address");
        assert!(matches!(fault, EndpointFault::NoAddress(_)), "{fault}");
    }
}
