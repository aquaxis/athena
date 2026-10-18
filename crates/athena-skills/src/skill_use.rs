//! The high-risk skill approval route (P5-12; `ATH-SKILL-024` MUST — a `high` risk
//! skill's use requires human approval *before* the use):
//!
//! The route composes the Core Host approval machinery (§11, `ApprovalManager`) with
//! the Skill Catalog's risk face. Since `athena-skills` cannot see Core (§32.2), this
//! module is the **review layer**: the fact-pair Core Host consults when a resolved
//! skill set carries a `high`-risk member — the use routes through an approval whose
//! state declares what happens next, exactly like the Task-level privileged operations
//! (`ATH-HITL-001`, `ATH-HITL-010`).
//!
//! Rules:
//!
//! 1. A `low` / `medium` risk skill needs no approval — proceed.
//! 2. A `high` risk skill **requires** a `GRANTED`, **unconsumed** approval naming the
//!    skill's operation; without one, the use is held (`ApprovalRequested` shape the
//!    wiring emits; canonical descriptor foremost — the Agent's risk prose is never
//!    the classification, `ATH-CORE-038`).
//! 3. A `REJECTED` / `EXPIRED` approval denies the use — expiry is rejection
//!    (`ATH-HITL-005`, `ATH-STATE-011`); the denial is a `SkillError`-class fact for
//!    the Core Agent's next decision, never a silent downgrade to skip the skill.
//! 4. `CONSUMED` (uses exhausted) demands a fresh approval (`ATH-HITL-007`).

use athena_types::{ApprovalState, RiskLevel};

/// Why a skill's use was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SkillUseFault {
    /// A high-risk skill with no approval record — held for approval.
    #[error("skill {0:?} is high risk; its use requires an approval")]
    ApprovalRequired(String),
    /// The approval denied the use (rejected / expired ≡ rejected).
    #[error("the approval for {0:?} denies the use ({1})")]
    Denied(String, &'static str),
    /// The approval's uses are exhausted — a fresh approval is required.
    #[error("the approval for {0:?} is consumed; a fresh approval is required (ATH-HITL-007)")]
    Consumed(String),
}

/// The review: whether one skill's use may proceed.
///
/// # Errors
/// [`SkillUseFault`] naming the route's fact.
pub fn approve_skill_use(
    skill_id: &str,
    risk: RiskLevel,
    approval: Option<ApprovalState>,
) -> Result<(), SkillUseFault> {
    match risk {
        // 1: non-high risks proceed (the policy may still demand checks elsewhere —
        // the composition point is the Inspector, not a skill's self-assessment).
        RiskLevel::Low | RiskLevel::Medium => Ok(()),
        RiskLevel::High => match approval {
            // 2: without a record, the use is held.
            None => Err(SkillUseFault::ApprovalRequired(skill_id.to_string())),
            // 3: a denied record stays denied.
            Some(state @ ApprovalState::Rejected) => Err(SkillUseFault::Denied(
                skill_id.to_string(),
                denied_face(state),
            )),
            Some(state @ ApprovalState::Expired) => Err(SkillUseFault::Denied(
                skill_id.to_string(),
                denied_face(state),
            )),
            // 4: uses exhausted ⇒ fresh approval.
            Some(ApprovalState::Consumed) => Err(SkillUseFault::Consumed(skill_id.to_string())),
            // GRANTED: proceed (the consumption counts at the execution, `ATH-STATE-010`).
            Some(ApprovalState::Granted) => Ok(()),
            // REQUESTED: still pending — held, never an implicit grant.
            Some(ApprovalState::Requested) => {
                Err(SkillUseFault::ApprovalRequired(skill_id.to_string()))
            }
        },
    }
}

fn denied_face(state: ApprovalState) -> &'static str {
    match state {
        ApprovalState::Rejected => "rejected by the human",
        ApprovalState::Expired => "expired; silence is rejection",
        _ => "denied",
    }
}

#[cfg(test)]
mod skill_use_tests;
