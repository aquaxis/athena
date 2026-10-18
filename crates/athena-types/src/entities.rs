//! Core entities (spec sections 26.2 to 26.6).
//!
//! Shapes only. Each entity has exactly one Core Host module permitted to mutate it
//! (ATH-DATA-003), so no entity here carries a mutating method; construction and transition
//! belong to the owning module.

use crate::enums::*;
use crate::ids::*;
use serde::{Deserialize, Serialize};

/// RFC 3339 UTC timestamp.
///
/// Held as `String` deliberately: the specification fixes the *representation*
/// (RFC 3339 / UTC) without constraining the implementation's date-time type
/// (spec section 26, ATH-DATA-002).
pub type Timestamp = String;

/// A provider and model pair. Mandatory on every Agent Definition, with no ATHENA-side default
/// (ATH-AGENT-008), so that cost and quality are judgeable from the definition file alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ModelRef {
    /// Provider name, e.g. `claude`.
    pub provider: String,
    /// Model name, e.g. `claude-opus-5`.
    pub name: String,
}

/// A pinned skill reference (spec section 26.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SkillRef {
    /// Skill slug.
    pub id: SkillId,
    /// Semantic version, pinned at agent start (ATH-SKILL-020).
    pub version: String,
    /// Content checksum, recorded in execution history (ATH-SKILL-021).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
}

/// A permission. A superset of `ToolName`, since not every permission is a tool
/// (spec section 26.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Permission {
    /// Permission to use a tool.
    Tool {
        /// The tool.
        tool: ToolName,
    },
    /// Filesystem access within a scope.
    Fs {
        /// Read or write.
        mode: FsMode,
        /// Path scope.
        scope: String,
    },
    /// Network access.
    Network {
        /// Access mode.
        mode: NetworkMode,
        /// Host allow-list when `mode` is `Allowlist`.
        #[serde(skip_serializing_if = "Option::is_none")]
        hosts: Option<Vec<String>>,
    },
    /// Process manipulation.
    Process,
}

/// Startup and execution deadlines in milliseconds (spec section 26.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deadlines {
    /// Startup budget.
    pub startup_ms: u64,
    /// Execution budget. Time spent awaiting a human is excluded (ATH-FAIL-014).
    pub execution_ms: u64,
}

/// Provenance of a memory record (spec section 26.1, ATH-MEM-005).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MemoryOrigin {
    /// Originating Task.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    /// Originating Event.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<EventId>,
}

/// A unit of requested work (spec section 26.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    /// Identifier.
    pub id: TaskId,
    /// Parent Task, for SubTasks (ATH-TASK-001).
    pub parent_id: Option<TaskId>,
    /// Human-facing title.
    pub title: String,
    /// The human's natural-language input, verbatim.
    ///
    /// Never replaced by a summary (ATH-DATA-004).
    pub instruction: String,
    /// Absolute path of the Agent Workspace, leased exclusively per Task (ATH-DATA-009).
    pub workspace: String,
    /// Core Agent's interpretation of the objective.
    pub goal: Option<String>,
    /// Constraints extracted during Understand.
    pub constraints: Vec<String>,
    /// Completion criteria. Execution must not begin on a guess when these cannot be
    /// extracted (ATH-CORE-014).
    pub completion_criteria: Vec<String>,
    /// Current state.
    pub state: TaskState,
    /// Scheduling priority.
    pub priority: Priority,
    /// Current Plan.
    pub plan_id: Option<PlanId>,
    /// Assessed risk.
    pub risk_level: RiskLevel,
    /// Attempts so far.
    pub attempt_count: u64,
    /// Attempt ceiling (ATH-FAIL-005).
    pub max_attempts: u64,
    /// Agent Loop iterations so far (ATH-CORE-013).
    pub iteration_count: u64,
    /// Iteration ceiling; reaching it escalates to a human.
    pub max_iterations: u64,
    /// Optional deadline.
    pub deadline: Option<Timestamp>,
    /// Submitting user.
    pub created_by: String,
    /// Creation time.
    pub created_at: Timestamp,
    /// Last update time.
    pub updated_at: Timestamp,
    /// Completion time, once terminal.
    pub finished_at: Option<Timestamp>,
}

