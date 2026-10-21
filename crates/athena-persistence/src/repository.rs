//! Repository interfaces (ATH-PERS-007).
//!
//! Core depends on these traits and never on SQL, so the store can be replaced without
//! touching a caller (ATH-PERS-006). One trait per aggregate, carrying the operations Core
//! actually performs rather than blanket CRUD: a narrow surface is what keeps the
//! entity-ownership rule of spec section 26.2 enforceable.

use crate::error::Result;
use athena_types::{
    AgentDefinition, AgentId, AgentInstance, AgentInstanceState, Approval, ApprovalId,
    ApprovalState, Artifact, Clarification, ClarificationId, Conversation, Evaluation,
    EvaluationId, EventId, ExecutionResult, Hierarchy, HierarchyId, InstanceId, MemoryId,
    MemoryRecord, MemoryTier, Plan, PlanId, PlanStep, PlanStepId, PlanStepState, ResultId,
    RuntimeInstance, RuntimeInstanceId, Skill, SkillId, Task, TaskId, TaskState, ToolExecution,
    ToolExecutionId, Utterance,
};

/// A stored Event (spec section 23.1).
///
/// Defined here rather than in `athena-events` because `athena-persistence` must not depend on
/// it: the permitted dependency direction is `athena-core -> athena-persistence` and
/// `athena-core -> athena-events`, with no edge between the two (spec section 32.2).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StoredEvent {
    /// Identifier.
    pub event_id: EventId,
    /// Payload schema version (ATH-EVENT-006).
    pub schema_version: u64,
    /// Type name, from the section 23.2 catalog.
    pub event_type: String,
    /// When it happened.
    pub occurred_at: String,
    /// Monotonic position, assigned by the store (ATH-EVENT-003).
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
    pub actor: String,
    /// Payload as JSON text.
    pub payload_json: String,
}

/// An Event about to be appended, before the store assigns its position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEvent {
    /// Identifier, minted by Core Host (ATH-CORE-041).
    pub event_id: EventId,
    /// Payload schema version.
    pub schema_version: u64,
    /// Type name.
    pub event_type: String,
    /// When it happened.
    pub occurred_at: String,
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
    /// Correlation identifier.
    pub correlation_id: String,
    /// Causing Event.
    pub causation_id: Option<EventId>,
    /// Who caused it.
    pub actor: String,
    /// Payload as JSON text. Must carry no secret and no full agent input
    /// (ATH-EVENT-007).
    pub payload_json: String,
}

/// A cursor-paginated page (ATH-API-005).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    /// The items.
    pub items: Vec<T>,
    /// Cursor for the next page, absent at the end.
    pub next_cursor: Option<String>,
}

/// Tasks (spec section 26.2). Mutated by the State Manager alone.
pub trait TaskRepository {
    /// Insert a new Task.
    fn insert(&mut self, task: &Task) -> Result<()>;
    /// Whether the id exists.
    fn exists(&self, id: &TaskId) -> Result<bool> {
        Ok(self.get(id)?.is_some())
    }
    /// Every task row, in creation order, newest first (the API list walks this).
    fn list_all(&self) -> Result<Vec<Task>>;
    /// One cursor page served directly by the store, newest first (`nth` stable-ordered
    /// by insertion rowid). The list endpoint answers from this instead of
    /// materializing the whole table, so deep pages cost a LIMIT/OFFSET scan, not a
    /// full hydration (NFR-02's p95 budget at 10,000 Tasks).
    fn list_page(&self, offset: u64, limit: u64) -> Result<Vec<Task>> {
        // The natural face of `list_all` chopped in memory — drivers with a real
        // backend override this; the default keeps the contract meaningful.
        Ok(self
            .list_all()?
            .into_iter()
            .skip(offset as usize)
            .take(limit as usize)
            .collect())
    }
    /// Fetch by identifier.
    fn get(&self, id: &TaskId) -> Result<Option<Task>>;
    /// List by state, newest first.
    fn list_by_state(&self, state: TaskState, limit: u64) -> Result<Vec<Task>>;
    /// List every Task not in a terminal state.
    ///
    /// This is the query crash recovery runs: non-terminal Tasks are restored from the store
    /// and resumed (ATH-NFR-003).
    fn list_non_terminal(&self) -> Result<Vec<Task>>;
    /// Child Tasks of a parent.
    fn list_children(&self, parent: &TaskId) -> Result<Vec<Task>>;
    /// Replace a Task's mutable fields.
    fn update(&mut self, task: &Task) -> Result<()>;
}

