//! The Core Control Protocol (spec §7.5.5, ATH-CORE-033).
//!
//! Core Agent's privileged requests are expressed as `CoreRequest` values carried in a
//! message `payload` with `type: control_request`; Core Host answers every `CoreRequest`
//! with a `CoreResponse` (ATH-CORE-034) and never leaves a side path: no direct tool
//! execution, no direct process start, no direct store writes (ATH-CORE-033, ATH-CORE-027).
//!
//! Wire form: JSON, validated against the JSON Schema generated from these types
//! (ATH-CORE-039, D-4). Re-emission after a validation failure is bounded by Core Host
//! (ATH-CORE-040, max 3, then `InternalError` escalation).
//!
//! Identifier issuance (ATH-CORE-041) is structural here: an agent submits *drafts*
//! (`PlanStepDraft`, `HierarchyNodeDraft`) whose wire type has no identifier field at all,
//! and every aggregate identifier that appears in a request was minted by Core Host earlier
//! — in the same session and recorded by the store — so an agent cannot invent one that
//! would survive inspection.

use athena_types::{
    AgentId, ApprovalId, HierarchyNodeDraft, PlanStepDraft, PlanStepId, ResultId, SkillRef, TaskId,
    TaskState, ToolName, Understanding, Verdict,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The twelve privileged request kinds (spec §7.5.5). Exhaustive on purpose: a kind outside
/// this set is a specification revision first (ATH-CORE-033).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[derive(JsonSchema)]
pub enum CoreRequest {
    /// Understand: the agent's interpretation of the Task (ATH-CORE-014).
    SubmitUnderstanding {
        /// The Task the understanding applies to.
        task_id: TaskId,
        /// Goal, constraints, completion criteria, unknowns, assessed risk.
        understanding: Understanding,
    },
    /// Plan: a full replacement plan at a revision (ATH-CORE-042).
    SubmitPlan {
        /// The Task.
        task_id: TaskId,
        /// The revision this plan edits; must match Core's current revision.
        base_revision: u64,
        /// Draft steps — identifiers are issued by Core Host (ATH-CORE-041).
        steps: Vec<PlanStepDraft>,
    },
    /// Decide: the hierarchy the agent judged the Task needs (ATH-HIER-009).
    ComposeHierarchy {
        /// The Task.
        task_id: TaskId,
        /// Draft nodes without identifiers (D-5).
        nodes: Vec<HierarchyNodeDraft>,
    },
    /// Act: delegate a PlanStep to an agent (ATH-CORE-035).
    Delegate {
        /// The Task.
        task_id: TaskId,
        /// The step to run.
        plan_step_id: PlanStepId,
        /// The agent selected.
        agent_id: AgentId,
        /// The resolved Skill Set for the step.
        skill_set: Vec<SkillRef>,
        /// The delegation instruction — the agent's procedure reference.
        instruction: String,
        /// `plan_step_id + ":" + attempt`; Core Host executes a key once (ATH-CORE-035).
        idempotency_key: String,
    },
    /// Act: run a tool under the agent's grant (ATH-TOOL-002).
    ExecuteTool {
        /// The Task.
        task_id: TaskId,
        /// The ATHENA tool.
        tool: ToolName,
        /// The arguments; risk comes from `policy.rules`, never from the agent's prose
        /// (ATH-CORE-038).
        args: serde_json::Value,
        /// `plan_step_id + ":" + attempt` style key (ATH-CORE-035).
        idempotency_key: String,
    },
    /// Request a human approval (ATH-HITL-001) — `reason` / `impact` are agent prose,
    /// shown as untrusted context only (ATH-HITL-011).
    RequestApproval {
        /// The Task.
        task_id: TaskId,
        /// The canonical operation reference Core Host will reclassify independently.
        operation_ref: String,
        /// The agent's prose case.
        reason: String,
        /// The agent's prose impact statement. Never a risk input (ATH-CORE-038).
        impact: String,
    },
    /// Ask Core Host to consume an approval for an operation (ATH-STATE-013).
    ConsumeApproval {
        /// The approval to consume.
        approval_id: ApprovalId,
        /// The operation the consumption applies to; Core Host reclassifies risk at
        /// consumption (ATH-HITL-014).
        operation_ref: String,
    },
    /// Clarify with the human (ATH-HITL-018): the Task waits for an answer.
    AskHuman {
        /// The Task.
        task_id: TaskId,
        /// The question. Its digest is what travels through Events (ATH-EVENT-007).
        question: String,
        /// Options, when the question is a multiple choice.
        options: Option<Vec<String>>,
    },
    /// Evaluate a Result against the completion criteria (ATH-EVAL-001).
    SubmitEvaluation {
        /// The Task.
        task_id: TaskId,
        /// The Result under evaluation.
        result_id: ResultId,
        /// Exactly one of the five verdicts (ATH-EVAL-004).
        verdict: Verdict,
        /// Rationale (ATH-CORE-016).
        rationale: String,
    },
    /// Progress: a Task state change the agent asks for, optimistic on `expected_state`
    /// (ATH-STATE-014).
    TransitionTask {
        /// The Task.
        task_id: TaskId,
        /// The requested state.
        to: TaskState,
        /// The state the agent believes holds (ATH-CORE-036).
        expected_state: TaskState,
    },
    /// Record a memory record (ATH-MEM-003) — Working Memory throughout, Long Term only
    /// under an explicit judgement.
    WriteMemory {
        /// Which tier the agent asks to write.
        tier: WritableMemoryTier,
        /// The content; Core Host masks secrets before persistence (R-POL-13).
        content: String,
    },
    /// Complete: the Task's final summary and its Results (ATH-EVAL-002).
    CompleteTask {
        /// The Task.
        task_id: TaskId,
        /// The closing summary.
        summary: String,
        /// The Results that make up the completion; verifiable deliverables must already
        /// have passed Verification.
        result_ids: Vec<ResultId>,
    },
}

impl CoreRequest {
    /// The serde tag this kind rides on, for the `PrivilegedRequestDenied` payload
    /// (ATH-CORE-037).
    #[must_use]
    pub const fn kind_tag(&self) -> &'static str {
        match self {
            Self::SubmitUnderstanding { .. } => "submit_understanding",
            Self::SubmitPlan { .. } => "submit_plan",
            Self::ComposeHierarchy { .. } => "compose_hierarchy",
            Self::Delegate { .. } => "delegate",
            Self::ExecuteTool { .. } => "execute_tool",
            Self::RequestApproval { .. } => "request_approval",
            Self::ConsumeApproval { .. } => "consume_approval",
            Self::AskHuman { .. } => "ask_human",
            Self::SubmitEvaluation { .. } => "submit_evaluation",
            Self::TransitionTask { .. } => "transition_task",
            Self::WriteMemory { .. } => "write_memory",
            Self::CompleteTask { .. } => "complete_task",
        }
    }

    /// The request's Task, when the kind carries one (`WriteMemory` does not).
    #[must_use]
    pub fn task_id(&self) -> Option<&TaskId> {
        match self {
            Self::SubmitUnderstanding { task_id, .. }
            | Self::SubmitPlan { task_id, .. }
            | Self::ComposeHierarchy { task_id, .. }
            | Self::Delegate { task_id, .. }
            | Self::ExecuteTool { task_id, .. }
            | Self::RequestApproval { task_id, .. }
            | Self::AskHuman { task_id, .. }
            | Self::SubmitEvaluation { task_id, .. }
            | Self::TransitionTask { task_id, .. }
            | Self::CompleteTask { task_id, .. } => Some(task_id),
            Self::ConsumeApproval { .. } | Self::WriteMemory { .. } => None,
        }
    }

    /// The idempotency key, for the two kinds that must carry one (ATH-CORE-035).
    #[must_use]
    pub fn idempotency_key(&self) -> Option<&str> {
        match self {
            Self::Delegate {
                idempotency_key, ..
            }
            | Self::ExecuteTool {
                idempotency_key, ..
            } => Some(idempotency_key.as_str()),
            _ => None,
        }
    }
}

