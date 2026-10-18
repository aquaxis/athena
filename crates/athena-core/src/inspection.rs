//! The privileged-request inspection path (ATH-CORE-026, spec §7.5.2).
//!
//! The Core Agent Process is **outside the trust boundary** (ATH-SEC-021), so every
//! `CoreRequest` passes through here before anything executes:
//!
//! - **Grant check.** Tools run only inside the requesting agent's permission set
//!   (ATH-SEC-025); out-of-grant use is refused as `PermissionError` (R-TOOL-02). The
//!   arguments never widen the grant.
//! - **Idempotency claim.** `Delegate` / `ExecuteTool` must carry
//!   `plan_step_id + ":" + attempt`, and Core Host must not execute the same key twice
//!   (ATH-CORE-035). The claim is taken here, persisted by the store; a replayed key
//!   comes back as a `Proceed` with `replay: true`, so the caller returns the recorded
//!   outcome instead of running the work again (ATH-NFR-17).
//! - **Risk classification.** First match over `policy.rules`, unmatched ⇒ `high`
//!   (ATH-CFG-009), built strictly from the request's facts — the tool and its
//!   arguments. The agent's `reason` / `impact` prose is structurally outside
//!   classification: [`Operation`] has no prose fields (ATH-CORE-038, R-CORE-25).
//!
//! Refusals carry everything the §23.2 `PrivilegedRequestDenied` Event needs
//! (`request_kind`, `error_class`, `policy_id`); the caller emits through the Event
//! Manager (ATH-PERS-003, D-1). The engine decides nothing beyond what configuration and
//! state already state (ATH-CORE-022).

use crate::protocol::{CoreRequest, ErrorClass, ErrorEnvelope};
use athena_config::policy::{Classification, CompiledPolicy, Operation};
use athena_events::events::PrivilegedRequestDenied;
use athena_persistence::repository::IdempotencyRepository;
use athena_types::{PlanStepId, ToolName};

/// The grounds for a refusal, with its Event and envelope material.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Reason {
    /// The requested tool is outside the agent's grant (R-TOOL-02).
    #[error("tool {tool:?} is not in the agent's allowed_tools")]
    OutOfGrantTool {
        /// The refused tool.
        tool: ToolName,
    },
    /// The idempotency key is not `plan_step_id + ":" + attempt` (ATH-CORE-035).
    #[error("idempotency key {0:?} is not `step-…:<attempt>`")]
    MalformedIdempotencyKey(String),
    /// The key does not belong to the PlanStep that carries it (ATH-CORE-035).
    #[error("idempotency key {0:?} is not a key of the referenced PlanStep")]
    ForeignIdempotencyKey(String),
    /// The delegation carried no executable instruction.
    #[error("a delegation must name a procedure for the agent to execute")]
    EmptyInstruction,
    /// The claims layer itself failed; ATHENA's fault, never retried (ATH-FAIL-002).
    #[error("the claims layer refused the claim: {0}")]
    ClaimFault(String),
}

impl Reason {
    /// The §20.1 class.
    #[must_use]
    pub const fn class(&self) -> ErrorClass {
        match self {
            Self::OutOfGrantTool { .. } => ErrorClass::PermissionError,
            Self::MalformedIdempotencyKey(_)
            | Self::ForeignIdempotencyKey(_)
            | Self::EmptyInstruction => ErrorClass::ValidationError,
            Self::ClaimFault(_) => ErrorClass::InternalError,
        }
    }

