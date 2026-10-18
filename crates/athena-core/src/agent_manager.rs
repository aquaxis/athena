//! Agent Manager — `AgentDefinition` registration and validation (spec §26.3).
//!
//! Validation is where ATH-AGENT-008 bites: `model.provider` and `model.name` are
//! mandatory at registration *and* start; ATHENA keeps no default. Owned entities:
//! `AgentDefinition` (no Events of its own).
//!
//! Owned entities (`OWNED`): AgentDefinition.

use crate::ownership::{Entity, ModuleRole};

/// The entities this module may mutate.
pub const OWNED: &[(Entity, ModuleRole)] = &[(Entity::AgentDefinition, ModuleRole::AgentManager)];