/// Build the idempotency key exactly as the specification prescribes (ATH-CORE-035).
#[must_use]
pub fn idempotency_key(step: &PlanStepId, attempt: u64) -> String {
    format!("{}:{}", step.as_str(), attempt)
}

/// The tiers an agent may explicitly write (ATH-MEM-003).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WritableMemoryTier {
    /// Working Memory: scope and lifetime of the Task.
    Working,
    /// Long Term: requires an explicit Core Agent judgement or human instruction.
    Long,
}

/// The fixed handling a §20.1 class carries (`ATH-FAIL-001`/`002`): the behaviour is
/// decided by the class, never re-decided at a call site; the tables below are the
/// spec section 20.1 columns as code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Disposition {
    /// The retry kind.
    pub retry: RetryKind,
    /// Whether the class re-plans the Task when its budget allows.
    pub replan: bool,
    /// Whether the class escalates to a human.
    pub escalation: Escalation,
    /// Whether re-plan applies at the attempt limit (the "上限到達時に実施" rows).
    pub replan_at_limit: bool,
}

/// How a class may retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryKind {
    /// No retry at all.
    Never,
    /// Exponential backoff (`ATH-FAIL-006`).
    ExponentialBackoff,
    /// Bounded retries, re-plan when the bound is reached.
    Bounded,
    /// The next attempt runs on an alternate agent.
    AlternateAgent,
    /// The next attempt runs an alternate skill (`ATH-FAIL-012`'s shape).
    AlternateSkill,
    /// The Runtime Instance is recreated.
    RecreateInstance,
}