    /// A stable machine code within the class.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::OutOfGrantTool { .. } => "GRANT_TOOL_NOT_ALLOWED",
            Self::MalformedIdempotencyKey(_) => "INSPECTION_IDEMPOTENCY_KEY_SHAPE",
            Self::ForeignIdempotencyKey(_) => "INSPECTION_IDEMPOTENCY_FOREIGN_KEY",
            Self::EmptyInstruction => "INSPECTION_EMPTY_INSTRUCTION",
            Self::ClaimFault(_) => "CLAIMS_LAYER_FAULT",
        }
    }

    /// The `policy_id` the `PrivilegedRequestDenied` Event must carry.
    #[must_use]
    pub const fn policy_id(&self) -> &'static str {
        match self {
            Self::OutOfGrantTool { .. } => "agent.grant",
            Self::MalformedIdempotencyKey(_)
            | Self::ForeignIdempotencyKey(_)
            | Self::EmptyInstruction => "core.inspection",
            Self::ClaimFault(_) => "core.claims",
        }
    }

    /// The structured detail fields of the refusal.
    #[must_use]
    pub fn details(&self) -> serde_json::Value {
        match self {
            Self::OutOfGrantTool { tool } => serde_json::json!({ "tool": tool.as_str() }),
            Self::MalformedIdempotencyKey(key) | Self::ForeignIdempotencyKey(key) => {
                serde_json::json!({ "key": key.clone() })
            }
            Self::EmptyInstruction => serde_json::json!({}),
            Self::ClaimFault(fault) => serde_json::json!({ "fault": fault.clone() }),
        }
    }

    /// The `PrivilegedRequestDenied` payload this refusal records (ATH-CORE-037).
    #[must_use]
    pub fn denial_event(&self, request_kind: &str) -> PrivilegedRequestDenied {
        PrivilegedRequestDenied {
            request_kind: request_kind.into(),
            error_class: serde_wire_class(self.class()),
            policy_id: self.policy_id().into(),
        }
    }
}

/// The outcome of inspection.
#[derive(Debug, Clone, PartialEq)]
pub enum Inspection {
    /// The request may proceed.
    Proceed {
        /// The classification, for the kinds whose execution risk drives an approval
        /// (`execute_tool`: ATH-HITL-001).
        classification: Option<Classification>,
        /// Whether the admission granted was idempotent replay (ATH-CORE-035): the caller
        /// returns the recorded outcome instead of executing again.
        replay: bool,
    },
    /// Refused; the caller records the denial and answers `CoreResponse::Err`
    /// (ATH-CORE-034, ATH-CORE-037).
    Rejected(Reason),
}

/// The per-request facts inspection needs and must not discover by itself.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InspectionFacts {
    /// The requesting agent's own grant, from its definition (ATH-SEC-025).
    pub allowed_tools: Vec<ToolName>,
}

/// The inspection engine, over a compiled `policy.rules` set.
pub struct Inspector<'policy> {
    policy: &'policy CompiledPolicy,
}

impl<'policy> Inspector<'policy> {
    /// An engine over an already compiled policy (compile happens at ATHENA start:
    /// ATH-CFG-003).
    #[must_use]
    pub const fn new(policy: &'policy CompiledPolicy) -> Self {
        Self { policy }
    }

