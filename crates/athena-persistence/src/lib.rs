//! Persistence (spec section 27).
//!
//! Core depends on the Repository traits here and never on SQL (ATH-PERS-007), so that a later
//! move to another store changes no caller. The persistent store is authoritative and
//! in-memory state is derived (ATH-PERS-002).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod conversation_store;
pub mod db;
pub mod ddl;
pub mod error;
pub mod hierarchy_runtime_store;
pub mod repository;
pub mod skill_store;
pub mod sqlite;

pub use db::{Database, format_rfc3339, now_rfc3339};
pub use ddl::{CURRENT_SCHEMA_VERSION, MIGRATIONS, Migration};
pub use error::{PersistenceError, Result};
pub use repository::*;
pub use sqlite::SqliteStore;
