//! The Agent surface (spec §8): definitions, hierarchy composition, permission
//! containment, and capability matching — the *decision materials* Core Host validates;
//! Core Host alone enforces (R-SEC-02).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod agent_manager;
pub mod hierarchy;
pub mod persona;
pub mod scoring;
pub mod selection;

pub use agent_manager::{KnownUniverse, Refusal, validate_definition, validate_dynamic};
pub use hierarchy::{CompositionError, DefinitionFacts, SettledNode, compose, parent_level};
pub use persona::{
    LoadedPersona, PersonaFault, PersonaLedger, front_matter, load_persona_from_text,
};
pub use selection::{Candidate, NoMatch, Selection, select};

#[cfg(test)]
mod tests;