/// Plans and their steps (spec section 16.3). A revision is never overwritten.
pub trait PlanRepository {
    /// Insert a Plan together with its steps and dependency edges.
    fn insert(&mut self, plan: &Plan) -> Result<()>;
    /// Fetch a Plan with its steps.
    fn get(&self, id: &PlanId) -> Result<Option<Plan>>;
    /// The highest revision for a Task.
    fn latest_for_task(&self, task: &TaskId) -> Result<Option<Plan>>;
    /// A specific revision.
    fn revision_for_task(&self, task: &TaskId, revision: u64) -> Result<Option<Plan>>;
    /// Every revision for a Task, ascending.
    fn revisions_for_task(&self, task: &TaskId) -> Result<Vec<u64>>;
    /// Update one step's state.
    fn update_step_state(&mut self, step: &PlanStepId, state: PlanStepState) -> Result<()>;
    /// Fetch one step.
    fn get_step(&self, step: &PlanStepId) -> Result<Option<PlanStep>>;
    /// Record an attempt against a step (ATH-FAIL-007).
    fn record_attempt(
        &mut self,
        step: &PlanStepId,
        attempt: u64,
        instance: Option<&InstanceId>,
    ) -> Result<()>;
}

/// Events (spec section 23). Append-only (ATH-PERS-008).
pub trait EventRepository {
    /// Append an Event, assigning the next sequence number.
    fn append(&mut self, event: &NewEvent) -> Result<StoredEvent>;
    /// Fetch by identifier.
    fn get(&self, id: &EventId) -> Result<Option<StoredEvent>>;
    /// Events of a Task in chronological order, for replay (ATH-EVENT-012).
    fn list_for_task(&self, task: &TaskId) -> Result<Vec<StoredEvent>>;
    /// Events after a sequence position, so a reconnecting subscriber can fetch what it
    /// missed (ATH-EVENT-020).
    fn list_since(&self, sequence: u64, limit: u64) -> Result<Vec<StoredEvent>>;
    /// The highest assigned sequence.
    fn max_sequence(&self) -> Result<u64>;
    /// Delete Events older than a cutoff.
    ///
    /// The only permitted deletion (ATH-PERS-008), and it must preserve each Task's final
    /// result and Results (ATH-EVENT-014) — which holds because Results live in their own
    /// table and are not swept here.
    fn prune_before(&mut self, cutoff_rfc3339: &str) -> Result<u64>;
}

/// Idempotency keys (ATH-CORE-035).
pub trait IdempotencyRepository {
    /// Claim a key, returning `false` when it was already claimed.
    ///
    /// This is the guard against a double delegation after a Core Agent restart
    /// (ATH-NFR-017).
    fn claim(&mut self, key: &str) -> Result<bool>;
    /// Whether a key has been claimed.
    fn is_claimed(&self, key: &str) -> Result<bool>;
    /// Record the outcome against a claimed key, so a repeat can return the same result
    /// (ATH-API-004).
    fn record_outcome(&mut self, key: &str, outcome: &str) -> Result<()>;
    /// The recorded outcome, if any.
    fn outcome(&self, key: &str) -> Result<Option<String>>;
    /// Delete keys older than a cutoff (default retention 7 days).
    fn prune_before(&mut self, cutoff_rfc3339: &str) -> Result<u64>;
}

/// Workspace leases (ATH-DATA-009).
pub trait WorkspaceLeaseRepository {
    /// Acquire a lease, returning `false` when another Task holds it.
    ///
    /// A `false` here is what the API turns into 409 (ATH-API-023).
    fn acquire(&mut self, workspace: &str, task: &TaskId) -> Result<bool>;
    /// Release a lease.
    fn release(&mut self, workspace: &str) -> Result<()>;
    /// The Task holding a workspace.
    fn holder(&self, workspace: &str) -> Result<Option<TaskId>>;
}

