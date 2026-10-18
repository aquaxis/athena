//! `ORPHANED` PlanStep handling (P4-25) — the Verification-gated retry and the
//! one-instance-per-attempt rule.
//!
//! When an instance's process vanishes (boot reconciliation or supervision) its attached
//! `RUNNING` step lands `ORPHANED` (`InstanceVanished`, §17.5). The step's side effects are
//! then *undetermined*: the spec gates any re-run of such a step behind an objective
//! Verification (`ATH-STATE-029`), fires the re-arm only from Core Host
//! (`ORPHANED → READY`, so the next attempt re-enters through the Scheduler), and requires
//! every attempt to be an independent Agent Instance (`ATH-STATE-030`, `ATH-FAIL-007`,
//! `ATH-SCHED-009`).
//!
//! This module is the review layer for exactly those two facts. Like
//! [`crate::recovery`] it decides nothing itself — Core Agent owns the recovery decision
//! (`ATH-DEV-002`, `ATH-CORE-049`); Core Host reviews *records* against facts and returns
//! the §17.5 route (or a refusal the caller hands back, unfixed) to the wiring.

use athena_types::PlanStepState;
use athena_types::{Evaluator, Verdict};

/// The objective Verification an `ORPHANED` step must pass before a re-run
/// (`ATH-STATE-029` via `ATH-EVAL-002`): establish what its side effects actually are.
///
/// Every field is a fact that must be recordable (`ATH-EVAL-003`): the implementor, the
/// objective method, and the verdict. A verification whose method is empty is a
/// self-attestation, never accepted (`ATH-EVAL-005`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SideEffectVerification {
    /// Who performed the verification (Agent or Tool; `ATH-EVAL-003`).
    pub evaluator: Evaluator,
    /// The objective means: test, lint, build, review, or simulation.
    pub method: String,
    /// The verdict the objective means produced.
    pub verdict: Verdict,
    /// What the verification established about the step's side effects, recorded verbatim.
    pub rationale: String,
}

/// Why an `ORPHANED` re-run request was refused (the caller hands the refusal back
/// unfixed — nothing is silently substituted, `ATH-CORE-049`'s rule at recovery).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OrphanFault {
    /// The step is not `ORPHANED`; the gate applies to that state only
    /// (`ATH-STATE-029` scopes the rule to `ORPHANED`).
    #[error("the step is not ORPHANED; the Verification gate does not apply")]
    NotOrphaned,
    /// No Verification record exists — the side effects remain undetermined, so no
    /// re-run may fire (`ATH-STATE-029`, `ATH-CORE-048`).
    #[error("no Verification record for the ORPHANED step's side effects")]
    Unverified,
    /// The record is a self-attestation: no implementor or no objective method —
    /// nothing objective stands behind it (`ATH-EVAL-002`, `ATH-EVAL-005`).
    #[error("the Verification is not an objective record (no implementor or no method)")]
    SelfAttestation,
    /// The objective means ran and did not accept (`ATH-EVAL-004`): the verdict is any
    /// of `Retry` / `Replan` / `Fail` / `Escalate` — the step is not re-armed, and the
    /// verdict is the fact the Core Agent's next decision records.
    #[error("the Verification verdict is {0:?}, not an accept")]
    NotAccepted(Verdict),
}

/// The §17.5 route an `ORPHANED` step legally takes after review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrphanRoute {
    /// Fire `VerifiedRetryAllowed`: the step returns to `READY`
    /// (`ORPHANED → READY`, Core Host, `PlanStepReady`) and the next attempt schedules
    /// on a fresh instance. Carries the reviewed record.
    Rearmed(SideEffectVerification),
    /// The step cannot be retried (`ORPHANED → FAILED`, Core Agent's decision,
    /// `PlanStepFailed`): the Core Agent concluded `RetryImpossible` — for example the
    /// budget block, or a Verification the Core Agent declines to run. The refusal
    /// (when one preceded) is the recorded cause, not a substitution.
    FailedStep(Option<OrphanFault>),
    /// Nothing fires here; the refusal returned to the Core Agent unfixed
    /// (`ATH-CORE-049`'s rule at recovery).
    Refused(OrphanFault),
}