/// An execution plan (spec section 16.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// Identifier.
    pub id: PlanId,
    /// Owning Task.
    pub task_id: TaskId,
    /// Revision, incremented by Core Host alone (ATH-CORE-042).
    ///
    /// Old revisions are retained; a Plan is never overwritten destructively (ATH-PLAN-004).
    pub revision: u64,
    /// Objective.
    pub goal: String,
    /// Steps.
    pub steps: Vec<PlanStep>,
    /// Creation time.
    pub created_at: Timestamp,
    /// Superseding Plan, if any.
    pub superseded_by: Option<PlanId>,
}

/// One unit of action within a Plan, and the Worker's unit of work
/// (spec sections 16.3 and 8.2.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    /// Identifier.
    pub id: PlanStepId,
    /// Owning Plan.
    pub plan_id: PlanId,
    /// Ordering hint.
    pub order: u64,
    /// What this step achieves.
    pub intent: String,
    /// Prerequisite steps. Must be acyclic, validated at creation (ATH-PLAN-006).
    pub depends_on: Vec<PlanStepId>,
    /// Assigned agent.
    pub assigned_agent: Option<AgentId>,
    /// Resolved skills.
    pub skill_set: Vec<SkillRef>,
    /// Current state.
    pub state: PlanStepState,
    /// Produced Result.
    pub result_id: Option<ResultId>,
}

/// A draft step as submitted by Core Agent.
///
/// Carries no identifier: identifiers are Core Host's to issue (ATH-CORE-041), and omitting the
/// field makes an agent-minted identifier structurally impossible rather than merely rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PlanStepDraft {
    /// Ordering hint.
    pub order: u64,
    /// What this step achieves.
    pub intent: String,
    /// Prerequisites, referenced by `order` within the same submission.
    #[serde(default)]
    pub depends_on_orders: Vec<u64>,
    /// Proposed agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assigned_agent: Option<AgentId>,
    /// Proposed skills.
    #[serde(default)]
    pub skill_set: Vec<SkillRef>,
}

/// A static agent definition (spec section 26.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDefinition {
    /// Identifier slug.
    pub id: AgentId,
    /// Display name.
    pub name: String,
    /// Hierarchy level.
    pub level: AgentLevel,
    /// Description.
    pub description: String,
    /// Declared capabilities, matched against Task requirements (ATH-AGENT-004).
    pub capabilities: Vec<String>,
    /// Persona file reference.
    pub persona_ref: String,
    /// Persona version, recorded in history (ATH-PERSONA-005).
    pub persona_version: String,
    /// Tools this agent may execute. Minimal by default (ATH-SEC-008).
    pub allowed_tools: Vec<ToolName>,
    /// Tools this agent may grant to subordinates. Empty by default and stated explicitly
    /// (ATH-SEC-024); this is independent of what the agent may run itself.
    pub delegable_tools: Vec<ToolName>,
    /// Default skills.
    pub default_skills: Vec<SkillRef>,
    /// Runtime kind.
    pub runtime: RuntimeType,
    /// Provider and model. Mandatory (ATH-AGENT-008).
    pub model: ModelRef,
    /// Delegation depth ceiling (ATH-HIER-005).
    pub max_delegation_depth: u64,
    /// Whether this definition was generated dynamically (ATH-HIER-008).
    pub dynamic: bool,
}

/// A running agent individual (spec section 26.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInstance {
    /// Identifier.
    pub id: InstanceId,
    /// Originating definition.
    pub agent_id: AgentId,
    /// Owning Task. `None` for the Core Agent Instance, which spans Tasks (ATH-STATE-012).
    pub task_id: Option<TaskId>,
    /// Assigned PlanStep.
    pub plan_step_id: Option<PlanStepId>,
    /// Superior instance.
    pub superior_instance_id: Option<InstanceId>,
    /// Delegation depth, checked against the ceiling (ATH-DATA-005).
    pub delegation_depth: u64,
    /// Current state.
    pub state: AgentInstanceState,
    /// Backing Runtime Instance.
    pub runtime_instance_id: Option<RuntimeInstanceId>,
    /// Workspace, which is this instance's cwd (ATH-OPS-009).
    pub workspace: String,
    /// Skills, frozen at start and never changed afterwards (ATH-DATA-006, ATH-SKILL-010).
    pub skill_set: Vec<SkillRef>,
    /// Reference to this instance's aiprj work log (ATH-EVENT-016, ATH-OBS-010).
    pub aiprj_log_ref: Option<String>,
    /// Attempt number; each attempt is an independent instance (ATH-FAIL-007).
    pub attempt: u64,
    /// Start time.
    pub started_at: Option<Timestamp>,
    /// Finish time.
    pub finished_at: Option<Timestamp>,
}

