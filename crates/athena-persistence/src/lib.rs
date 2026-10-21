//! The persistence crate (ATH-PERS).
//!
//! The store is authoritative; in-memory state is derived (`ATH-PERS-002`). The engine
//! is a pure-Rust embedded store ([`redb`]) — the instruction source prohibits SQLite
//! (最優先事項, 2026-10-18), and there is no SQL anywhere in the workspace
//! (`ATH-PERS-007`'s boundary made one-narrower).

pub mod db;
pub mod ddl;
pub mod error;
pub mod repository;
pub mod store;

#[cfg(test)]
mod conversation_store_tests;
#[cfg(test)]
mod debug_lease_tests;
#[cfg(test)]
mod store_tests_extracted;

pub use db::{Database, format_rfc3339, now_rfc3339};
pub use ddl::CURRENT_SCHEMA_VERSION;
pub use ddl::MIGRATIONS;
pub use ddl::Migration;
pub use error::{PersistenceError, Result};
pub use repository::*;
pub use store::{AthenaStore, digest_of};