/// Agent definitions and instances (spec section 26.3).
pub trait AgentRepository {
    /// Register or replace a definition.
    fn upsert_definition(&mut self, def: &AgentDefinition) -> Result<()>;
    /// Fetch a definition.
    fn get_definition(&self, id: &AgentId) -> Result<Option<AgentDefinition>>;
    /// Every definition.
    fn list_definitions(&self) -> Result<Vec<AgentDefinition>>;
    /// Insert an instance.
    fn insert_instance(&mut self, instance: &AgentInstance) -> Result<()>;
    /// Fetch an instance.
    fn get_instance(&self, id: &InstanceId) -> Result<Option<AgentInstance>>;
    /// Update an instance's state.
    fn update_instance_state(&mut self, id: &InstanceId, state: AgentInstanceState) -> Result<()>;
    /// Instances of a Task.
    fn list_instances_for_task(&self, task: &TaskId) -> Result<Vec<AgentInstance>>;
    /// Instances not in a terminal state.
    ///
    /// Startup reconciliation uses this to find instances whose process no longer exists and
    /// finalize them as `FAILED` with unknown side effects (ATH-CORE-048).
    fn list_non_terminal_instances(&self) -> Result<Vec<AgentInstance>>;
    /// Non-terminal instances for a PlanStep, which must be empty before a new delegation
    /// (ATH-DATA-010).
    fn count_non_terminal_for_step(&self, step: &PlanStepId) -> Result<u64>;
    /// Every instance row, any state (the Memory API's short-tier scan needs the full
    /// set: Working Memory and the short tier outlive terminal states).
    fn list_all_instances(&self) -> Result<Vec<AgentInstance>>;
}

/// Results, artifacts and evaluations (spec section 19).
pub trait ResultRepository {
    /// Insert a Result with its artifacts. Immutable once written.
    fn insert(&mut self, result: &ExecutionResult) -> Result<()>;
    /// Fetch a Result.
    fn get(&self, id: &ResultId) -> Result<Option<ExecutionResult>>;
    /// Results of a Task.
    fn list_for_task(&self, task: &TaskId) -> Result<Vec<ExecutionResult>>;
    /// Artifacts of a Result.
    fn list_artifacts(&self, result: &ResultId) -> Result<Vec<Artifact>>;
    /// Record an evaluation.
    fn insert_evaluation(&mut self, evaluation: &Evaluation) -> Result<()>;
    /// Fetch an evaluation.
    fn get_evaluation(&self, id: &EvaluationId) -> Result<Option<Evaluation>>;
    /// Evaluations of a Result.
    fn list_evaluations_for_result(&self, result: &ResultId) -> Result<Vec<Evaluation>>;
}

/// Skills, keyed by `(id, version)` (ATH-STATE-026).
pub trait SkillRepository {
    /// Register or replace a skill version.
    fn upsert(&mut self, skill: &Skill) -> Result<()>;
    /// Fetch one version.
    fn get(&self, id: &SkillId, version: &str) -> Result<Option<Skill>>;
    /// Every version of a skill.
    fn list_versions(&self, id: &SkillId) -> Result<Vec<Skill>>;
    /// Skills in a given state. Only `ENABLED` ones are selection candidates
    /// (ATH-STATE-008).
    fn list_by_state(&self, state: athena_types::SkillState) -> Result<Vec<Skill>>;
    /// Skills offering a capability (ATH-SKILL-006).
    fn search_by_capability(&self, capability: &str) -> Result<Vec<Skill>>;
    /// Remove one version.
    fn delete(&mut self, id: &SkillId, version: &str) -> Result<()>;
}

/// Approvals (spec section 15).
pub trait ApprovalRepository {
    /// Insert a request.
    fn insert(&mut self, approval: &Approval) -> Result<()>;
    /// Fetch an approval.
    fn get(&self, id: &ApprovalId) -> Result<Option<Approval>>;
    /// Approvals in a state.
    fn list_by_state(&self, state: ApprovalState) -> Result<Vec<Approval>>;
    /// Update state, responder and remaining uses.
    fn update_state(
        &mut self,
        id: &ApprovalId,
        state: ApprovalState,
        responded_by: Option<&str>,
        uses_remaining: u64,
    ) -> Result<()>;
    /// Requested approvals past their deadline, which expire as rejections
    /// (ATH-HITL-005).
    fn list_expired(&self, now_rfc3339: &str) -> Result<Vec<Approval>>;
    /// Granted and unconsumed approvals, which must appear in restart context
    /// (ATH-CORE-046).
    fn list_granted_unconsumed(&self) -> Result<Vec<Approval>>;
}

