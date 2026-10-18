//! The §26.2 ownership table, as code (ATH-DATA-003).
//!
//! Mutation rights are confined to the owning module. This table is the single home for
//! the mapping; each Core Host module re-declares its slice (`OWNED` in each manager) and
//! the tests refuse any drift between the two — an ownership change surfaces as a test
//! failure, not as documentation lag.
//!
//! Two owners sit outside `athena-core`: the Skill Manager, which belongs to the Runtime
//! abstraction (`athena-skills`, ATH-SKILL-012), and conversation persistence, which Core
//! Host exercises directly (spec §26.2, row `Conversation / Utterance → Core Host`).
//! Both rows are represented here so the table is complete.

use serde::{Deserialize, Serialize};

/// The entities whose mutation rights the §26.2 table distributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Entity {
    /// The Task aggregate.
    Task,
    /// The Plan revision aggregate.
    Plan,
    /// One unit of action inside a Plan.
    PlanStep,
    /// A static agent definition.
    AgentDefinition,
    /// A running agent individual.
    AgentInstance,
    /// Inter-agent traffic and its delivery records.
    Message,
    /// A work product.
    Result,
    /// A deliverable attached to a Result.
    Artifact,
    /// The acceptance record for a Result.
    Evaluation,
    /// A skill version (state per `(id, version)`).
    Skill,
    /// The composed agent hierarchy.
    Hierarchy,
    /// The Runtime-side execution unit.
    RuntimeInstance,
    /// A tool invocation record.
    ToolExecution,
    /// The pre-Task conversation.
    Conversation,
    /// One turn of a conversation.
    Utterance,
    /// A human approval.
    Approval,
    /// A memory record across the three tiers.
    MemoryRecord,
}

impl Entity {
    /// The Event prefixes the §26.2 table grants the owner for this entity.
    #[must_use]
    pub const fn event_prefixes(self) -> &'static [&'static str] {
        match self {
            Self::Task => &["Task"],
            Self::Plan | Self::PlanStep => &["Plan"],
            Self::AgentDefinition => &[],
            Self::AgentInstance => &["Agent"],
            Self::Message => &["AgentMessage"],
            Self::Result => &["TaskResultReceived"],
            Self::Artifact => &[],
            Self::Evaluation => &["Evaluation"],
            Self::Skill => &["Skill"],
            Self::Hierarchy => &["Hierarchy"],
            Self::RuntimeInstance => &["RuntimeInstance"],
            Self::ToolExecution => &["ToolExecution"],
            Self::Conversation | Self::Utterance => &["Clarification"],
            Self::Approval => &["Approval"],
            Self::MemoryRecord => &["MemoryRecord"],
        }
    }
}

/// The ownership roles. `SkillManager` is implemented by `athena-skills` (Runtime
/// abstraction), and `CoreHost` covers the module-free remainder (Conversation / Utterance).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleRole {
    /// Registers and validates `AgentDefinition`.
    AgentManager,
    /// Approval state machine and canonical descriptors.
    ApprovalManager,
    /// Message persistence, routing, dedup and delivery semantics.
    Broker,
    /// Core Agent instance #0 lifecycle, restart window, context rebuild.
    CoreAgentSession,
    /// `events` append and SSE fan-out; the emitter for everything else.
    EventManager,
    /// Composed hierarchies (Core Agent's judgement, recorded).
    HierarchyManager,
    /// Memory records across the three tiers.
    MemoryManager,
    /// Risk classification and permission checks.
    PolicyManager,
    /// Results and their evaluations.
    ResultManager,
    /// Runtime instances and their Runtime Instances.
    RuntimeManager,
    /// The PlanStep queue, limits, backoff.
    Scheduler,
    /// Task / Plan / PlanStep transitions (Core Agent's direction).
    StateManager,
    /// Tool execution records and grants.
    ToolManager,
    /// Provided by `athena-skills`, outside Core Host (ATH-SKILL-012).
    SkillManager,
    /// Conversation / Utterance, held directly by Core Host.
    CoreHost,
}

