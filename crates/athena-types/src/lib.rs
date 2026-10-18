//! Shared ATHENA types (spec section 26).
//!
//! This crate is the bottom of the dependency graph: every other crate may depend on it and it
//! depends on nothing in the workspace (spec section 32.2). It carries the vocabulary only —
//! identifiers, value domains, and entity shapes. Behaviour lives in the owning crate, so that
//! each entity has exactly one module permitted to mutate it (ATH-DATA-003).
//!
//! Type mapping follows spec section 26: `Option<T>` for absent values, `u64` for counts, sizes,
//! milliseconds and revisions, `u32` for pids, `i32` for exit codes, `f64` for ratios,
//! `serde_json::Value` for free-form data, and RFC 3339 UTC `String` for times (ATH-DATA-002).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod clock;
pub mod entities;
pub mod enums;
pub mod ids;
pub mod skill_state;

pub use clock::now_rfc3339;
pub use entities::*;
pub use enums::*;
pub use ids::*;
pub use skill_state::{SkillTransition, SkillTrigger, skill_transition};