/// Memory records (spec section 13).
pub trait MemoryRepository {
    /// Insert a record.
    fn insert(&mut self, record: &MemoryRecord) -> Result<()>;
    /// Fetch a record.
    fn get(&self, id: &MemoryId) -> Result<Option<MemoryRecord>>;
    /// Records in a tier and scope.
    fn list(&self, tier: MemoryTier, scope_id: &str) -> Result<Vec<MemoryRecord>>;
    /// Every record of one tier, across scopes (the Memory API's unscoped reference face).
    fn list_by_tier(&self, tier: MemoryTier) -> Result<Vec<MemoryRecord>>;
    /// Record count in a tier and scope, for limit enforcement (ATH-MEM-002).
    fn count(&self, tier: MemoryTier, scope_id: &str) -> Result<u64>;
    /// Delete a record. Deletion is recorded as an Event by the caller (ATH-MEM-006).
    fn delete(&mut self, id: &MemoryId) -> Result<()>;
    /// Replace a record's content with a summary, for eviction (ATH-MEM-002).
    fn summarize(&mut self, id: &MemoryId, summary: &str) -> Result<()>;
    /// The provenance query (P8-03, ATH-MEM-005): Long records whose origin anchors
    /// include the given Task and/or Event. `None` on one side means "any".
    fn list_by_provenance(
        &self,
        task: Option<&TaskId>,
        event: Option<&EventId>,
    ) -> Result<Vec<MemoryRecord>>;
}

/// Clarifications (`ATH-HITL-017..020`): a question a human must answer; silence is
/// never an answer.
pub trait ClarificationRepository {
    /// Open a clarification (`requested`).
    fn open(&mut self, clarification: &Clarification) -> Result<()>;
    /// Fetch one.
    fn get(&self, id: &ClarificationId) -> Result<Option<Clarification>>;
    /// The Task's open clarifications (`requested`).
    fn pending_for_task(&self, task: &TaskId) -> Result<Vec<Clarification>>;
    /// One clarification is `still_pending`, its deadline already past.
    fn pending_expired(&self, now_rfc3339: &str) -> Result<Vec<Clarification>>;
    /// Record the answer (`answered`). Silently not-answered is impossible: the store
    /// refuses an answer for a row that is not `requested`.
    fn answer(&mut self, id: &ClarificationId, answer: &str, answered_at: &str) -> Result<()>;
    /// Mark the row expired (the expiry sweep's own record; the Task transition is the
    /// caller's).
    fn mark_expired(&mut self, id: &ClarificationId) -> Result<()>;
}

/// Messages and their delivery records (spec section 10).
pub trait MessageRepository {
    /// Persist a message. The Broker acknowledges only after this succeeds
    /// (ATH-MSG-012).
    fn insert(&mut self, message: &MessageRow) -> Result<()>;
    /// Fetch a message.
    fn get(&self, id: &athena_types::MessageId) -> Result<Option<MessageRow>>;
    /// Messages of a thread, in order (ATH-MSG-004).
    fn list_for_thread(&self, thread_id: &str) -> Result<Vec<MessageRow>>;
    /// Mark a message delivered to an instance, returning `false` when it already was.
    ///
    /// Deduplication lives here rather than in the agent, because agent-side memory cannot be
    /// relied upon (ATH-MSG-014).
    fn mark_delivered(&mut self, to: &InstanceId, id: &athena_types::MessageId) -> Result<bool>;
    /// Mark a delivered message accepted.
    fn mark_accepted(&mut self, to: &InstanceId, id: &athena_types::MessageId) -> Result<()>;
    /// Whether a message was already delivered to an instance.
    fn was_delivered(&self, to: &InstanceId, id: &athena_types::MessageId) -> Result<bool>;
    /// Delete delivery records older than a cutoff (`messaging.dedup_retention`).
    fn prune_deliveries_before(&mut self, cutoff_rfc3339: &str) -> Result<u64>;
}