/// Every other module contains one entry per module role, and the table below is the
/// top-level pointer list.
pub const EVERY_MODULE: [ModuleRole; 15] = [
    ModuleRole::AgentManager,
    ModuleRole::ApprovalManager,
    ModuleRole::Broker,
    ModuleRole::CoreAgentSession,
    ModuleRole::EventManager,
    ModuleRole::HierarchyManager,
    ModuleRole::MemoryManager,
    ModuleRole::PolicyManager,
    ModuleRole::ResultManager,
    ModuleRole::RuntimeManager,
    ModuleRole::Scheduler,
    ModuleRole::StateManager,
    ModuleRole::ToolManager,
    ModuleRole::SkillManager,
    ModuleRole::CoreHost,
];

/// Every §26.2 row: entity, unique mutation owner.
///
/// `Artifact` has no dedicated row in the §26.2 table — it travels with its owning Result
/// (ATH-DATA-012), so its owner is the Result Manager.
pub const OWNERSHIP: &[(Entity, ModuleRole)] = &[
    (Entity::Task, ModuleRole::StateManager),
    (Entity::Plan, ModuleRole::StateManager),
    (Entity::PlanStep, ModuleRole::StateManager),
    (Entity::AgentDefinition, ModuleRole::AgentManager),
    (Entity::AgentInstance, ModuleRole::RuntimeManager),
    (Entity::Message, ModuleRole::Broker),
    (Entity::Result, ModuleRole::ResultManager),
    (Entity::Artifact, ModuleRole::ResultManager),
    (Entity::Skill, ModuleRole::SkillManager),
    (Entity::Hierarchy, ModuleRole::HierarchyManager),
    (Entity::RuntimeInstance, ModuleRole::RuntimeManager),
    (Entity::ToolExecution, ModuleRole::ToolManager),
    (Entity::Evaluation, ModuleRole::ResultManager),
    (Entity::Conversation, ModuleRole::CoreHost),
    (Entity::Utterance, ModuleRole::CoreHost),
    (Entity::Approval, ModuleRole::ApprovalManager),
    (Entity::MemoryRecord, ModuleRole::MemoryManager),
];

impl Entity {
    /// Every entity in the enumeration order above.
    pub const fn list_all() -> [Entity; 17] {
        [
            Entity::Task,
            Entity::Plan,
            Entity::PlanStep,
            Entity::AgentDefinition,
            Entity::AgentInstance,
            Entity::Message,
            Entity::Result,
            Entity::Artifact,
            Entity::Skill,
            Entity::Hierarchy,
            Entity::RuntimeInstance,
            Entity::ToolExecution,
            Entity::Evaluation,
            Entity::Conversation,
            Entity::Utterance,
            Entity::Approval,
            Entity::MemoryRecord,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every entity in the crate appears exactly once, with its unique owner
    /// (ATH-DATA-003).
    #[test]
    fn ownership_is_exhaustive_and_functional() {
        let mut entities: Vec<Entity> = OWNERSHIP.iter().map(|(e, _)| *e).collect();
        entities.sort();
        let unique = entities.clone();
        entities.dedup();
        assert_eq!(entities, unique, "an entity must have exactly one owner");

        for entity in Entity::list_all() {
            assert!(
                OWNERSHIP.iter().any(|(e, _)| e == &entity),
                "{entity:?} has no owner in the table"
            );
        }
    }

    #[test]
    fn the_event_prefixes_follow_the_specification() {
        for (entity, _) in OWNERSHIP {
            for prefix in entity.event_prefixes() {
                assert!(!prefix.is_empty(), "{entity:?} declares an empty prefix");
            }
        }
        // Spot-checks against the §26.2 table.
        assert_eq!(Entity::Message.event_prefixes(), &["AgentMessage"]);
        assert_eq!(Entity::Result.event_prefixes(), &["TaskResultReceived"]);
        assert_eq!(Entity::Conversation.event_prefixes(), &["Clarification"]);
        assert_eq!(Entity::AgentDefinition.event_prefixes(), &[] as &[&str]);
    }
}
