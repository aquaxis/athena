//! Configuration errors.
//!
//! Every variant is fatal at startup: an invalid configuration aborts rather than being patched
//! up with defaults (ATH-CFG-003).

use std::path::PathBuf;

/// A configuration failure.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// `ATHENA_HOME` could not be resolved by any of the three routes (ATH-CFG-010).
    ///
    /// Carries the directory the upward search started from, so the message can tell the user
    /// where `athena init` should be run.
    #[error(
        "cannot locate .athena/: searched upward from {searched_from}. \
         run `athena init` in the project directory to create it"
    )]
    HomeUnresolved {
        /// Where the upward search began.
        searched_from: PathBuf,
    },

    /// A path that should name a directory does not.
    #[error("{what} is not a directory: {path}")]
    NotADirectory {
        /// What was expected.
        what: &'static str,
        /// The offending path.
        path: PathBuf,
    },

    /// The configuration file could not be read.
    #[error("cannot read configuration file {path}: {source}")]
    Unreadable {
        /// The file.
        path: PathBuf,
        /// Underlying cause.
        source: std::io::Error,
    },

    /// The configuration file is not valid YAML, or contains unknown or mistyped keys.
    ///
    /// Unknown keys are rejected rather than ignored, so a typo fails loudly instead of
    /// silently leaving a default in place.
    #[error("invalid configuration{}: {source}", match path { Some(p) => format!(" in {}", p.display()), None => String::new() })]
    Invalid {
        /// The file, when the error came from one.
        path: Option<PathBuf>,
        /// Underlying cause.
        source: serde_yaml::Error,
    },

    /// An environment variable or CLI override did not name a known key.
    #[error("override {origin} does not match any configuration key")]
    UnknownKey {
        /// The override as written.
        origin: String,
    },

    /// An override value was not of the key's type.
    #[error("override {origin} has an invalid value: {reason}")]
    BadOverride {
        /// The override as written.
        origin: String,
        /// Why it was rejected.
        reason: String,
    },

    /// An unsupported variable reference appeared in a value.
    ///
    /// Only `${athena.home}` exists; introducing another would make resolution order
    /// ambiguous (ATH-CFG-010).
    #[error("unsupported variable reference `${{{name}}}` at {at}: only ${{athena.home}} exists")]
    UnsupportedVariable {
        /// The referenced name.
        name: String,
        /// Where it appeared.
        at: String,
    },

    /// A cross-key constraint from spec section 28.2 was violated (ATH-CFG-008).
    #[error("configuration constraint violated: {constraint}")]
    ConstraintViolated {
        /// A description naming the constraint and the offending values.
        constraint: String,
    },

    /// A secret appears to be written into the configuration file (ATH-CFG-005).
    #[error(
        "configuration key {key} looks like a secret; \
         tokens and provider keys must come from the environment or a secret manager"
    )]
    SecretInConfig {
        /// The offending key path.
        key: String,
    },
}

/// Configuration result.
pub type Result<T> = std::result::Result<T, ConfigError>;