/// An agent's returned work product (spec section 19.1). Immutable (ATH-RESULT-003).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionResult {
    /// Identifier.
    pub id: ResultId,
    /// Owning Task.
    pub task_id: TaskId,
    /// Originating PlanStep.
    pub plan_step_id: Option<PlanStepId>,
    /// Reporting agent.
    pub agent_id: AgentId,
    /// Reporting instance.
    pub instance_id: InstanceId,
    /// Self-declared status. Never sufficient alone for acceptance (ATH-EVAL-005).
    pub status: ResultStatus,
    /// Output text.
    pub output: String,
    /// Attached artifacts.
    pub artifacts: Vec<Artifact>,
    /// Log reference.
    pub logs_ref: Option<String>,
    /// Skills used, for reproducibility (ATH-SKILL-021).
    pub skills_used: Vec<SkillRef>,
    /// Model used.
    pub model: Option<ModelRef>,
    /// Start time.
    pub started_at: Timestamp,
    /// Finish time.
    pub finished_at: Timestamp,
    /// Duration.
    pub execution_time_ms: u64,
    /// Free-form metadata.
    pub metadata: serde_json::Value,
}

/// A deliverable attached to a Result (spec section 26.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    /// Owning Result (ATH-DATA-012).
    pub result_id: ResultId,
    /// Kind.
    pub kind: ArtifactKind,
    /// Path base. Only `work_target` or `store` (ATH-RESULT-002).
    pub base: ArtifactBase,
    /// Path relative to `base`.
    pub path: String,
    /// Size.
    pub bytes: Option<u64>,
    /// Checksum.
    pub checksum: Option<String>,
    /// Creation time.
    pub created_at: Timestamp,
}

/// A registered skill, keyed by `(id, version)` (spec sections 26.5 and 12.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skill {
    /// Slug.
    pub id: SkillId,
    /// Name.
    pub name: String,
    /// Semantic version (ATH-SKILL-019).
    pub version: String,
    /// Description.
    pub description: String,
    /// Capabilities offered.
    pub capabilities: Vec<String>,
    /// Tools required.
    pub required_tools: Vec<ToolName>,
    /// Permissions required, checked as a union over the dependency closure
    /// (ATH-SKILL-031, ATH-SEC-012).
    pub required_permissions: Vec<Permission>,
    /// Agents this skill supports.
    pub supported_agents: Vec<AgentId>,
    /// Dependencies, as SemVer ranges (ATH-SKILL-028).
    pub dependencies: Vec<SkillRef>,
    /// Runtime provider.
    pub runtime_provider: String,
    /// Origin.
    pub source: String,
    /// Checksum. Verified before registration, but never the sole basis of trust
    /// (ATH-SKILL-035).
    pub checksum: String,
    /// Publisher, checked against the allow-list.
    pub publisher: String,
    /// Risk level; `high` requires approval to use (ATH-SKILL-024).
    pub risk_level: RiskLevel,
    /// Lifecycle state.
    pub state: SkillState,
    /// Whether hot reload is declared safe (ATH-SKILL-011).
    pub hot_reload_safe: bool,
    /// Registration time.
    pub registered_at: Timestamp,
    /// Last update time.
    pub updated_at: Timestamp,
}

/// The scope of an approval (spec sections 15.3 and 26.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ApprovalScope {
    /// Valid for exactly one operation (ATH-STATE-010).
    OperationOnly {
        /// Always true; discriminates the variant on the wire.
        operation_only: bool,
    },
    /// Valid for a pattern drawn from the configured enumeration (ATH-HITL-012), bounded by
    /// `policy.max_approval_ttl` (ATH-HITL-013).
    Pattern {
        /// The pattern, which must come from `policy.approval_scopes`.
        pattern: String,
        /// Expiry.
        expires_at: Timestamp,
    },
}

