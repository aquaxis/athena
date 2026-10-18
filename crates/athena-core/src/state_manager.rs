//! State Manager — Task / Plan / PlanStep transitions (spec §26.2, ATH-STATE-001).
//!
//! The only writer of Task / Plan / PlanStep state; agents, the Core Agent Process and the
//! Runtime may *request* transitions but never perform one. Each resolved transition is
//! applied together with its Event in a single transaction (ATH-PERS-003, D-1), which is
//! why cross-module effects route through here and the Event Manager rather than calling
//! sibling modules.
//!
//! Owned entities (`OWNED`): Task, Plan, PlanStep. Emitted Events: `Task*`, `Plan*`.

use crate::ownership::{Entity, ModuleRole};

/// The entities this module applies transitions to.
pub const OWNED: &[(Entity, ModuleRole)] = &[
    (Entity::Task, ModuleRole::StateManager),
    (Entity::Plan, ModuleRole::StateManager),
    (Entity::PlanStep, ModuleRole::StateManager),
];
