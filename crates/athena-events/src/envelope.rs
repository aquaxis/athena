//! The Event envelope (spec section 23.1).
//!
//! The envelope is fixed: `event_id`, `schema_version`, `type`, `occurred_at`, `sequence`,
//! the optional aggregate references, `correlation_id`, `causation_id`, `actor` and the
//! payload (ATH-EVENT-001). `schema_version` exists so a payload-structure change can be
//! recorded (ATH-EVENT-006); the value this revision of the specification describes is
//! [`SCHEMA_VERSION`]. Envelopes are immutable once built — a correction is a new Event,
//! never an edit (ATH-EVENT-002).

use athena_types::{AgentId, InstanceId, PlanStepId, SkillId, TaskId};
use serde::{Deserialize, Serialize};

/// Payload schema version for this revision of the specification (ATH-EVENT-006).
pub const SCHEMA_VERSION: u64 = 1;

/// Sequence carried by a draft (assigned by the store on append, ATH-EVENT-003).
///
/// `athena-persistence` reads `MAX(sequence) + 1` inside the same transaction that writes
/// the row, so concurrent appends cannot share a position (ATH-EVENT-003); a caller
/// therefore has no authority to choose the value.
pub const SEQUENCE_UNASSIGNED: u64 = 0;

/// The actor vocabulary and Event identifier live in `athena-types`, where every other
/// crate can reach them; re-exported here for envelope-construction callers.
pub use athena_types::{Actor, EventId};

/// The concrete Event type name carried by the envelope, against the §23.2 catalog
/// (e.g. `TaskCreated`).
///
/// Held per payload type rather than as an enum: the catalog, the §17 transition tables
/// and the SSE layer all name Events the same way, and catalog entries whose payload is
/// empty must be constructible as easily as fully-mandated ones (spec section 23.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EventKind(&'static str);

impl EventKind {
    /// Wrap a catalog name.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self(name)
    }

    /// The catalog name.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        self.0
    }
}

impl PartialEq<&str> for EventKind {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

/// Everything that surrounds an Event and is independent of its type-specific payload:
/// the §23.1 envelope columns outside `type` and `payload`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventContext {
    /// Identifier, minted by Core Host (ATH-CORE-041).
    pub event_id: EventId,
    /// When it happened (RFC 3339).
    pub occurred_at: String,
    /// Who caused it.
    pub actor: Actor,
    /// Associated Task.
    pub task_id: Option<TaskId>,
    /// Associated PlanStep.
    pub plan_step_id: Option<PlanStepId>,
    /// Associated agent.
    pub agent_id: Option<AgentId>,
    /// Associated instance.
    pub instance_id: Option<InstanceId>,
    /// Associated skill.
    pub skill_id: Option<SkillId>,
    /// Correlation identifier, one per Task (ATH-OBS-009).
    pub correlation_id: String,
    /// The Event that directly caused this one.
    pub causation_id: Option<EventId>,
}

/// The §23.1 envelope of a concrete Event: a catalog type with its hygiene-checked,
/// serialized payload.
#[derive(Debug, Clone)]
pub struct EventEnvelope {
    /// Identifier.
    pub event_id: EventId,
    /// Payload schema version (ATH-EVENT-006).
    pub schema_version: u64,
    /// Event type, from the §23.2 catalog.
    pub kind: EventKind,
    /// When it happened (RFC 3339).
    pub occurred_at: String,
    /// Core-monotonic position; [`SEQUENCE_UNASSIGNED`] until the store assigns it.
    pub sequence: u64,
    /// Associated Task.
    pub task_id: Option<TaskId>,
    /// Associated PlanStep.
    pub plan_step_id: Option<PlanStepId>,
    /// Associated agent.
    pub agent_id: Option<AgentId>,
    /// Associated instance.
    pub instance_id: Option<InstanceId>,
    /// Associated skill.
    pub skill_id: Option<SkillId>,
    /// Correlation identifier, one per Task (ATH-OBS-009).
    pub correlation_id: String,
    /// The Event that directly caused this one.
    pub causation_id: Option<EventId>,
    /// Who caused it.
    pub actor: Actor,
    /// Payload as JSON text; ATH-EVENT-007 was already applied at construction.
    pub payload_json: String,
}

impl EventEnvelope {
    /// Record the position the store assigned to this Event.
    ///
    /// This records a fact produced by the store after the append transaction commits; it is
    /// book-keeping, not a mutation of the Event's meaning (ATH-EVENT-002).
    pub fn record_sequence(&mut self, sequence: u64) {
        self.sequence = sequence;
    }

    /// The catalog name.
    #[must_use]
    pub fn kind(&self) -> &str {
        self.kind.as_str()
    }

    /// The payload as JSON text.
    #[must_use]
    pub fn payload_json(&self) -> &str {
        &self.payload_json
    }
}