/// A human approval of a high-risk operation (spec section 26.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    /// Identifier.
    pub id: ApprovalId,
    /// Owning Task.
    pub task_id: TaskId,
    /// Requesting agent.
    pub requested_by: AgentId,
    /// Target operation.
    pub operation: String,
    /// Agent-authored reason. Untrusted; displayed as such (ATH-HITL-011).
    pub reason: String,
    /// Agent-authored impact. Untrusted (ATH-HITL-011).
    pub impact: String,
    /// Risk, decided by Core Host from configuration (ATH-CORE-038).
    pub risk_level: RiskLevel,
    /// State.
    pub state: ApprovalState,
    /// Canonical descriptor, generated by Core Host and the primary display content
    /// (ATH-HITL-010, ATH-HITL-011).
    pub operation_descriptor: String,
    /// Requesting instance.
    pub requested_by_instance_id: InstanceId,
    /// Remaining uses; 1 for `operation_only` (ATH-STATE-010).
    pub uses_remaining: u64,
    /// Expiry. No response means rejection (ATH-HITL-005).
    pub expires_at: Timestamp,
    /// Responder identity, recorded for audit (ATH-HITL-006).
    pub responded_by: Option<String>,
    /// Response time.
    pub responded_at: Option<Timestamp>,
    /// Scope.
    pub scope: ApprovalScope,
}

/// A memory record (spec section 26.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRecord {
    /// Identifier.
    pub id: MemoryId,
    /// Tier.
    pub tier: MemoryTier,
    /// Scope: Instance ID, Task ID, or Project ID by tier (spec section 13.1).
    pub scope_id: String,
    /// Content.
    pub content: String,
    /// Summary, present once evicted and summarized (ATH-MEM-002).
    pub summary: Option<String>,
    /// Provenance (ATH-MEM-005).
    pub origin: MemoryOrigin,
    /// Creation time.
    pub created_at: Timestamp,
    /// Expiry.
    pub expires_at: Option<Timestamp>,
}

/// A question the Core Agent asked a human (spec section 15.3 / `ATH-HITL-017..020`).
/// Silence is never read as an answer: the deadline (`timeout.clarification`, default
/// 24 h) expires the Task `FAILED`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub struct Clarification {
    /// Identifier.
    pub id: ClarificationId,
    /// Owning Task.
    pub task_id: TaskId,
    /// State.
    pub state: ClarificationState,
    /// The question (the Event carries only its digest, `ATH-EVENT-007`).
    pub question: String,
    /// The human answer, once answered.
    pub answer: Option<String>,
    /// Deadline; expiry fails the Task.
    pub expires_at: Timestamp,
    /// Creation time.
    pub created_at: Timestamp,
    /// Answer time.
    pub answered_at: Option<Timestamp>,
}

/// A composed agent hierarchy (spec section 26.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hierarchy {
    /// Identifier.
    pub id: HierarchyId,
    /// Owning Task.
    pub task_id: TaskId,
    /// Revision.
    pub revision: u64,
    /// Nodes.
    pub nodes: Vec<HierarchyNode>,
    /// Creation time.
    pub created_at: Timestamp,
}

/// What a hierarchy node is responsible for (spec section 26.6).
///
/// Director covers a group of SubTasks, Lead one SubTask, Worker one PlanStep
/// (ATH-HIER-012).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScopeRef {
    /// A group of SubTasks.
    SubTasks {
        /// The SubTasks.
        task_ids: Vec<TaskId>,
    },
    /// A single SubTask.
    SubTask {
        /// The SubTask.
        task_id: TaskId,
    },
    /// A single PlanStep.
    PlanStep {
        /// The step.
        plan_step_id: PlanStepId,
    },
    /// The root Task, held by the Producer.
    RootTask {
        /// The root Task.
        task_id: TaskId,
    },
}

/// One node of a hierarchy (spec section 26.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HierarchyNode {
    /// The agent at this node.
    pub agent_id: AgentId,
    /// Level.
    pub level: AgentLevel,
    /// Superior agent; `None` for the Producer (ATH-HIER-002).
    pub superior_agent_id: Option<AgentId>,
    /// What this node is responsible for.
    pub scope_ref: Option<ScopeRef>,
    /// Sibling peers this node may address directly.
    ///
    /// Recorded at composition time and checked by Core Host at delivery
    /// (ATH-MSG-006, ATH-MSG-018).
    pub sibling_acl: Vec<AgentId>,
}