/// Which escalation a class demands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Escalation {
    /// None.
    None,
    /// A human decides (the approval path, §20.1's 承認 column).
    Human,
}

/// The nine-class error taxonomy (spec §20.1), the `class` value of an error envelope
/// (ATH-API-008).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    /// Input that cannot be acted on.
    ValidationError,
    /// Outside the agent's grant.
    PermissionError,
    /// A configured policy refuses it.
    PolicyError,
    /// Retryable without a fresh Core decision.
    TransientError,
    /// A deadline passed.
    TimeoutError,
    /// The agent failed.
    AgentError,
    /// A skill failed or conflicted.
    SkillError,
    /// The Runtime failed.
    RuntimeError,
    /// ATHENA itself is inconsistent; never retried (ATH-FAIL-002).
    InternalError,
}
impl ErrorClass {
    /// Every class, in specification order.
    pub const ALL: [Self; 9] = [
        Self::ValidationError,
        Self::PermissionError,
        Self::PolicyError,
        Self::TransientError,
        Self::TimeoutError,
        Self::AgentError,
        Self::SkillError,
        Self::RuntimeError,
        Self::InternalError,
    ];

    /// The canonical HTTP status of the class (spec §22.2's mapping, one representative
    /// per class).
    ///
    /// Task-API handlers use this when an error carries no more specific destination;
    /// `ValidationError` variants may narrow to 404 / 409 by the code they carry (the
    /// handler decides, not the class).
    #[must_use]
    pub const fn default_status(self) -> u16 {
        match self {
            Self::ValidationError => 400,
            Self::PermissionError | Self::PolicyError => 403,
            Self::TransientError => 429,
            Self::InternalError => 500,
            Self::RuntimeError => 503,
            Self::TimeoutError => 504,
            Self::AgentError | Self::SkillError => 500,
        }
    }

