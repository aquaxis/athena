//! The `write_memory` route's semantics (X-34): the Core Agent's `WriteMemory` request
//! applied by the Core Host, and the human's `POST /api/v1/memory` write, in one testable
//! face the resident cadence and the API handler both call (`core_execution`'s D-35
//! split: the cadence owns the clock, this owns the rule).
//!
//! Faces (`ATH-MEM-003`):
//! - Working Memory (`tier: working`): any Core Agent request writes — its scope is the
//!   Task, the §13.1 content set is the Working tier's own (`WorkingItem`), so a plain
//!   prose judgement rides a *memory note* item. The ceiling (`memory.working_max_records`)
//!   is the tier's own (ATH-MEM-002), and the write route's fallback (R-MEM-07) is the
//!   eviction the tier already implements.
//! - Long Term (`tier: long`): only an *explicit* requester writes
//!   (`long_term::Requester` — the Core Agent's explicit judgement or the human
//!   instruction), so `apply_write_memory`'s face is explicit by construction; the
//!   provenance gate is ATH-MEM-005's (task/event anchors mandatory), and the ceiling is
//!   the retention policy (ATH-MEM-002: refusal, never a silent digest —
//!   `write_with_retention`). The requester's identity (`HumanInstruction`) carries the
//!   API token's name, never the token itself.

use crate::memory_manager::long_term as long_term_face;
use athena_events::redact::mask_text;
use athena_persistence::AthenaStore;
use athena_persistence::repository::TaskRepository;
use athena_types::{EventId, MemoryOrigin, MemoryTier, ProjectId, TaskId};

/// Why a `write_memory` request was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WriteMemoryFault {
    /// The Task the memory would belong to is unknown.
    #[error("the task {0:?} does not exist")]
    UnknownTask(String),
    /// Working Memory write refused: the ceiling was met and the tier's own fallback
    /// could not evict (a store fault here is surfaced; a refusal is never silent).
    #[error("the working memory write refused: {0}")]
    Working(String),
    /// Long Term write refused by the tier's own gate (implicit requester, provenance
    /// missing, tier disabled).
    #[error("the long-term write refused: {0}")]
    Long(&'static str),
    /// Long Term write refused by the retention policy (ceiling reached per scope).
    #[error("the long-term retention refused: {0}")]
    LongRetention(String),
    /// The store refused; surfaced whole.
    #[error("store fault: {0}")]
    Store(String),
}

/// Which explicit requester carried the write, as the API's own faces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Requester {
    /// The Core Agent judged explicitly (`WriteMemory { tier: Long }` through the
    /// protocol gate — `ATH-MEM-003`'s (a) face).
    CoreAgentJudgement,
    /// A human asked through the API (`ATH-MEM-003`'s (b) face); the identity is the
    /// token's audit name (`HumanInstruction { identity }`).
    HumanInstruction(String),
}

/// What one accepted write produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteMemoryReport {
    /// The tier written.
    pub tier: MemoryTier,
    /// The record's id (`MemoryId`).
    pub record_id: String,
    /// The scope the record landed under (the Task's `scope_id` for Working; the
    /// workspace's `ProjectId` for Long).
    pub scope_id: String,
}

/// Apply an explicit `WriteMemory` request (`ATH-MEM-003`) — either tier, from either
/// requester face. Hygiene runs at the writer (R-POL-13): the content is masked before
/// it ever reaches the store.
///
/// The Task's workspace derives the long tier's scope (`project_scope_of`), and the
/// Task's own id carries the provenance anchor (`ATH-MEM-005`) when the caller supplies
/// no stricter event anchor.
///
/// # Errors
/// [`WriteMemoryFault`] with the violated rule; the caller records every refusal as a
/// `PrivilegedRequestDenied` Event (`ATH-CORE-037`'s answer face).
#[allow(
    clippy::too_many_arguments,
    reason = "each argument is one gate input the caller owns: the store, the requester \
              face, the tier, the masked content, the provenance anchor pair's task, the \
              optional stricter event anchor, the retention ceiling, the clock and the \
              correlation — bundling them would only hide the caller's obligations"
)]
pub fn apply_write_memory(
    store: &mut AthenaStore,
    requester: &Requester,
    tier: WritableTier,
    content: &str,
    task: &TaskId,
    event_anchor: Option<&EventId>,
    max_records: u64,
    now: &str,
    correlation_id: &str,
) -> Result<WriteMemoryReport, WriteMemoryFault> {
    let Some(task_row) = TaskRepository::get(store, task)
        .map_err(|fault| WriteMemoryFault::Store(fault.to_string()))?
    else {
        return Err(WriteMemoryFault::UnknownTask(task.as_str().to_string()));
    };
    let masked = mask_text(content);
    let event_anchor = event_anchor.cloned();

    match tier {
        WritableTier::Working => apply_working(store, &task_row, &masked, event_anchor, now),
        WritableTier::Long => apply_long(
            store,
            requester,
            &task_row,
            &masked,
            event_anchor,
            max_records,
            now,
            correlation_id,
        ),
    }
}

/// The tiers a request names (the wire kind's own face, re-stated without pulling the
/// protocol's serde surface into the callers' signatures).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WritableTier {
    /// Working Memory (`ATH-MEM-003`).
    Working,
    /// Long Term Memory (`ATH-MEM-003`, explicit requester only).
    Long,
}

