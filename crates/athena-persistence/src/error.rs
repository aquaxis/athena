//! Persistence errors.
//!
//! These map onto the section 20.1 error classes: `Conflict` and `NotFound` surface as
//! `ValidationError` (409 and 404), while `Backend` and `SchemaTooNew` are `InternalError`.

use std::path::PathBuf;

/// A persistence failure.
#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    /// The underlying store (the engine) failed.
    #[error("database error: {detail}")]
    Backend {
        /// What failed.
        detail: String,
    },

    /// A surface-level fault that names itself (`UDS`-shaped wrappers and the like).
    #[error("{0}")]
    Plainly(String),

    /// A filesystem operation failed.
    #[error("cannot access {path}: {source}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// Underlying cause.
        source: std::io::Error,
    },

    /// The database was written by a newer build (ATH-PERS-010).
    #[error(
        "database schema version {found} is newer than this build supports ({supported}); \
         upgrade ATHENA rather than running against an unknown schema"
    )]
    SchemaTooNew {
        /// Version recorded in the database.
        found: u32,
        /// Version this build understands.
        supported: u32,
    },

    /// A uniqueness or state precondition failed. Surfaces as 409 (ATH-API-011).
    #[error("conflict: {detail}")]
    Conflict {
        /// What conflicted.
        detail: String,
    },

    /// The requested row does not exist.
    #[error("{entity} {id} not found")]
    NotFound {
        /// Entity kind.
        entity: &'static str,
        /// Identifier.
        id: String,
    },

    /// A stored value could not be decoded into its type.
    ///
    /// This is an internal inconsistency rather than bad input: the writer and reader of a
    /// column disagree.
    #[error("cannot decode {column} of {entity}: {detail}")]
    Decode {
        /// Entity kind.
        entity: &'static str,
        /// Column name.
        column: &'static str,
        /// What went wrong.
        detail: String,
    },
}

/// Persistence result.
pub type Result<T> = std::result::Result<T, PersistenceError>;