/// Review a request to re-run an `ORPHANED` step.
///
/// Facts reviewed, in order: the step's state (`ORPHANED` only — `ATH-STATE-029`),
/// the presence of an objective Verification record, its objectivity
/// (`ATH-EVAL-002`/`ATH-EVAL-005`), and its verdict (`ATH-EVAL-004`). The transition
/// itself rides the State Manager; the route here names the §17.5 trigger the wiring
/// must use ([`athena_state::PlanStepTrigger::VerifiedRetryAllowed`] for
/// [`OrphanRoute::Rearmed`], [`athena_state::PlanStepTrigger::RetryImpossible`] with the
/// Core Agent as requester for `FAILED`).
#[must_use]
pub fn authorize_orphan_retry(
    step_state: PlanStepState,
    verification: Option<&SideEffectVerification>,
) -> OrphanRoute {
    if step_state != PlanStepState::Orphaned {
        return OrphanRoute::Refused(OrphanFault::NotOrphaned);
    }
    let Some(record) = verification else {
        return OrphanRoute::Refused(OrphanFault::Unverified);
    };
    if record.method.trim().is_empty() || record.rationale.trim().is_empty() {
        return OrphanRoute::Refused(OrphanFault::SelfAttestation);
    }
    match record.verdict {
        Verdict::Accept => OrphanRoute::Rearmed(record.clone()),
        // The objective means ran and rejected: the step is not re-armed here. The
        // Core Agent chooses between a wider recovery and `RetryImpossible`; this
        // review returns the fact, never a substitution (`ATH-CORE-049`).
        verdict @ (Verdict::Retry | Verdict::Replan | Verdict::Fail | Verdict::Escalate) => {
            OrphanRoute::Refused(OrphanFault::NotAccepted(verdict))
        }
    }
}

/// The §17.5 route when the Core Agent concludes the step cannot be retried: Core Host
/// fires `RetryImpossible` (`ORPHANED → FAILED`, `PlanStepFailed`) with the Core Agent
/// named as the requesting actor.
///
/// # Errors
/// [`OrphanFault::NotOrphaned`] when the step is not `ORPHANED`.
pub fn fail_orphaned_step(step_state: PlanStepState) -> Result<(), OrphanFault> {
    if step_state != PlanStepState::Orphaned {
        return Err(OrphanFault::NotOrphaned);
    }
    // The §17.5 row exists and names the Core Agent as requester; the State Manager
    // owns the write. Verified here against the table the State Manager uses.
    match athena_state::plan_step_transition(
        PlanStepState::Orphaned,
        athena_state::PlanStepTrigger::RetryImpossible,
    ) {
        Some(row) if row.to == PlanStepState::Failed => Ok(()),
        _ => Err(OrphanFault::NotOrphaned),
    }
}

/// Enforce the one-instance-per-attempt rule before a retry attempt is issued
/// (`ATH-STATE-030`, `ATH-FAIL-007`, `ATH-SCHED-009`): every attempt of a PlanStep is an
/// independent Agent Instance, so (a) two live rows never share a `(step, attempt)` pair,
/// and (b) a new attempt's number comes strictly after every recorded attempt.
///
/// # Errors
/// [`OrphanFault::NotOrphaned`] is not raised here; the faults are the shape mismatches
/// [`FreshInstanceFault`] describes.
pub fn fresh_instance_for_attempt(
    step: &athena_types::PlanStepId,
    attempt: u64,
    existing: &[athena_types::AgentInstance],
) -> Result<(), FreshInstanceFault> {
    if attempt == 0 {
        return Err(FreshInstanceFault::AttemptNumber(attempt));
    }
    for instance in existing
        .iter()
        .filter(|i| i.plan_step_id.as_ref() == Some(step) && i.task_id.is_some())
    {
        if instance.attempt == attempt {
            return Err(FreshInstanceFault::AttemptAlreadyBound {
                step: step.clone(),
                attempt,
                instance: instance.id.clone(),
            });
        }
        if instance.attempt >= attempt {
            return Err(FreshInstanceFault::AttemptNotNew {
                step: step.clone(),
                recorded: instance.attempt,
                requested: attempt,
                instance: instance.id.clone(),
            });
        }
    }
    Ok(())
}

/// Why a new attempt's instance allocation was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FreshInstanceFault {
    /// Attempt numbers start at 1 (`ATH-FAIL-007`: an attempt is a record of a run).
    #[error("attempt number {0} is invalid; attempts start at 1")]
    AttemptNumber(u64),
    /// A row already binds this `(step, attempt)` pair — two instances would share
    /// one attempt, which is exactly what the rule forbids.
    #[error("attempt {attempt} of {step} is already bound to {instance}")]
    AttemptAlreadyBound {
        /// The step.
        step: athena_types::PlanStepId,
        /// The contested attempt.
        attempt: u64,
        /// The row already holding it.
        instance: athena_types::InstanceId,
    },
    /// The requested attempt number is not new; the recorded history runs at or
    /// beyond it.
    #[error("attempt {requested} of {step} is not new; {instance} already records {recorded}")]
    AttemptNotNew {
        /// The step.
        step: athena_types::PlanStepId,
        /// The attempt already recorded.
        recorded: u64,
        /// The attempted number.
        requested: u64,
        /// The row holding it.
        instance: athena_types::InstanceId,
    },
}

#[cfg(test)]
mod orphan_tests;