/// A stored message (spec section 10.2).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MessageRow {
    /// Identifier.
    pub message_id: athena_types::MessageId,
    /// Kind.
    pub message_type: athena_types::MessageType,
    /// Sending agent.
    pub from_agent: AgentId,
    /// Receiving agent definition.
    pub to_agent: AgentId,
    /// Receiving instance. Addressing uses this except for single-instance agents
    /// (ATH-MSG-010).
    pub to_instance_id: Option<InstanceId>,
    /// Sending instance, which identifies the attempt.
    pub from_instance_id: InstanceId,
    /// Associated Task.
    pub task_id: Option<TaskId>,
    /// Thread, issued by Core Host, one per delegation (ATH-MSG-013).
    pub thread_id: String,
    /// Message being replied to.
    pub reply_to: Option<athena_types::MessageId>,
    /// Priority.
    pub priority: athena_types::Priority,
    /// Natural-language body. Carries no plaintext secret (ATH-MSG-008).
    pub body: String,
    /// Structured payload.
    pub payload_json: Option<String>,
    /// Reply deadline (ATH-MSG-005).
    pub deadline: Option<String>,
    /// Creation time.
    pub created_at: String,
}

/// Hierarchies (spec section 8).
pub trait HierarchyRepository {
    /// Insert a hierarchy with its nodes.
    fn insert(&mut self, hierarchy: &Hierarchy) -> Result<()>;
    /// Fetch a hierarchy.
    fn get(&self, id: &HierarchyId) -> Result<Option<Hierarchy>>;
    /// The latest hierarchy for a Task.
    fn latest_for_task(&self, task: &TaskId) -> Result<Option<Hierarchy>>;
}

/// Runtime instances (spec section 14.5).
pub trait RuntimeInstanceRepository {
    /// Insert a runtime instance.
    fn insert(&mut self, instance: &RuntimeInstance) -> Result<()>;
    /// Fetch one.
    fn get(&self, id: &RuntimeInstanceId) -> Result<Option<RuntimeInstance>>;
    /// Record a heartbeat (ATH-CORE-047).
    fn record_heartbeat(&mut self, id: &RuntimeInstanceId, at: &str) -> Result<()>;
    /// Instances whose heartbeat lapsed before a cutoff.
    fn list_stale(&self, cutoff_rfc3339: &str) -> Result<Vec<RuntimeInstance>>;
    /// Mark stopped, recording whether side effects are known (ATH-CORE-048).
    fn mark_stopped(
        &mut self,
        id: &RuntimeInstanceId,
        reason: athena_types::StopReason,
        exit_code: Option<i32>,
        side_effects: athena_types::SideEffects,
    ) -> Result<()>;
}

/// Tool executions (spec section 11).
pub trait ToolExecutionRepository {
    /// Insert an execution record.
    fn insert(&mut self, execution: &ToolExecution) -> Result<()>;
    /// Fetch one.
    fn get(&self, id: &ToolExecutionId) -> Result<Option<ToolExecution>>;
    /// Update its status.
    fn update_status(
        &mut self,
        id: &ToolExecutionId,
        status: athena_types::ToolExecutionStatus,
    ) -> Result<()>;
    /// Executions by an instance.
    fn list_for_instance(&self, instance: &InstanceId) -> Result<Vec<ToolExecution>>;
}

/// Conversations and utterances (ATH-API-021).
pub trait ConversationRepository {
    /// Insert a conversation.
    fn insert(&mut self, conversation: &Conversation) -> Result<()>;
    /// Fetch one.
    fn get(&self, id: &athena_types::ConversationId) -> Result<Option<Conversation>>;
    /// Associate a Task once one is created.
    fn link_task(&mut self, id: &athena_types::ConversationId, task: &TaskId) -> Result<()>;
    /// Append an utterance.
    fn append_utterance(&mut self, utterance: &Utterance) -> Result<()>;
    /// Utterances in order.
    fn list_utterances(
        &self,
        conversation: &athena_types::ConversationId,
    ) -> Result<Vec<Utterance>>;
}
