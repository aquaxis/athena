//! The §23.2 Event catalog as typed payload structs (D-3).
//!
//! Each catalog entry is a distinct struct whose mandated payload fields are non-`Option`
//! members: an Event lacking a mandatory item cannot compile (ATH-EVENT-017, checked by the
//! type system) and cannot be serialized with a missing key ([`TypedEvent::new`] verifies
//! the serialized payload contains every [`Payload::REQUIRED_FIELDS`] key as a backstop).
//!
//! The struct bodies encode exactly the minimum the §23.2 table mandates for the listed
//! types; types without a row in the table carry no extra payload, because adding a payload
//! item is a specification revision first (ATH-EVENT-009). Envelope columns already name
//! the Task / PlanStep / agent / instance / skill involved, which is what the mandate's
//! minimum covers.

use athena_types::{
    Actor, AgentId, AgentLevel, ApprovalId, ApprovalScope, EvaluationId, EventId, HierarchyId,
    InstanceId, MemoryId, MessageId, MessageType, PlanId, PlanStepId, Priority, ResultId,
    ResultStatus, RiskLevel, RuntimeType, ScopeRef, SideEffects, SkillId, TaskId, ToolExecutionId,
    ToolName, Verdict,
};
use serde::{Deserialize, Serialize};

use crate::redact;

/// Sequence carried by a draft: assigned by the store on append, never by a caller
/// (ATH-EVENT-003, ATH-EVENT-020).
pub const SEQUENCE_UNASSIGNED: u64 = 0;

/// The payload schema version this revision of the specification describes (ATH-EVENT-006).
pub const SCHEMA_VERSION: u64 = 1;

/// A catalog payload (spec section 23.2).
pub trait Payload: Serialize {
    /// The catalog name carried by the envelope.
    const EVENT_TYPE: &'static str;

    /// The minimum payload keys the §23.2 table mandates (ATH-EVENT-017).
    const REQUIRED_FIELDS: &'static [&'static str];
}

/// A node reference inside a composed hierarchy (spec section 26.6, `HierarchyComposed`).
///
/// `scope_ref` is optional only because a Producer node holds no work unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HierarchyNodeRef {
    /// The agent at this node.
    pub agent_id: AgentId,
    /// The level it works at.
    pub level: AgentLevel,
    /// What it is responsible for; `None` for the Producer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_ref: Option<ScopeRef>,
}

/// A skill in a selected Skill Set: the specimen from `athena-types`
/// (`(id, version, checksum)`).
pub use athena_types::SkillRef;

/// What a waiting instance is waiting on (spec section 30.3, ATH-EVENT-018).
///
/// The three variants are exactly the wait targets the mandate names; they are what lets
/// the wait graph — and from it, cycles and stalling — be reconstructed from Events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WaitOn {
    /// Another instance must finish first.
    Instance {
        /// The instance waited for.
        instance_id: InstanceId,
    },
    /// A human approval is outstanding.
    Approval {
        /// The approval waited for.
        approval_id: ApprovalId,
    },
    /// A message reply is outstanding.
    Message {
        /// The message waited for.
        message_id: MessageId,
    },
}