    /// Inspect one request. `claims` is the persisted idempotency store; the claim is
    /// taken inside the engine, and the caller records the outcome under the same key.
    pub fn inspect(
        &self,
        request: &CoreRequest,
        facts: &InspectionFacts,
        claims: &mut dyn IdempotencyRepository,
    ) -> Inspection {
        match request {
            CoreRequest::ExecuteTool {
                tool,
                args,
                idempotency_key,
                ..
            } => {
                if let Err(reason) = shape_of(idempotency_key, None) {
                    return Inspection::Rejected(reason);
                }
                let replay = match claims.claim(idempotency_key) {
                    Ok(first_time) => !first_time,
                    Err(err) => {
                        // A claim-layer breakdown is ATHENA's own fault, not the agent's;
                        // escalated as `InternalError` by the caller (ATH-FAIL-002).
                        return Inspection::Rejected(misconfigured_claims(&err));
                    }
                };
                if !facts.allowed_tools.contains(tool) {
                    return Inspection::Rejected(Reason::OutOfGrantTool { tool: *tool });
                }
                let operation = Operation::new(*tool, args.to_string());
                Inspection::Proceed {
                    classification: Some(self.policy.classify(&operation)),
                    replay,
                }
            }
            CoreRequest::Delegate {
                plan_step_id,
                idempotency_key,
                instruction,
                ..
            } => {
                if let Err(reason) = shape_of(idempotency_key, Some(plan_step_id)) {
                    return Inspection::Rejected(reason);
                }
                let replay = match claims.claim(idempotency_key) {
                    Ok(first_time) => !first_time,
                    Err(err) => return Inspection::Rejected(misconfigured_claims(&err)),
                };
                if instruction.trim().is_empty() {
                    return Inspection::Rejected(Reason::EmptyInstruction);
                }
                // The subordinate's grant belongs to its own definition, and depth/containment
                // checks belong to the Hierarchy Manager (ATH-HIER-005, ATH-SEC-009) — they
                // run at composition and delegation, on their own rules.
                Inspection::Proceed {
                    classification: None,
                    replay,
                }
            }
            // Their decisive checks live in their owning modules:
            CoreRequest::TransitionTask { .. } | CoreRequest::ConsumeApproval { .. } => {
                // `expected_state` mismatch is the State Manager's optimistic check
                // (ATH-CORE-036, ATH-STATE-014); consumption is the Approval Manager's
                // reclassification (ATH-HITL-014).
                proceed_none()
            }
            CoreRequest::SubmitUnderstanding { .. }
            | CoreRequest::SubmitPlan { .. }
            | CoreRequest::ComposeHierarchy { .. }
            | CoreRequest::RequestApproval { .. }
            | CoreRequest::AskHuman { .. }
            | CoreRequest::SubmitEvaluation { .. }
            | CoreRequest::WriteMemory { .. }
            | CoreRequest::CompleteTask { .. } => proceed_none(),
        }
    }
}

/// The §20.1 wire form of a class (serde snake_case), as text for the Event payload.
fn serde_wire_class(class: ErrorClass) -> String {
    let wire = serde_json::to_value(class).expect("class always serializes");
    wire.as_str().expect("class wire form").to_string()
}

fn proceed_none() -> Inspection {
    Inspection::Proceed {
        classification: None,
        replay: false,
    }
}

/// A claims-layer failure is ATHENA's own fault (never the agent's prose): recorded as
/// `InternalError`, never retried (ATH-FAIL-002).
fn misconfigured_claims(err: &athena_persistence::error::PersistenceError) -> Reason {
    Reason::ClaimFault(format!("{err}"))
}

/// The `plan_step_id + ":" + attempt` shape check (ATH-CORE-035).
///
/// With the owning PlanStep present, the key's identifier portion must be that step's —
/// a mis-keyed delegation after restart replay must never steal another step's claim.
fn shape_of(key: &str, owner: Option<&PlanStepId>) -> Result<(), Reason> {
    let Some((slug, attempt)) = key.split_once(':') else {
        return Err(Reason::MalformedIdempotencyKey(key.into()));
    };
    if !slug.starts_with("step-") {
        return Err(Reason::MalformedIdempotencyKey(key.into()));
    }
    if attempt.is_empty() || !attempt.chars().all(|c| c.is_ascii_digit()) {
        return Err(Reason::MalformedIdempotencyKey(key.into()));
    }
    if let Some(owner) = owner
        && owner.as_str() != slug
    {
        return Err(Reason::ForeignIdempotencyKey(key.into()));
    }
    Ok(())
}

/// The §22.2 envelope a refusal produces (ATH-CORE-034).
#[must_use]
pub fn deny_envelope(reason: &Reason, correlation_id: impl Into<String>) -> ErrorEnvelope {
    ErrorEnvelope {
        class: reason.class(),
        code: reason.code().to_string(),
        message: reason.to_string(),
        details: {
            let mut details = reason.details();
            if let Some(object) = details.as_object_mut() {
                object.insert("policy_id".into(), serde_json::json!(reason.policy_id()));
            }
            details
        },
        correlation_id: correlation_id.into(),
    }
}