/// A draft hierarchy node as submitted by Core Agent (identifiers remain Core Host's).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HierarchyNodeDraft {
    /// The agent at this node.
    pub agent_id: AgentId,
    /// Level.
    pub level: AgentLevel,
    /// Superior agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superior_agent_id: Option<AgentId>,
    /// Responsibility.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_ref: Option<ScopeRef>,
    /// Requested sibling permissions.
    #[serde(default)]
    pub sibling_acl: Vec<AgentId>,
}

/// A Runtime-managed execution unit (spec section 26.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeInstance {
    /// Identifier.
    pub id: RuntimeInstanceId,
    /// Runtime kind.
    pub runtime_type: RuntimeType,
    /// Corresponding Agent Instance, one to one (ATH-RT-012).
    pub instance_id: InstanceId,
    /// State.
    pub state: AgentInstanceState,
    /// Process id.
    pub pid: Option<u32>,
    /// Last heartbeat (ATH-CORE-047).
    pub last_heartbeat_at: Option<Timestamp>,
    /// Start time.
    pub started_at: Timestamp,
    /// Stop time.
    pub stopped_at: Option<Timestamp>,
    /// Stop reason.
    pub stop_reason: Option<StopReason>,
    /// Exit code.
    pub exit_code: Option<i32>,
    /// Whether side effects are known. `Unknown` gates retry behind Verification
    /// (ATH-CORE-048).
    pub side_effects: SideEffects,
}

/// A tool invocation record (spec section 26.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolExecution {
    /// Identifier.
    pub id: ToolExecutionId,
    /// Invoking instance.
    pub instance_id: InstanceId,
    /// Owning Task.
    pub task_id: Option<TaskId>,
    /// Tool.
    pub tool: ToolName,
    /// Canonical descriptor generated by Core Host (ATH-HITL-010).
    pub operation_descriptor: String,
    /// Risk, from configuration.
    pub risk_level: RiskLevel,
    /// Consumed approval, when one was required.
    pub approval_id: Option<ApprovalId>,
    /// Outcome.
    pub status: ToolExecutionStatus,
    /// Start time.
    pub started_at: Timestamp,
    /// Finish time.
    pub finished_at: Option<Timestamp>,
}

/// Who performed a verification (spec section 26.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Evaluator {
    /// An agent performed it.
    Agent {
        /// The agent.
        agent_id: AgentId,
    },
    /// A tool performed it.
    Tool,
}

/// An evaluation of a Result (spec section 26.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evaluation {
    /// Identifier.
    pub id: EvaluationId,
    /// Owning Task.
    pub task_id: TaskId,
    /// Evaluated Result.
    pub result_id: ResultId,
    /// Who evaluated (ATH-EVAL-003).
    pub evaluator: Evaluator,
    /// Method: test, lint, build, review, or simulation.
    pub method: String,
    /// Verdict.
    pub verdict: Verdict,
    /// Rationale.
    pub rationale: String,
    /// Creation time.
    pub created_at: Timestamp,
}

/// A chat session, which may precede Task creation (spec section 26.6, ATH-API-021).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    /// Identifier.
    pub id: ConversationId,
    /// Associated Task, linked once one is created.
    pub task_id: Option<TaskId>,
    /// Creation time.
    pub created_at: Timestamp,
}

/// One turn in a conversation (spec section 26.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Utterance {
    /// Identifier.
    pub id: UtteranceId,
    /// Owning conversation.
    pub conversation_id: ConversationId,
    /// Speaker.
    pub role: UtteranceRole,
    /// Kind.
    pub kind: UtteranceKind,
    /// Body.
    pub body: String,
    /// Associated clarification, when this turn is part of one.
    pub clarification_id: Option<String>,
    /// Creation time.
    pub created_at: Timestamp,
}