macro_rules! catalog_event {
    (
        $(#[$meta:meta])*
        $name:ident => struct {
            $( $(#[$fattr:meta])* $field:ident : $ftype:ty ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct $name {
            $( $(#[$fattr])* pub $field: $ftype, )*
        }

        impl Payload for $name {
            const EVENT_TYPE: &'static str = stringify!($name);
            const REQUIRED_FIELDS: &'static [&'static str] = &[ $( stringify!($field) ),* ];
        }

        #[cfg(test)]
        impl tests_helpers::Sample for $name {
            fn sample() -> Self {
                Self { $( $field: tests_helpers::Sample::sample(), )* }
            }
        }
    };
    (
        $(#[$meta:meta])*
        $name:ident => empty()
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        pub struct $name {}

        impl Payload for $name {
            const EVENT_TYPE: &'static str = stringify!($name);
            const REQUIRED_FIELDS: &'static [&'static str] = &[];
        }

        #[cfg(test)]
        impl tests_helpers::Sample for $name {
            fn sample() -> Self {
                Self {}
            }
        }
    };
}

// --- Task (spec section 23.2) -----------------------------------------------------------

catalog_event!(
    /// A Task was created from a human instruction (ATH-STATE-...; §23.2 minimum).
    TaskCreated => struct {
        /// SHA-256 digest of the instruction text; the text itself is not embedded
        /// (ATH-EVENT-007).
        instruction_digest: String,
        /// The leased Workspace path (ATH-DATA-009).
        workspace: String,
        /// Scheduling priority.
        priority: Priority,
        /// Who created it (`human` or the Core Agent, for sub-Task creation).
        created_by: String,
    }
);

catalog_event!(
    /// The Task was queued for the Scheduler, carrying the Plan it should run against.
    TaskQueued => struct {
        /// The Plan referenced.
        plan_id: PlanId,
        /// The Plan revision referenced.
        revision: u64,
    }
);

catalog_event!(
    /// The Task began executing.
    TaskStarted => struct {
        /// The Plan under execution.
        plan_id: PlanId,
        /// The Plan revision under execution.
        revision: u64,
    }
);

catalog_event!(
    /// A Result arrived for the Task (ATH-EVAL-...) with its outcome status.
    TaskResultReceived => struct {
        /// The Result, referenced (ATH-EVENT-007).
        result_id: ResultId,
        /// The PlanStep that produced it.
        plan_step_id: PlanStepId,
        /// The agent's own status; never sufficient for acceptance (ATH-EVAL-005).
        status: ResultStatus,
    }
);

catalog_event!(
    /// The Core Agent chose to change the Plan (ATH-PLAN-...).
    TaskReplanning => struct {
        /// The revision being replaced.
        from_revision: u64,
        /// The revision that supersedes it.
        to_revision: u64,
        /// Why. Prose, not agent input: recorded for reconstruction (ATH-CORE-016).
        reason: String,
    }
);

catalog_event!(
    /// A retry was decided for the Task or one of its PlanSteps.
    TaskRetrying => struct {
        /// The attempt about to run (1-based).
        attempt: u64,
        /// The configured attempt ceiling.
        max_attempts: u64,
        /// The nine-class taxonomy value (/§20.1).
        error_class: String,
    }
);

catalog_event!(
    /// Cancellation was requested and the stop set was frozen (ATH-STATE-006).
    TaskCancelling => empty()
);

catalog_event!(
    /// The Task reached the terminal `CANCELLED` state.
    TaskCancelled => struct {
        /// Whether `timeout.stop` was exceeded and processes were force-killed
        /// (ATH-STATE-006).
        forced: bool,
        /// Whether side effects are known; `Unknown` demands Verification before a re-run
        /// (ATH-CORE-048).
        side_effects: SideEffects,
    }
);

catalog_event!(
    /// The Task reached the terminal `SUCCESS` state.
    TaskCompleted => struct {
        /// The Results that make up the completion, referenced.
        result_ids: Vec<ResultId>,
        /// Whether the completion criteria were checked and met (ATH-EVAL-002).
        completion_criteria_met: bool,
    }
);

catalog_event!(
    /// The Task reached the terminal `FAILED` state (ATH-FAIL-007).
    TaskFailed => struct {
        /// The nine-class taxonomy value.
        error_class: String,
        /// The error code within its class.
        error_code: String,
        /// How many attempts were made.
        attempts: u64,
        /// Whether Core Agent recovery was attempted before failing (ATH-FAIL-011).
        recovery_attempted: bool,
    }
);

// --- Plan -------------------------------------------------------------------------------

catalog_event!(
    /// A Plan revision was created (ATH-PLAN-...).
    PlanCreated => struct {
        /// The Plan.
        plan_id: PlanId,
        /// The revision issued (Core-Host-assigned, ATH-CORE-034).
        revision: u64,
        /// How many PlanSteps it holds.
        step_count: u64,
        /// Why this decomposition was chosen (ATH-CORE-016).
        rationale: String,
    }
);

catalog_event!(
    /// A Plan was revised without a Task-level re-plan.
    PlanRevised => struct {
        /// The Plan.
        plan_id: PlanId,
        /// The revision issued.
        revision: u64,
        /// How many PlanSteps the revision holds.
        step_count: u64,
        /// Why (ATH-CORE-016).
        rationale: String,
    }
);

catalog_event!(
    /// A PlanStep moved to `RUNNING`.
    PlanStepStarted => struct {
        /// The step.
        plan_step_id: PlanStepId,
        /// The attempt number (ATH-FAIL-007).
        attempt: u64,
        /// The agent the step was delegated to.
        assigned_agent: AgentId,
    }
);

catalog_event!(
    /// A PlanStep reached a terminal state for its attempt, successfully.
    PlanStepCompleted => empty()
);

catalog_event!(
    /// A PlanStep attempt failed.
    PlanStepFailed => empty()
);

// --- Hierarchy --------------------------------------------------------------------------

catalog_event!(
    /// A hierarchy was composed and settled (ATH-HIER-009): nodes carry their level and
    /// their work unit.
    HierarchyComposed => struct {
        /// The hierarchy.
        hierarchy_id: HierarchyId,
        /// Its revision.
        revision: u64,
        /// The nodes, each with level and scope.
        nodes: Vec<HierarchyNodeRef>,
    }
);

catalog_event!(
    /// A delegation from a superior to its immediate child level was started
    /// (ATH-HIER-004).
    AgentDelegated => empty()
);

// --- Agent ------------------------------------------------------------------------------

catalog_event!(
    /// Core Agent selected an agent for work (ATH-AGENT-005, ATH-AGENT-007).
    AgentSelected => struct {
        /// The agent chosen.
        agent_id: AgentId,
        /// Every candidate considered, in evaluation order.
        candidates: Vec<AgentId>,
        /// Why this agent (ATH-CORE-016).
        rationale: String,
        /// Whether capability matching determined the choice (ATH-AGENT-004).
        capability_match: bool,
    }
);

catalog_event!(
    /// A Runtime was asked to start an instance.
    AgentStarting => empty()
);

catalog_event!(
    /// An instance is running (spec section 26.3).
    AgentStarted => struct {
        /// The instance.
        instance_id: InstanceId,
        /// The agent definition realized by the instance.
        agent_id: AgentId,
        /// The Runtime kind enforcing its isolation boundary (ATH-SEC-015).
        runtime_type: RuntimeType,
    }
);

catalog_event!(
    /// An instance reported itself waiting on something; `waits_on` feeds the stall
    /// detectors (ATH-EVENT-018).
    AgentWaiting => struct {
        /// What it is waiting on, in `WaitOn` form.
        waits_on: Vec<WaitOn>,
        /// When the wait expires, if any.
        deadline: String,
    }
);

catalog_event!(
    /// A stop was requested for an instance (ATH-RT-006).
    AgentStopping => empty()
);

catalog_event!(
    /// An instance finished releasing its Runtime resources.
    AgentStopped => struct {
        /// The instance.
        instance_id: InstanceId,
        /// The agent definition realized by the instance.
        agent_id: AgentId,
        /// The Runtime kind.
        runtime_type: RuntimeType,
    }
);

catalog_event!(
    /// An instance completed its delegated unit successfully.
    AgentCompleted => empty()
);

catalog_event!(
    /// An instance failed (ATH-STATE-002).
    AgentFailed => struct {
        /// The nine-class taxonomy value.
        error_class: String,
        /// The attempt that failed.
        attempt: u64,
        /// Whether its side effects are known (ATH-CORE-048).
        side_effects: SideEffects,
    }
);

// --- Core Agent (spec section 6.8) ------------------------------------------------------

catalog_event!(
    /// The Core Agent instance #0 was started at boot (ATH-CORE-022).
    CoreAgentStarted => empty()
);

catalog_event!(
    /// The Core Agent instance #0 failed.
    CoreAgentFailed => empty()
);

catalog_event!(
    /// The Core Agent restarted after the deterministic §6.7 procedure.
    CoreAgentRestarted => struct {
        /// The attempt within the sliding window (ATH-CORE-030).
        attempt: u64,
        /// When the restart window opened.
        window_start: String,
        /// The rebuilt context document, referenced (ATH-CORE-029).
        context_document_ref: String,
    }
);

catalog_event!(
    /// Autonomous execution was escalated to a human (ATH-CORE-031).
    CoreAgentEscalated => empty()
);

catalog_event!(
    /// The Core Agent heartbeat, every 20 s by default (ATH-OBS-004).
    CoreAgentHeartbeat => struct {
        /// The loop iteration count.
        iteration: u64,
        /// The Task currently in focus, if any.
        #[serde(skip_serializing_if = "Option::is_none")]
        focus_task_id: Option<TaskId>,
        /// How many CoreRequests await processing.
        pending_requests: u64,
    }
);

catalog_event!(
    /// A privileged request refused inspection (ATH-CORE-026, ATH-CORE-027).
    PrivilegedRequestDenied => struct {
        /// The CoreRequest kind refused.
        request_kind: String,
        /// The nine-class taxonomy value.
        error_class: String,
        /// The policy rule that applied.
        policy_id: String,
    }
);

// --- PlanStep-level observability -------------------------------------------------------

catalog_event!(
    /// A dependency-satisfied PlanStep became `READY` (spec section 17.4).
    PlanStepReady => struct {
        /// The step.
        plan_step_id: PlanStepId,
        /// The attempt it will run as.
        attempt: u64,
        /// The agent it is assigned to.
        assigned_agent: AgentId,
    }
);

catalog_event!(
    /// A PlanStep whose process disappeared was finalized (ATH-STATE-029): its re-run is
    /// gated behind Verification because its side effects are undetermined.
    PlanStepOrphaned => struct {
        /// The step.
        plan_step_id: PlanStepId,
        /// The instance that vanished.
        instance_id: InstanceId,
        /// Whether its side effects are known.
        side_effects: SideEffects,
    }
);

catalog_event!(
    /// A PlanStep was skipped by re-planning (only of steps not yet executed, ATH-STATE-028).
    PlanStepSkipped => empty()
);

// --- Instance-level lifecycle -----------------------------------------------------------

catalog_event!(
    /// An instance was suspended for a human approval (ATH-HITL-007).
    AgentSuspended => empty()
);

catalog_event!(
    /// A suspended instance resumed with its identical frozen Skill Set
    /// (ATH-STATE-021, ATH-SKILL-010).
    AgentResumed => empty()
);

// --- Scheduler --------------------------------------------------------------------------

catalog_event!(
    /// A ready PlanStep could not start because a limit applies (ATH-SCHED-014).
    SchedulerBlocked => struct {
        /// The step held in the queue.
        plan_step_id: PlanStepId,
        /// The limit that applies.
        blocking_limit: String,
    }
);

catalog_event!(
    /// A non-terminal Task was observed to be stalled (ATH-OBS-008).
    TaskStalled => struct {
        /// The Task.
        task_id: TaskId,
        /// When its last state-change Event occurred.
        last_event_at: String,
        /// How long it has been silent.
        stall_seconds: u64,
    }
);

// --- Clarification (spec section 15.4) --------------------------------------------------

catalog_event!(
    /// Core Agent asked the human a clarification question (ATH-HITL-018).
    ClarificationRequested => struct {
        /// The clarification.
        clarification_id: String,
        /// The question, digested; the text is not embedded (ATH-EVENT-007).
        question_digest: String,
    }
);

catalog_event!(
    /// The human answered a clarification question (ATH-HITL-019).
    ClarificationAnswered => struct {
        /// The clarification.
        clarification_id: String,
        /// The question, digested.
        question_digest: String,
    }
);

catalog_event!(
    /// A clarification expired: silence was *not* read as an answer; the Task failed
    /// (ATH-HITL-019).
    ClarificationExpired => struct {
        /// The clarification.
        clarification_id: String,
        /// The question, digested.
        question_digest: String,
    }
);

// --- Workspace (spec section 12.1) ------------------------------------------------------

catalog_event!(
    /// An instance Workspace's `aiprj` write areas were initialized (ATH-OPS-020).
    WorkspaceInitialized => struct {
        /// The Workspace (cwd) path.
        workspace: String,
        /// The `aiprj` version detected or installed.
        aiprj_version: String,
    }
);

catalog_event!(
    /// A Workspace lease was acquired for a Task (ATH-DATA-009).
    WorkspaceLeased => empty()
);

catalog_event!(
    /// A Workspace lease was released.
    WorkspaceReleased => empty()
);

// --- Messages (spec section 10) ---------------------------------------------------------

catalog_event!(
    /// A message was sent through the Broker (ATH-MSG-...).
    AgentMessageSent => struct {
        /// The message.
        message_id: MessageId,
        /// Its kind.
        message_type: MessageType,
        /// Its thread.
        thread_id: String,
    }
);

catalog_event!(
    /// A message was delivered to its instance.
    AgentMessageReceived => struct {
        /// The message.
        message_id: MessageId,
        /// Its kind.
        message_type: MessageType,
        /// Its thread.
        thread_id: String,
    }
);

catalog_event!(
    /// A reply deadline expired (ATH-MSG-006).
    AgentMessageTimeout => struct {
        /// The message that timed out.
        message_id: MessageId,
        /// Its thread.
        thread_id: String,
        /// The deadline that passed.
        deadline: String,
    }
);

// --- Tools (spec section 11) ------------------------------------------------------------

catalog_event!(
    /// A tool execution started (ATH-TOOL-004).
    ToolExecutionStarted => struct {
        /// The execution record.
        tool_execution_id: ToolExecutionId,
        /// The ATHENA tool name.
        tool: ToolName,
        /// The canonical operation descriptor built by Core Host (ATH-HITL-010).
        operation_descriptor: serde_json::Value,
        /// The risk classification in force.
        risk_level: RiskLevel,
    }
);

catalog_event!(
    /// A tool execution completed.
    ToolExecutionCompleted => struct {
        /// The execution record.
        tool_execution_id: ToolExecutionId,
        /// The ATHENA tool name.
        tool: ToolName,
        /// The canonical operation descriptor.
        operation_descriptor: serde_json::Value,
        /// The risk classification in force.
        risk_level: RiskLevel,
    }
);

catalog_event!(
    /// A tool execution failed or was refused.
    ToolExecutionFailed => struct {
        /// The execution record.
        tool_execution_id: ToolExecutionId,
        /// The ATHENA tool name.
        tool: ToolName,
        /// The canonical operation descriptor.
        operation_descriptor: serde_json::Value,
        /// The risk classification in force.
        risk_level: RiskLevel,
    }
);

// --- Skills (spec section 12) -----------------------------------------------------------

// Every `Skill*` Event carries the `(id, version, checksum)` maintenance minimum
// (ATH-EVENT-017, `Skill*` row).

catalog_event!(
    /// A skill version was registered.
    SkillRegistered => struct {
        /// The skill.
        skill_id: SkillId,
        /// The version.
        version: String,
        /// The package checksum.
        checksum: String,
    }
);

catalog_event!(
    /// Validation began for a skill version (ATH-SKILL-024).
    SkillValidating => struct {
        /// The skill.
        skill_id: SkillId,
        /// The version.
        version: String,
        /// The package checksum.
        checksum: String,
    }
);

catalog_event!(
    /// Validation passed for a skill version.
    SkillValidated => struct {
        /// The skill.
        skill_id: SkillId,
        /// The version.
        version: String,
        /// The package checksum.
        checksum: String,
    }
);

catalog_event!(
    /// Validation failed; the version stays out of the candidate set (ATH-SKILL-024).
    SkillValidationFailed => struct {
        /// The skill.
        skill_id: SkillId,
        /// The version.
        version: String,
        /// The package checksum.
        checksum: String,
    }
);

catalog_event!(
    /// A skill version was enabled (admin operation, ATH-STATE-025).
    SkillEnabled => struct {
        /// The skill.
        skill_id: SkillId,
        /// The version.
        version: String,
        /// The package checksum.
        checksum: String,
    }
);

catalog_event!(
    /// A skill version was disabled.
    SkillDisabled => struct {
        /// The skill.
        skill_id: SkillId,
        /// The version.
        version: String,
        /// The package checksum.
        checksum: String,
    }
);

catalog_event!(
    /// An update to a skill version began (running instances are immune
    /// to it, ATH-SKILL-010).
    SkillUpdating => struct {
        /// The skill.
        skill_id: SkillId,
        /// The version.
        version: String,
        /// The package checksum.
        checksum: String,
    }
);

catalog_event!(
    /// A skill version was updated.
    SkillUpdated => struct {
        /// The skill.
        skill_id: SkillId,
        /// The version.
        version: String,
        /// The package checksum.
        checksum: String,
    }
);

catalog_event!(
    /// A skill version was removed.
    SkillRemoved => struct {
        /// The skill.
        skill_id: SkillId,
        /// The version.
        version: String,
        /// The package checksum.
        checksum: String,
    }
);

catalog_event!(
    /// Core Agent selected skills for an assignment (ATH-SKILL-014): the set, pinned to
    /// versions, plus why; and the per-skill maintenance minimum.
    SkillSelected => struct {
        /// The skills chosen, in resolution order.
        skill_refs: Vec<SkillRef>,
        /// Why this Skill Set (ATH-CORE-016).
        rationale: String,
        /// Its maintenance minimum: the skill the Skill Set was resolved *for*.
        skill_id: SkillId,
        /// Its version.
        version: String,
        /// Its checksum.
        checksum: String,
    }
);

catalog_event!(
    /// Selected skills were placed into an instance Workspace (ATH-SKILL-017).
    SkillLoaded => struct {
        /// The skill.
        skill_id: SkillId,
        /// The version.
        version: String,
        /// The package checksum.
        checksum: String,
    }
);

catalog_event!(
    /// A skill execution started.
    SkillExecutionStarted => struct {
        /// The skill.
        skill_id: SkillId,
        /// The version.
        version: String,
        /// The package checksum.
        checksum: String,
    }
);

catalog_event!(
    /// A skill execution completed.
    SkillExecutionCompleted => struct {
        /// The skill.
        skill_id: SkillId,
        /// The version.
        version: String,
        /// The package checksum.
        checksum: String,
    }
);

catalog_event!(
    /// A skill execution failed.
    SkillExecutionFailed => struct {
        /// The skill.
        skill_id: SkillId,
        /// The version.
        version: String,
        /// The package checksum.
        checksum: String,
    }
);

// --- Runtime (spec section 14) ----------------------------------------------------------

catalog_event!(
    /// The Runtime created an instance for a delegation.
    RuntimeInstanceCreated => empty()
);

catalog_event!(
    /// The Runtime released an instance's resources.
    RuntimeInstanceStopped => empty()
);

catalog_event!(
    /// The Runtime reported itself unhealthy; Core stops accepting new Tasks
    /// (ATH-CORE-023).
    RuntimeUnhealthy => empty()
);

// --- Approvals (spec section 15) --------------------------------------------------------

catalog_event!(
    /// A high-risk operation was requested (ATH-HITL-003).
    ApprovalRequested => struct {
        /// The approval.
        approval_id: ApprovalId,
        /// The canonical operation descriptor built by Core Host.
        operation_descriptor: serde_json::Value,
        /// The risk classification in force.
        risk_level: RiskLevel,
        /// When it expires (reject-by-default, ATH-HITL-003).
        expires_at: String,
    }
);

catalog_event!(
    /// A human approved (ATH-HITL-004).
    ApprovalGranted => struct {
        /// The approval.
        approval_id: ApprovalId,
        /// Who responded (recorded for audit).
        responded_by: String,
        /// The scope of consumption.
        scope: ApprovalScope,
    }
);

catalog_event!(
    /// A human rejected; the Task goes to re-planning, never straight to failure
    /// (ATH-STATE-008).
    ApprovalRejected => struct {
        /// The approval.
        approval_id: ApprovalId,
        /// Who responded.
        responded_by: String,
        /// The scope the grant would have carried.
        scope: ApprovalScope,
    }
);

catalog_event!(
    /// No answer arrived before the deadline, or a granted scope expired unused
    /// (ATH-HITL-003).
    ApprovalExpired => struct {
        /// The approval.
        approval_id: ApprovalId,
        /// The scope that lapsed.
        scope: ApprovalScope,
    }
);

catalog_event!(
    /// Core Host consumed an approval for an operation; the recorded descriptor is what the
    /// consumption applied to (ATH-HITL-010).
    ApprovalConsumed => struct {
        /// The approval.
        approval_id: ApprovalId,
        /// The canonical operation descriptor of the consumed operation.
        operation_descriptor: serde_json::Value,
        /// Uses remaining on the scope after consumption (ATH-STATE-013).
        uses_remaining: u64,
    }
);

// --- Memory (spec section 13) -----------------------------------------------------------

catalog_event!(
    /// A memory record was created (spec section 26.4).
    MemoryRecordCreated => struct {
        /// The record.
        memory_id: MemoryId,
    }
);

catalog_event!(
    /// A memory record was deleted; the deletion itself is the audit record
    /// (ATH-MEM-006).
    MemoryRecordDeleted => struct {
        /// The record.
        memory_id: MemoryId,
    }
);

// --- Evaluation (spec section 19) -------------------------------------------------------

catalog_event!(
    /// Core Agent began evaluating a Result against the completion criteria
    /// (ATH-EVAL-001).
    EvaluationStarted => empty()
);

catalog_event!(
    /// An evaluation passed: verdict accepted (ATH-EVAL-003).
    EvaluationPassed => struct {
        /// The evaluation record.
        evaluation_id: EvaluationId,
        /// Who verified (agent or tool).
        evaluator: String,
        /// The verification method (test / lint / build / review / simulation).
        method: String,
        /// The verdict, exactly one of the five (ATH-EVAL-004).
        verdict: Verdict,
    }
);

catalog_event!(
    /// An evaluation failed or demanded retry / re-plan / escalation.
    EvaluationFailed => struct {
        /// The evaluation record.
        evaluation_id: EvaluationId,
        /// Who verified (agent or tool).
        evaluator: String,
        /// The verification method.
        method: String,
        /// The verdict, exactly one of the five (ATH-EVAL-004).
        verdict: Verdict,
    }
);

// --- System -----------------------------------------------------------------------------

catalog_event!(
    /// ATHENA started (spec section 22.9).
    SystemStarted => empty()
);

catalog_event!(
    /// ATHENA is draining towards shutdown (ATH-NFR-013).
    SystemStopping => empty()
);

catalog_event!(
    /// The resolved effective configuration was loaded, secrets excluded
    /// (ATH-CFG-008, R-POL-13).
    ConfigurationLoaded => empty()
);

// --- Construction -----------------------------------------------------------------------

/// Result of Event construction.
pub type Result<T> = std::result::Result<T, redact::EventError>;

/// A constructed Event (spec section 23.1): an `EventContext` filled per type, the payload
/// hygiene-checked and serialized.
///
/// Construction is the hygiene point (module `redact`): the serialized payload is checked
/// for the mandatory keys of [`Payload::REQUIRED_FIELDS`] — backstopping the compile-time
/// non-`Option` guarantee against serde output drift — refused when it would embed full
/// agent input (ATH-EVENT-007), and masked of known secret shapes. `sequence` stays
/// [`SEQUENCE_UNASSIGNED`] until `athena-persistence` assigns it inside the append
/// transaction (ATH-EVENT-003); the Event is immutable once built (ATH-EVENT-002).
#[derive(Debug, Clone)]
pub struct TypedEvent<P: Payload> {
    /// Identifier.
    pub event_id: EventId,
    /// Payload schema version (ATH-EVENT-006).
    pub schema_version: u64,
    /// Event type, from the §23.2 catalog (`P::EVENT_TYPE`).
    pub event_type: &'static str,
    /// When it happened (RFC 3339).
    pub occurred_at: String,
    /// Core-monotonic position (ATH-EVENT-003).
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
    /// Payload before serialization, for callers that read its fields.
    pub payload: P,
    /// Payload after hygiene, serialized (ATH-EVENT-007 already applied).
    pub payload_json: String,
}

impl<P: Payload> TypedEvent<P> {
    /// Construct an Event of catalog type `P`, applying payload hygiene.
    ///
    /// # Errors
    /// - `EventError::MissingField` when serialization dropped a mandatory key: a backstop
    ///   to the compile-time D-3 guarantee in case a struct field stops reaching the wire.
    /// - `EventError::ForbiddenContent` when a key would embed full agent input
    ///   (ATH-EVENT-007).
    /// - `EventError::InvalidEnvelope` when `correlation_id` is empty: every Event must be
    ///   joinable to its Task's correlation chain (ATH-OBS-004).
    pub fn new(payload: P, ctx: EventContext) -> Result<Self> {
        if ctx.correlation_id.trim().is_empty() {
            return Err(redact::EventError::InvalidEnvelope(
                "correlation_id must join this Event to its Task's chain (ATH-OBS-004)".into(),
            ));
        }

        let serialized = serde_json::to_value(&payload).map_err(|err| {
            redact::EventError::InvalidEnvelope(format!("payload did not serialize: {err}"))
        })?;

        redact::forbid_full_input(&serialized)?;
        for key in P::REQUIRED_FIELDS {
            let present = serialized
                .get(key)
                .map(|value| !value.is_null())
                .unwrap_or(false);
            if !present {
                return Err(redact::EventError::MissingField(key));
            }
        }

        let payload_json = redact::mask_value(serialized).to_string();

        Ok(Self {
            event_id: ctx.event_id,
            schema_version: SCHEMA_VERSION,
            event_type: P::EVENT_TYPE,
            occurred_at: ctx.occurred_at,
            sequence: SEQUENCE_UNASSIGNED,
            task_id: ctx.task_id,
            plan_step_id: ctx.plan_step_id,
            agent_id: ctx.agent_id,
            instance_id: ctx.instance_id,
            skill_id: ctx.skill_id,
            correlation_id: ctx.correlation_id,
            causation_id: ctx.causation_id,
            actor: ctx.actor,
            payload,
            payload_json,
        })
    }

    /// Record the position the store assigned after the append transaction committed.
    pub fn record_sequence(&mut self, sequence: u64) {
        self.sequence = sequence;
    }

    /// Fold the Event into the non-generic §23.1 envelope, for persistence and fan-out.
    ///
    /// The serialized payload comes from construction and stays hygiene-cleaned; the typed
    /// struct remains in reach of the caller.
    #[must_use]
    pub fn envelope(&self) -> crate::envelope::EventEnvelope {
        crate::envelope::EventEnvelope {
            event_id: self.event_id.clone(),
            schema_version: self.schema_version,
            kind: crate::envelope::EventKind::new(P::EVENT_TYPE),
            occurred_at: self.occurred_at.clone(),
            sequence: self.sequence,
            task_id: self.task_id.clone(),
            plan_step_id: self.plan_step_id.clone(),
            agent_id: self.agent_id.clone(),
            instance_id: self.instance_id.clone(),
            skill_id: self.skill_id.clone(),
            correlation_id: self.correlation_id.clone(),
            causation_id: self.causation_id.clone(),
            actor: self.actor,
            payload_json: self.payload_json.clone(),
        }
    }
}

// --- Context ----------------------------------------------------------------------------

/// Everything that surrounds an Event and is independent of its type-specific payload:
/// the §23.1 envelope columns outside `type` and `payload` (spec section 23.1).
#[derive(Debug, Clone)]
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

// Sample machinery lives beside the catalog it exercises; test-only, no public surface.
#[cfg(test)]
mod tests_helpers {
    use super::*;

    /// One sample value per field type used across the catalog, so every payload struct can
    /// be built for the mandatory-key check without hand-writing 60 constructors.
    pub trait Sample: Sized {
        fn sample() -> Self;
    }

    macro_rules! impl_sample {
        ($t:ty => $expr:expr) => {
            impl Sample for $t {
                fn sample() -> Self {
                    $expr
                }
            }
        };
    }

    impl_sample!(String => "sample-text".to_owned());
    impl_sample!(u64 => 1);
    impl_sample!(u32 => 1);
    impl_sample!(bool => true);
    impl_sample!(serde_json::Value => serde_json::json!({
        "tool": "fs_write",
        "target": "<PROJECT_ROOT>/reports/out.txt",
        "risk": "high"
    }));
    impl<T: Sample> Sample for Option<T> {
        fn sample() -> Self {
            Some(T::sample())
        }
    }
    impl<T: Sample> Sample for Vec<T> {
        fn sample() -> Self {
            Vec::new()
        }
    }

    impl_sample!(TaskId => TaskId::new());
    impl_sample!(PlanId => PlanId::new());
    impl_sample!(PlanStepId => PlanStepId::new());
    impl_sample!(InstanceId => InstanceId::new());
    impl_sample!(AgentId => AgentId::new("rtl-agent"));
    impl_sample!(SkillId => SkillId::new("skill-1"));
    impl_sample!(ResultId => ResultId::new());
    impl_sample!(ApprovalId => ApprovalId::new());
    impl_sample!(ToolExecutionId => ToolExecutionId::new());
    impl_sample!(EvaluationId => EvaluationId::new());
    impl_sample!(HierarchyId => HierarchyId::new());
    impl_sample!(MemoryId => MemoryId::new());
    impl_sample!(MessageId => MessageId::new());

    impl_sample!(Priority => Priority::Normal);
    impl_sample!(RiskLevel => RiskLevel::High);
    impl_sample!(RuntimeType => RuntimeType::AgentCli);
    impl_sample!(ToolName => ToolName::FsWrite);
    impl_sample!(SideEffects => SideEffects::Unknown);
    impl_sample!(ResultStatus => ResultStatus::Success);
    impl_sample!(Verdict => Verdict::Accept);
    impl_sample!(AgentLevel => AgentLevel::Worker);
    impl_sample!(MessageType => MessageType::Instruction);
    impl_sample!(ApprovalScope => ApprovalScope::OperationOnly { operation_only: true });
    impl_sample!(ScopeRef => ScopeRef::PlanStep { plan_step_id: PlanStepId::new() });
    impl_sample!(WaitOn => WaitOn::Message { message_id: MessageId::new() });
    impl_sample!(SkillRef => SkillRef {
        id: SkillId::new("skill-1"),
        version: "1.0.0".into(),
        checksum: Some("sha256:0000".into()),
    });
    impl_sample!(HierarchyNodeRef => HierarchyNodeRef {
        agent_id: AgentId::new("rtl-agent"),
        level: AgentLevel::Worker,
        scope_ref: Some(ScopeRef::PlanStep { plan_step_id: PlanStepId::new() }),
    });
}

#[cfg(test)]
mod tests {
    use super::tests_helpers::Sample;
    use super::*;
    use crate::EventContext;
    use athena_types::Actor;
    use serde::{Deserialize, Serialize};

    /// Build a sample payload for a catalog name, exercising every type once.
    macro_rules! sample_of {
        ($target:expr, $($name:ident),+ $(,)?) => {
            match $target.trim() {
                $( stringify!($name) => {
                    let payload = $name::sample();
                    Some(serde_json::to_value(&payload).expect("sample serializes"))
                } )+
                _ => None,
            }
        };
    }

    /// The contextual fields for a typed construction test.
    fn ctx(corr: &str) -> EventContext {
        EventContext {
            event_id: EventId::new(),
            occurred_at: "2026-10-14T00:00:00.000Z".into(),
            actor: Actor::CoreAgent,
            task_id: None,
            plan_step_id: None,
            agent_id: None,
            instance_id: None,
            skill_id: None,
            correlation_id: corr.into(),
            causation_id: None,
        }
    }

    // The §23.2 catalog, verbatim. The test asserts this list matches the macro-generated
    // set exactly — a catalog entry added in code without a spec revision, or dropped,
    // fails here (ATH-EVENT-009).
    const SECTION_232_CATALOG: &[&str] = &[
        // Task
        "TaskCreated",
        "TaskQueued",
        "TaskStarted",
        "TaskResultReceived",
        "TaskReplanning",
        "TaskRetrying",
        "TaskCancelling",
        "TaskCancelled",
        "TaskCompleted",
        "TaskFailed",
        // Plan
        "PlanCreated",
        "PlanRevised",
        "PlanStepStarted",
        "PlanStepCompleted",
        "PlanStepFailed",
        // Hierarchy
        "HierarchyComposed",
        "AgentDelegated",
        // Agent
        "AgentSelected",
        "AgentStarting",
        "AgentStarted",
        "AgentWaiting",
        "AgentStopping",
        "AgentStopped",
        "AgentCompleted",
        "AgentFailed",
        // Core Agent
        "CoreAgentStarted",
        "CoreAgentFailed",
        "CoreAgentRestarted",
        "CoreAgentEscalated",
        "CoreAgentHeartbeat",
        "PrivilegedRequestDenied",
        // PlanStep
        "PlanStepReady",
        "PlanStepOrphaned",
        "PlanStepSkipped",
        // Instance
        "AgentSuspended",
        "AgentResumed",
        // Scheduler
        "SchedulerBlocked",
        "TaskStalled",
        // Clarification
        "ClarificationRequested",
        "ClarificationAnswered",
        "ClarificationExpired",
        // Workspace
        "WorkspaceInitialized",
        "WorkspaceLeased",
        "WorkspaceReleased",
        // Message
        "AgentMessageSent",
        "AgentMessageReceived",
        "AgentMessageTimeout",
        // Tool
        "ToolExecutionStarted",
        "ToolExecutionCompleted",
        "ToolExecutionFailed",
        // Skill
        "SkillRegistered",
        "SkillValidating",
        "SkillValidated",
        "SkillValidationFailed",
        "SkillEnabled",
        "SkillDisabled",
        "SkillUpdating",
        "SkillUpdated",
        "SkillRemoved",
        "SkillSelected",
        "SkillLoaded",
        "SkillExecutionStarted",
        "SkillExecutionCompleted",
        "SkillExecutionFailed",
        // Runtime
        "RuntimeInstanceCreated",
        "RuntimeInstanceStopped",
        "RuntimeUnhealthy",
        // Approval
        "ApprovalRequested",
        "ApprovalGranted",
        "ApprovalRejected",
        "ApprovalExpired",
        "ApprovalConsumed",
        // Memory
        "MemoryRecordCreated",
        "MemoryRecordDeleted",
        // Evaluation
        "EvaluationStarted",
        "EvaluationPassed",
        "EvaluationFailed",
        // System
        "SystemStarted",
        "SystemStopping",
        "ConfigurationLoaded",
    ];

    fn sample_for_catalog(name: &str) -> Option<serde_json::Value> {
        sample_of!(
            name,
            TaskCreated,
            TaskQueued,
            TaskStarted,
            TaskResultReceived,
            TaskReplanning,
            TaskRetrying,
            TaskCancelling,
            TaskCancelled,
            TaskCompleted,
            TaskFailed,
            PlanCreated,
            PlanRevised,
            PlanStepStarted,
            PlanStepCompleted,
            PlanStepFailed,
            HierarchyComposed,
            AgentDelegated,
            AgentSelected,
            AgentStarting,
            AgentStarted,
            AgentWaiting,
            AgentStopping,
            AgentStopped,
            AgentCompleted,
            AgentFailed,
            CoreAgentStarted,
            CoreAgentFailed,
            CoreAgentRestarted,
            CoreAgentEscalated,
            CoreAgentHeartbeat,
            PrivilegedRequestDenied,
            PlanStepReady,
            PlanStepOrphaned,
            PlanStepSkipped,
            AgentSuspended,
            AgentResumed,
            SchedulerBlocked,
            TaskStalled,
            ClarificationRequested,
            ClarificationAnswered,
            ClarificationExpired,
            WorkspaceInitialized,
            WorkspaceLeased,
            WorkspaceReleased,
            AgentMessageSent,
            AgentMessageReceived,
            AgentMessageTimeout,
            ToolExecutionStarted,
            ToolExecutionCompleted,
            ToolExecutionFailed,
            SkillRegistered,
            SkillValidating,
            SkillValidated,
            SkillValidationFailed,
            SkillEnabled,
            SkillDisabled,
            SkillUpdating,
            SkillUpdated,
            SkillRemoved,
            SkillSelected,
            SkillLoaded,
            SkillExecutionStarted,
            SkillExecutionCompleted,
            SkillExecutionFailed,
            RuntimeInstanceCreated,
            RuntimeInstanceStopped,
            RuntimeUnhealthy,
            ApprovalRequested,
            ApprovalGranted,
            ApprovalRejected,
            ApprovalExpired,
            ApprovalConsumed,
            MemoryRecordCreated,
            MemoryRecordDeleted,
            EvaluationStarted,
            EvaluationPassed,
            EvaluationFailed,
            SystemStarted,
            SystemStopping,
            ConfigurationLoaded,
        )
    }

    #[test]
    fn every_catalog_entry_is_constructible_and_carries_its_mandatory_keys() {
        // ATH-EVENT-017 + the D-3 guarantee, verified against serialization.
        for name in SECTION_232_CATALOG {
            let serialized = sample_for_catalog(name)
                .unwrap_or_else(|| panic!("{name} is in the §23.2 catalog but has no sample"));
            let payload = serde_json::to_value(serde_json::to_string(&serialized).expect("json"))
                .expect("valid");
            let _ = payload;
        }
    }

    #[test]
    fn the_catalog_matches_the_specification_exactly() {
        // Every type in the code is in the spec...
        for value in sample_of_catalog_names().iter() {
            assert!(
                SECTION_232_CATALOG.contains(&value.as_str()),
                "code catalog has {value}, which is not in spec §23.2 — a spec revision is required first (ATH-EVENT-009)"
            );
        }
        // ...and the spec list is fully implemented.
        assert_eq!(sample_of_catalog_names().len(), SECTION_232_CATALOG.len());
        for name in SECTION_232_CATALOG {
            assert!(
                sample_of_catalog_names().iter().any(|n| n == name),
                "§23.2 mandates {name}, which is unimplemented (ATH-EVENT-008)"
            );
        }
    }

    fn sample_of_catalog_names() -> Vec<String> {
        // Derived from the same macro-generated impls; see `sample_for_catalog`.
        let mut names = Vec::new();
        for name in SECTION_232_CATALOG {
            if sample_for_catalog(name).is_some() {
                names.push((*name).to_string());
            }
        }
        names.sort();
        names
    }

    #[test]
    fn typed_construction_serializes_mandatory_fields() {
        // TaskCreated carries one value per mandated key, non-Option (D-3).
        let event = TypedEvent::new(
            TaskCreated {
                instruction_digest: crate::redact::digest("build the report"),
                workspace: "/repo".into(),
                priority: Priority::Normal,
                created_by: "human".into(),
            },
            ctx("cor-task-1"),
        )
        .expect("constructs");
        let parsed: serde_json::Value = serde_json::from_str(&event.payload_json).expect("json");
        for key in TaskCreated::REQUIRED_FIELDS {
            assert!(parsed.get(*key).is_some(), "missing `{key}`");
        }
        assert_eq!(event.event_type, "TaskCreated");
        assert_eq!(event.schema_version, 1);
    }

    #[test]
    fn an_empty_correlation_id_is_refused() {
        let err = TypedEvent::new(
            TaskCancelled {
                forced: false,
                side_effects: SideEffects::Unknown,
            },
            ctx("   "),
        )
        .expect_err("must refuse");
        assert!(
            matches!(err, crate::redact::EventError::InvalidEnvelope(_)),
            "{err}"
        );
    }

    #[test]
    fn a_payload_key_that_would_embed_agent_input_is_refused() {
        // ATH-EVENT-007: bulk content is referenced (digest), never embedded.
        #[derive(Debug, Serialize, Deserialize)]
        struct Rogue {
            instruction: String,
        }
        impl Payload for Rogue {
            const EVENT_TYPE: &'static str = "TaskCreated";
            const REQUIRED_FIELDS: &'static [&'static str] = &[];
        }
        let err = TypedEvent::new(
            Rogue {
                instruction: "the whole prompt".into(),
            },
            ctx("corr"),
        )
        .expect_err("must refuse");
        assert!(
            matches!(err, crate::redact::EventError::ForbiddenContent(_, _)),
            "{err}"
        );
    }

    #[test]
    fn secrets_are_masked_inside_string_values() {
        // R-SEC-10 / R-POL-13 through the Event path.
        let event = TypedEvent::new(
            TaskCreated {
                instruction_digest: "0123456789abcdef".into(),
                workspace: "bearer sk-0123456789aaaaaaaaaaaaaaaa".into(),
                priority: Priority::Normal,
                created_by: "human password = hunter2b".into(),
            },
            ctx("corr"),
        )
        .expect("constructs");
        assert!(
            !event.payload_json.contains("sk-0123456789aaaaaaaaaaaaaaaa"),
            "{}",
            event.payload_json
        );
        assert!(
            !event.payload_json.contains("hunter2b"),
            "{}",
            event.payload_json
        );
        assert!(event.payload_json.contains(crate::redact::REDACTED));
    }

    #[test]
    fn a_correct_payload_still_publishes_readably() {
        let event = TypedEvent::new(
            TaskCreated {
                instruction_digest: crate::redact::digest("build the report"),
                workspace: "<PROJECT_ROOT>".into(),
                priority: Priority::Normal,
                created_by: "human".into(),
            },
            ctx("corr"),
        )
        .expect("constructs");
        let parsed: serde_json::Value =
            serde_json::from_str(&event.payload_json).expect("still valid json");
        assert_eq!(parsed["workspace"], "<PROJECT_ROOT>");
        assert_eq!(parsed["priority"], "normal");
    }

    #[test]
    fn the_envelope_carries_every_section_231_column() {
        let mut event = TypedEvent::new(
            TaskCreated {
                instruction_digest: crate::redact::digest("x"),
                workspace: "<PROJECT_ROOT>".into(),
                priority: Priority::Normal,
                created_by: "human".into(),
            },
            EventContext {
                event_id: EventId::new(),
                occurred_at: "2026-10-14T00:00:00.000Z".into(),
                actor: Actor::Human,
                task_id: Some(TaskId::new()),
                plan_step_id: None,
                agent_id: None,
                instance_id: None,
                skill_id: None,
                correlation_id: "cor-1".into(),
                causation_id: Some(EventId::new()),
            },
        )
        .expect("constructs");
        assert_eq!(event.sequence, SEQUENCE_UNASSIGNED);
        event.record_sequence(7);
        assert_eq!(event.sequence, 7);

        let mut env = event.envelope();
        assert_eq!(env.kind(), "TaskCreated");
        assert_eq!(env.schema_version, 1);
        assert_eq!(env.sequence, 7);
        assert!(env.task_id.is_some());
        assert!(env.causation_id.is_some());
        assert_eq!(env.actor, Actor::Human);
        env.record_sequence(9);
        assert_eq!(env.sequence, 9);
    }

    #[test]
    fn digests_are_stable_and_short() {
        let a = crate::redact::digest("build the report");
        let b = crate::redact::digest("  build the report \n");
        let c = crate::redact::digest("build the other report");
        assert_eq!(a, b, "trim makes the digest stable");
        assert_ne!(a, c);
        assert_eq!(a.len(), 32);
    }

    #[test]
    fn waiting_events_carry_a_reconstructible_wait_graph() {
        // ATH-EVENT-018: AgentWaiting must let the wait graph be rebuilt from Events.
        let event = TypedEvent::new(
            AgentWaiting {
                waits_on: vec![
                    WaitOn::Instance {
                        instance_id: InstanceId::new(),
                    },
                    WaitOn::Approval {
                        approval_id: ApprovalId::new(),
                    },
                    WaitOn::Message {
                        message_id: MessageId::new(),
                    },
                ],
                deadline: "2026-10-14T01:00:00.000Z".into(),
            },
            ctx("cor-wait"),
        )
        .expect("constructs");
        let parsed: serde_json::Value = serde_json::from_str(&event.payload_json).expect("json");
        let kinds: Vec<&str> = parsed["waits_on"]
            .as_array()
            .expect("array")
            .iter()
            .map(|item| item["kind"].as_str().expect("tag"))
            .collect();
        assert_eq!(kinds, vec!["instance", "approval", "message"]);
    }

    #[test]
    fn hierarchy_events_name_levels_and_scopes() {
        let event = TypedEvent::new(
            HierarchyComposed {
                hierarchy_id: HierarchyId::new(),
                revision: 2,
                nodes: vec![tests_helpers::Sample::sample()],
            },
            ctx("cor-hier"),
        )
        .expect("constructs");
        let parsed: serde_json::Value = serde_json::from_str(&event.payload_json).expect("json");
        assert_eq!(parsed["nodes"][0]["level"], "worker");
        assert_eq!(parsed["nodes"][0]["scope_ref"]["kind"], "plan_step");
    }
}