/// The Working face: the tier's ceiling is `memory.working_max_records`, its content set
/// rides the tier's own `WorkingItem` note (a plain prose judgement rides a memory note —
/// the §13.1 minimum the rebuild context lifts stays intact), and the tier's own fallback
/// (R-MEM-07's eviction) is what the write applies when at ceiling.
fn apply_working(
    store: &mut AthenaStore,
    task: &athena_types::Task,
    masked: &str,
    event_anchor: Option<EventId>,
    now: &str,
) -> Result<WriteMemoryReport, WriteMemoryFault> {
    use crate::working_memory::{WorkingItem, WorkingMemory, WorkingWrite};

    // The ceiling is the configuration's (working_max_records); eviction is the tier's
    // own fallback, so a ceiling met is handled inside the tier's write — the fault here
    // is only a real store fault.
    let working = WorkingMemory::new(memory_manager_policy_working_records());
    let write = WorkingWrite {
        // The judgement itself is the record (`ATH-MEM-003`'s Working face): the §13.1
        // list is a floor (ATH-MEM-007), the content arrives already masked, and the
        // tier's ceiling / fallback treat it as any content record. The Event anchor
        // (when the caller carries one) stays the record's provenance origin.
        item: WorkingItem::AgentJudgement {
            content: masked.to_string(),
        },
        origin_event_id: event_anchor.map(|event| event.as_str().to_string()),
        at: now.to_string(),
    };
    let report = working
        .write(store, &task.id, write)
        .map_err(|fault| WriteMemoryFault::Working(fault.to_string()))?;
    Ok(WriteMemoryReport {
        tier: MemoryTier::Working,
        record_id: report.record_id,
        scope_id: task.id.as_str().to_string(),
    })
}

/// The Long face: the tier's gate (explicit requester, provenance, tier enabled) and the
/// retention ceiling (`write_with_retention` — refusal at pressure, never a digest).
#[allow(
    clippy::too_many_arguments,
    reason = "one argument per gate input: the store, the requester face, the tier's anchor task, the masked content, the provenance anchor, the retention ceiling, and the clock/correlation pair"
)]
fn apply_long(
    store: &mut AthenaStore,
    requester: &Requester,
    task: &athena_types::Task,
    masked: &str,
    event_anchor: Option<EventId>,
    max_records: u64,
    now: &str,
    correlation_id: &str,
) -> Result<WriteMemoryReport, WriteMemoryFault> {
    let requester = match requester {
        Requester::CoreAgentJudgement => long_term_face::Requester::CoreAgentJudgement,
        Requester::HumanInstruction(identity) => long_term_face::Requester::HumanInstruction {
            identity: identity.clone(),
        },
    };
    let scope = long_term_face::project_scope_of(&task.workspace);
    let provenance = MemoryOrigin {
        task_id: Some(task.id.clone()),
        event_id: event_anchor,
    };
    let staged = long_term_face::review(
        &requester,
        masked,
        &scope,
        long_term_face_enabled(),
        provenance,
    )
    .map_err(|fault| WriteMemoryFault::Long(fault_static_text(&fault)))?;
    long_term_face::write_with_retention(store, &staged, max_records)
        .map_err(|fault| match fault {
            long_term_face::RetentionFault::Store(store_fault) => {
                WriteMemoryFault::Store(store_fault.to_string())
            }
            long_term_face::RetentionFault::CeilingReached {
                used,
                ceiling,
                scope,
            } => WriteMemoryFault::LongRetention(format!(
                "ceiling reached for {scope} ({used}/{ceiling}); removal is an explicit judgement (ATH-MEM-002)"
            )),
        })?;
    // The catalog Event (spec §26.4): reference-shaped facts only (ATH-EVENT-007).
    let facts = long_term_face::created_event_facts(&staged);
    let event = athena_persistence::NewEvent {
        event_id: EventId::new(),
        schema_version: 1,
        event_type: "MemoryRecordCreated".into(),
        occurred_at: now.to_string(),
        task_id: Some(task.id.clone()),
        plan_step_id: None,
        agent_id: None,
        instance_id: None,
        skill_id: None,
        correlation_id: correlation_id.to_string(),
        causation_id: None,
        actor: requester_actor(&requester).to_string(),
        payload_json: serde_json::json!({
            "memory_id": facts.0,
            "kind": facts.1,
            "scope": facts.2,
            "record_created_at": facts.3,
        })
        .to_string(),
    };
    crate::EventRepository::append(store, &event)
        .map_err(|fault| WriteMemoryFault::Store(fault.to_string()))?;
    Ok(WriteMemoryReport {
        tier: MemoryTier::Long,
        record_id: facts.0,
        scope_id: scope.as_str().to_string(),
    })
}

/// The requester's Event actor face (`ATH-EVENT-016`'s who-caused-it).
fn requester_actor(requester: &long_term_face::Requester) -> &'static str {
    match requester {
        long_term_face::Requester::CoreAgentJudgement => "core-agent",
        long_term_face::Requester::HumanInstruction { .. } => "human",
    }
}

/// The tier's enablement, from configuration (`memory.long_term.enabled`).
fn long_term_face_enabled() -> bool {
    athena_config::schema::MemorySection::default()
        .long_term
        .enabled
}

/// The Working tier's ceiling, from configuration (`memory.working_max_records`).
fn memory_manager_policy_working_records() -> u64 {
    athena_config::schema::MemorySection::default().working_max_records
}

/// The tier's fault taxonomy, as the wire's stable text.
fn fault_static_text(fault: &long_term_face::WriteFault) -> &'static str {
    match fault {
        long_term_face::WriteFault::Implicit(_) => "implicit writes are refused",
        long_term_face::WriteFault::ProvenanceMissing => "provenance missing",
        long_term_face::WriteFault::TierDisabled => "tier disabled",
    }
}

/// The project scope of a workspace path — re-exported for the API's POST handler, whose
/// body names a workspace and so derives the same `ProjectId` the tier's own gate would
/// (`R-DAT-02`).
#[must_use]
pub fn project_scope_of(workspace: &str) -> ProjectId {
    long_term_face::project_scope_of(workspace)
}
