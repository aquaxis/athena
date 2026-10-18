//! Identifiers (spec section 26.1).
//!
//! Every externally visible identifier is a prefixed string; a numeric sequence must never be
//! exposed as one (ATH-DATA-001). Identifiers are issued by Core Host alone, never by an agent
//! (ATH-CORE-041), which is why the constructors here live behind `new_*` functions that the
//! Core Host modules call rather than being derivable from agent input.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Declare a prefixed-ULID identifier newtype.
///
/// A newtype per identifier kind keeps `TaskId` and `PlanId` from being interchangeable at
/// call sites, which matters because the control protocol passes several of them together
/// (spec section 7.5.5).
macro_rules! prefixed_id {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
            schemars::JsonSchema,
        )]
        #[serde(transparent)]
        #[schemars(transparent)]
        pub struct $name(String);

        impl $name {
            /// The prefix every value of this kind carries.
            pub const PREFIX: &'static str = $prefix;

            /// Mint a fresh identifier. Core Host only (ATH-CORE-041).
            #[must_use]
            pub fn new() -> Self {
                Self(format!("{}{}", $prefix, ulid::Ulid::new()))
            }

            /// Rehydrate an identifier from storage or a request payload.
            ///
            /// Returns `None` when the prefix is absent, so an agent-supplied string in the
            /// wrong shape cannot silently become a valid identifier.
            #[must_use]
            pub fn parse(value: impl Into<String>) -> Option<Self> {
                let value = value.into();
                value.starts_with($prefix).then_some(Self(value))
            }

            /// Borrow the underlying string.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

prefixed_id!(
    /// Task identifier.
    TaskId,
    "task-"
);
prefixed_id!(
    /// Plan identifier.
    PlanId,
    "plan-"
);
prefixed_id!(
    /// PlanStep identifier.
    PlanStepId,
    "step-"
);
prefixed_id!(
    /// Agent Instance identifier.
    InstanceId,
    "ai-"
);
prefixed_id!(
    /// Result identifier.
    ResultId,
    "res-"
);
prefixed_id!(
    /// Event identifier.
    EventId,
    "evt-"
);
prefixed_id!(
    /// Message identifier.
    MessageId,
    "msg-"
);
prefixed_id!(
    /// Approval identifier.
    ApprovalId,
    "apr-"
);
prefixed_id!(
    /// Memory record identifier.
    MemoryId,
    "mem-"
);
prefixed_id!(
    /// Clarification identifier.
    ClarificationId,
    "cla-"
);
prefixed_id!(
    /// Hierarchy identifier.
    HierarchyId,
    "hier-"
);
prefixed_id!(
    /// Runtime Instance identifier.
    RuntimeInstanceId,
    "ri-"
);
prefixed_id!(
    /// Tool execution identifier.
    ToolExecutionId,
    "tx-"
);
prefixed_id!(
    /// Evaluation identifier.
    EvaluationId,
    "ev-"
);
prefixed_id!(
    /// Conversation identifier.
    ConversationId,
    "conv-"
);
prefixed_id!(
    /// Utterance identifier.
    UtteranceId,
    "utt-"
);
prefixed_id!(
    /// Correlation identifier, one per Task (spec section 30.4).
    CorrelationId,
    "cor-"
);

/// Agent identifier: a persona-derived slug, not a ULID (spec section 26.1).
///
/// The Core Agent's value is fixed to `athena-core-agent` by ATH-MSG-011.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(transparent)]
#[schemars(transparent)]
pub struct AgentId(String);

impl AgentId {
    /// The Core Agent's reserved identifier (ATH-MSG-011).
    pub const CORE_AGENT: &'static str = "athena-core-agent";

    /// Wrap a slug.
    #[must_use]
    pub fn new(slug: impl Into<String>) -> Self {
        Self(slug.into())
    }

    /// The Core Agent identifier.
    #[must_use]
    pub fn core_agent() -> Self {
        Self(Self::CORE_AGENT.to_string())
    }

    /// Whether this is the Core Agent, which several rules treat specially
    /// (ATH-MSG-010, ATH-STATE-012, ATH-STATE-017).
    #[must_use]
    pub fn is_core_agent(&self) -> bool {
        self.0 == Self::CORE_AGENT
    }

    /// Borrow the underlying slug.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Skill identifier: a slug (spec section 26.1).
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(transparent)]
#[schemars(transparent)]
pub struct SkillId(String);

impl SkillId {
    /// Wrap a slug.
    #[must_use]
    pub fn new(slug: impl Into<String>) -> Self {
        Self(slug.into())
    }

    /// Borrow the underlying slug.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SkillId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Project identifier: the SHA-256 of the normalized absolute Workspace path, first 16 hex
/// digits (spec section 26.1).
///
/// Derivation must be deterministic (ATH-DATA-011) because Long Term Memory is scoped by it;
/// a non-deterministic derivation would silently partition a project's memory.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProjectId(String);

impl ProjectId {
    /// Number of hex digits retained from the digest.
    const DIGITS: usize = 16;

    /// Derive from a normalized absolute workspace path.
    #[must_use]
    pub fn derive(normalized_absolute_workspace: &str) -> Self {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(normalized_absolute_workspace.as_bytes());
        let hex = digest.iter().fold(String::new(), |mut acc, byte| {
            use std::fmt::Write as _;
            let _ = write!(acc, "{byte:02x}");
            acc
        });
        Self(hex[..Self::DIGITS].to_string())
    }

    /// Borrow the underlying value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_ids_carry_their_prefix() {
        assert!(TaskId::new().as_str().starts_with("task-"));
        assert!(PlanId::new().as_str().starts_with("plan-"));
        assert!(PlanStepId::new().as_str().starts_with("step-"));
        assert!(InstanceId::new().as_str().starts_with("ai-"));
        assert!(ResultId::new().as_str().starts_with("res-"));
        assert!(EventId::new().as_str().starts_with("evt-"));
        assert!(MessageId::new().as_str().starts_with("msg-"));
        assert!(ApprovalId::new().as_str().starts_with("apr-"));
        assert!(MemoryId::new().as_str().starts_with("mem-"));
        assert!(HierarchyId::new().as_str().starts_with("hier-"));
        assert!(RuntimeInstanceId::new().as_str().starts_with("ri-"));
        assert!(ToolExecutionId::new().as_str().starts_with("tx-"));
        assert!(EvaluationId::new().as_str().starts_with("ev-"));
        assert!(ConversationId::new().as_str().starts_with("conv-"));
        assert!(UtteranceId::new().as_str().starts_with("utt-"));
        assert!(CorrelationId::new().as_str().starts_with("cor-"));
    }

    #[test]
    fn minted_ids_are_unique() {
        let a = TaskId::new();
        let b = TaskId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn parse_rejects_a_wrong_or_missing_prefix() {
        // ATH-CORE-041: an agent-supplied identifier in the wrong shape must not be accepted.
        assert!(TaskId::parse("task-01JA2B3C").is_some());
        assert!(TaskId::parse("plan-01JA2B3C").is_none());
        assert!(TaskId::parse("01JA2B3C").is_none());
        assert!(TaskId::parse("").is_none());
    }

    #[test]
    fn core_agent_id_is_the_reserved_slug() {
        // ATH-MSG-011 fixes this value.
        assert_eq!(AgentId::core_agent().as_str(), "athena-core-agent");
        assert!(AgentId::core_agent().is_core_agent());
        assert!(!AgentId::new("timing-lead").is_core_agent());
    }

    #[test]
    fn project_id_is_deterministic_and_path_sensitive() {
        // ATH-DATA-011: deterministic derivation; Long Term Memory scoping depends on it.
        let a = ProjectId::derive("/work/fpga-project");
        let b = ProjectId::derive("/work/fpga-project");
        let c = ProjectId::derive("/work/other-project");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.as_str().len(), 16);
        assert!(a.as_str().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn ids_round_trip_through_json_as_plain_strings() {
        let id = TaskId::new();
        let json = serde_json::to_string(&id).expect("serialize");
        assert!(json.starts_with("\"task-"), "serialized as {json}");
        let back: TaskId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(id, back);
    }
}
