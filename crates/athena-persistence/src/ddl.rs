//! The data model's own version record and forward-migration runner (R-PRS-06).
//!
//! The store's logical data model lives in [`crate::tables`] (the key/row shapes are
//! engine statements, not SQL); this module carries what survives every engine: the
//! version record the startup verifies and the migration list a newer build walks.

/// The schema version this build understands.
pub const CURRENT_SCHEMA_VERSION: u32 = 2;

/// One forward-migration step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    /// The version this step moves the schema FROM.
    pub from: u32,
    /// The version this step moves the schema TO.
    pub to: u32,
    /// The step's own description, for the migration record.
    pub description: &'static str,
}

/// The forward migrations, in order.
///
/// v1 was the SQLite-engine-first layout; v2 is the engine-agnostic layout the
/// SQLite prohibition (instruction source, 2026-10-18) mandates: the same logical
/// tables (`crate::tables`), no SQL, no SQLite.
pub const MIGRATIONS: &[Migration] = &[Migration {
    from: 1,
    to: 2,
    description: "engine-agnostic store: the same §27.3 logical data model over a pure-Rust embedded engine; no SQL, no SQLite",
}];