/// Interpretation of a human instruction, produced by Understand (spec section 7.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Understanding {
    /// The objective.
    pub goal: String,
    /// Constraints.
    pub constraints: Vec<String>,
    /// Completion criteria.
    pub completion_criteria: Vec<String>,
    /// Missing information. A non-empty list should produce an investigation step
    /// (ATH-CORE-015).
    pub unknowns: Vec<String>,
    /// Assessed risk.
    pub risk_level: RiskLevel,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_base_round_trips_on_the_wire() {
        let artifact = Artifact {
            result_id: ResultId::new(),
            kind: ArtifactKind::Patch,
            base: ArtifactBase::Store,
            path: "patch.diff".into(),
            bytes: Some(42),
            checksum: None,
            created_at: "2026-10-14T00:00:00Z".into(),
        };
        let json = serde_json::to_value(&artifact).expect("serialize");
        assert_eq!(json["base"], "store");
        assert_eq!(json["kind"], "patch");
        let back: Artifact = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, artifact);
    }

    #[test]
    fn permission_is_tagged_so_variants_stay_distinguishable() {
        let fs = Permission::Fs {
            mode: FsMode::Write,
            scope: "/work/project".into(),
        };
        let json = serde_json::to_value(&fs).expect("serialize");
        assert_eq!(json["kind"], "fs");
        assert_eq!(json["mode"], "write");
        let back: Permission = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, fs);
    }

    #[test]
    fn plan_step_draft_has_no_identifier_field() {
        // ATH-CORE-041: an agent cannot supply a PlanStepId, because the draft has nowhere to
        // put one. This is the structural form of "Core Host issues identifiers".
        let draft = PlanStepDraft {
            order: 1,
            intent: "analyse timing".into(),
            depends_on_orders: vec![],
            assigned_agent: None,
            skill_set: vec![],
        };
        let json = serde_json::to_value(&draft).expect("serialize");
        assert!(
            json.get("id").is_none(),
            "draft must not carry an id: {json}"
        );
        assert!(json.get("plan_id").is_none());
        assert!(json.get("state").is_none());
    }

    #[test]
    fn hierarchy_node_draft_has_no_identifier_field() {
        let draft = HierarchyNodeDraft {
            agent_id: AgentId::new("timing-lead"),
            level: AgentLevel::Lead,
            superior_agent_id: Some(AgentId::new("fpga-director")),
            scope_ref: None,
            sibling_acl: vec![],
        };
        let json = serde_json::to_value(&draft).expect("serialize");
        // agent_id is a persona slug chosen from existing definitions, not a minted id.
        assert!(json.get("hierarchy_id").is_none());
        assert!(json.get("revision").is_none());
    }

    #[test]
    fn approval_scope_distinguishes_single_use_from_pattern() {
        let single = ApprovalScope::OperationOnly {
            operation_only: true,
        };
        let json = serde_json::to_value(&single).expect("serialize");
        assert_eq!(json["operation_only"], true);

        let pattern = ApprovalScope::Pattern {
            pattern: "tool:shell:same_argv".into(),
            expires_at: "2026-10-15T00:00:00Z".into(),
        };
        let json = serde_json::to_value(&pattern).expect("serialize");
        assert_eq!(json["pattern"], "tool:shell:same_argv");
        let back: ApprovalScope = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, pattern);
    }

    #[test]
    fn core_agent_instance_is_not_bound_to_a_task() {
        // ATH-STATE-012: task_id is None for instance #0, which spans Tasks.
        let instance = AgentInstance {
            id: InstanceId::new(),
            agent_id: AgentId::core_agent(),
            task_id: None,
            plan_step_id: None,
            superior_instance_id: None,
            delegation_depth: 0,
            state: AgentInstanceState::Idle,
            runtime_instance_id: None,
            workspace: "/p/.athena/var/core-agent".into(),
            skill_set: vec![],
            aiprj_log_ref: None,
            attempt: 0,
            started_at: None,
            finished_at: None,
        };
        assert!(instance.task_id.is_none());
        assert!(instance.agent_id.is_core_agent());
    }

    #[test]
    fn scope_ref_variants_cover_every_hierarchy_level() {
        // ATH-HIER-012: each level has a defined unit of work.
        let refs = [
            ScopeRef::RootTask {
                task_id: TaskId::new(),
            },
            ScopeRef::SubTasks {
                task_ids: vec![TaskId::new()],
            },
            ScopeRef::SubTask {
                task_id: TaskId::new(),
            },
            ScopeRef::PlanStep {
                plan_step_id: PlanStepId::new(),
            },
        ];
        for scope in refs {
            let json = serde_json::to_value(&scope).expect("serialize");
            assert!(json.get("kind").is_some(), "scope must be tagged: {json}");
            let back: ScopeRef = serde_json::from_value(json).expect("deserialize");
            assert_eq!(back, scope);
        }
    }
}