    /// The documented HTTP mapping (spec §22.2).
    ///
    /// Unauthenticated (401) reports no class by definition: there was no inspection.
    #[must_use]
    pub const fn http_statuses(self) -> &'static [u16] {
        match self {
            Self::ValidationError => &[400, 404, 409],
            Self::PermissionError | Self::PolicyError => &[403],
            Self::TransientError => &[429, 503],
            Self::InternalError => &[500],
            Self::RuntimeError => &[503],
            Self::TimeoutError => &[504],
            Self::AgentError | Self::SkillError => &[500],
        }
    }

    /// The §20.1 disposition (`ATH-FAIL-001`/`002`): every error class carries its fixed
    /// retry / re-plan / approval handling — the taxonomy *is* the behaviour; a caller
    /// never re-decides per call site.
    #[must_use]
    pub const fn disposition(self) -> Disposition {
        match self {
            Self::ValidationError => Disposition {
                retry: RetryKind::Never,
                replan: false,
                escalation: Escalation::None,
                replan_at_limit: false,
            },
            Self::PermissionError => Disposition {
                retry: RetryKind::Never,
                replan: false,
                escalation: Escalation::Human,
                replan_at_limit: false,
            },
            Self::PolicyError => Disposition {
                retry: RetryKind::Never,
                replan: false,
                escalation: Escalation::Human,
                replan_at_limit: false,
            },
            Self::TransientError => Disposition {
                retry: RetryKind::ExponentialBackoff,
                replan: false,
                escalation: Escalation::None,
                replan_at_limit: false,
            },
            Self::TimeoutError => Disposition {
                retry: RetryKind::Bounded,
                replan: false,
                escalation: Escalation::None,
                replan_at_limit: true,
            },
            Self::AgentError => Disposition {
                retry: RetryKind::AlternateAgent,
                replan: true,
                escalation: Escalation::None,
                replan_at_limit: true,
            },
            Self::SkillError => Disposition {
                retry: RetryKind::AlternateSkill,
                replan: true,
                escalation: Escalation::None,
                replan_at_limit: true,
            },
            Self::RuntimeError => Disposition {
                retry: RetryKind::RecreateInstance,
                replan: false,
                escalation: Escalation::None,
                replan_at_limit: true,
            },
            Self::InternalError => Disposition {
                retry: RetryKind::Never,
                replan: false,
                escalation: Escalation::None,
                replan_at_limit: false,
            },
        }
    }

    /// The unclassifiable face (`ATH-FAIL-002`): anything that did not classify maps
    /// to `InternalError`, which carries both `retry: Never` and no re-plan — a guess
    /// is never made on its behalf.
    #[must_use]
    pub const fn unclassifiable() -> Self {
        Self::InternalError
    }
}

/// The §22.2 error envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorEnvelope {
    /// The §20.1 class.
    pub class: ErrorClass,
    /// A stable machine code within the class.
    pub code: String,
    /// A human-readable message, safe to display (no secrets).
    pub message: String,
    /// Structured details; must not carry secrets (ATH-EVENT-007 by analogy).
    pub details: serde_json::Value,
    /// The correlation id that joins this rejection to its Task chain (ATH-API-009).
    pub correlation_id: String,
}

/// Every privileged request gets exactly one response (ATH-CORE-034).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CoreResponse<T = serde_json::Value> {
    /// Accepted, carrying the kind's effect.
    Ok {
        /// Identifier minted by Core Host, for matching request and answer.
        request_id: String,
        /// The effect payload.
        data: T,
    },
    /// Refused, with the specification's envelope.
    Err {
        /// Identifier minted by Core Host.
        request_id: String,
        /// Why, per §22.2.
        error: ErrorEnvelope,
    },
}

impl<T> CoreResponse<T> {
    /// The request identifier, whichever arm this is.
    #[must_use]
    pub fn request_id(&self) -> &str {
        match self {
            Self::Ok { request_id, .. } | Self::Err { request_id, .. } => request_id,
        }
    }
}

impl<T: Serialize> CoreResponse<T> {
    /// Serialize the response to the payload text a transport delivers.
    ///
    /// # Errors
    /// Serialization failure, which for a response is a Core-side bug
    /// (`InternalError` for the caller).
    pub fn to_payload_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

/// Rejection bookkeeping: Core Host records every refusal as `PrivilegedRequestDenied`
/// (ATH-CORE-037) and the fact re-surfaces in restarted context (ATH-CORE-029).
#[must_use]
pub fn denial_is_recordable(envelope: &ErrorEnvelope) -> bool {
    // A recordable rejection carries its correlation id, so the context rebuild and the
    // audit chain can find it.
    !envelope.correlation_id.trim().is_empty()
}

/// The wire-name check (§7.5.5): the twelve kinds, by their serde tag.
///
/// Kept next to the enum so a revision that adds a kind has to convince both lists.
pub const REQUEST_KINDS: [&str; 12] = [
    "submit_understanding",
    "submit_plan",
    "compose_hierarchy",
    "delegate",
    "execute_tool",
    "request_approval",
    "consume_approval",
    "ask_human",
    "submit_evaluation",
    "transition_task",
    "write_memory",
    "complete_task",
];
