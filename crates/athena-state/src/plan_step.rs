//! PlanStep state machine (spec section 17.5).
//!
//! The PlanStep is the Scheduler's dispatch unit and the Worker's unit of work
//! (spec section 8.2.1).

use athena_types::{Actor, PlanStepState};

/// What causes a PlanStep transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PlanStepTrigger {
    /// Every dependency is terminal and successful.
    DependenciesSatisfied,
    /// Re-planning made the step unnecessary.
    SkippedByReplan,
    /// The Agent Instance finished starting.
    InstanceStarted,
    /// An approval is pending and the instance was released.
    SuspendedForApproval,
    /// A Result arrived and passed evaluation.
    ResultAccepted,
    /// The Result failed, timed out, or the instance failed.
    Failed,
    /// The owning instance vanished (restart or heartbeat lapse).
    InstanceVanished,
    /// Approval was obtained.
    ApprovalGranted,
    /// Approval was rejected or expired.
    ApprovalDenied,
    /// Verification completed and a retry is permitted.
    VerifiedRetryAllowed,
    /// No retry is possible.
    RetryImpossible,
    /// A new attempt was scheduled.
    RetryScheduled,
}

/// One row of the transition table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanStepTransition {
    /// Resulting state.
    pub to: PlanStepState,
    /// Event the transition must emit.
    pub event: &'static str,
    /// Who may request it.
    pub requester: Actor,
}

/// Resolve a PlanStep transition. `None` means the table has no such row (ATH-STATE-002).
#[must_use]
pub fn plan_step_transition(
    from: PlanStepState,
    trigger: PlanStepTrigger,
) -> Option<PlanStepTransition> {
    use Actor::*;
    use PlanStepState as S;
    use PlanStepTrigger as T;

    let row = |to, event, requester| {
        Some(PlanStepTransition {
            to,
            event,
            requester,
        })
    };

    match (from, trigger) {
        (S::Pending, T::DependenciesSatisfied) => row(S::Ready, "PlanStepReady", Scheduler),
        (S::Pending, T::SkippedByReplan) => row(S::Skipped, "PlanStepSkipped", CoreAgent),

        (S::Ready, T::InstanceStarted) => row(S::Running, "PlanStepStarted", Scheduler),
        (S::Ready, T::SkippedByReplan) => row(S::Skipped, "PlanStepSkipped", CoreAgent),

        (S::Running, T::SuspendedForApproval) => row(S::Suspended, "AgentSuspended", System),
        (S::Running, T::ResultAccepted) => row(S::Success, "PlanStepCompleted", CoreAgent),
        (S::Running, T::Failed) => row(S::Failed, "PlanStepFailed", System),
        // The instance's side effects are undetermined here, which is why ORPHANED exists as a
        // distinct state rather than collapsing into FAILED (ATH-STATE-029).
        (S::Running, T::InstanceVanished) => row(S::Orphaned, "PlanStepOrphaned", System),

        (S::Suspended, T::ApprovalGranted) => row(S::Running, "AgentResumed", System),
        (S::Suspended, T::ApprovalDenied) => row(S::Failed, "PlanStepFailed", System),

        // ATH-STATE-029: a retry is permitted only once Verification has established what the
        // vanished instance actually did.
        (S::Orphaned, T::VerifiedRetryAllowed) => row(S::Ready, "PlanStepReady", System),
        (S::Orphaned, T::RetryImpossible) => row(S::Failed, "PlanStepFailed", CoreAgent),

        (S::Failed, T::RetryScheduled) => row(S::Ready, "PlanStepReady", Scheduler),

        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every row of the spec section 17.5 table.
    const SPEC_ROWS: &[(PlanStepState, PlanStepTrigger, PlanStepState, &str)] = &[
        (
            PlanStepState::Pending,
            PlanStepTrigger::DependenciesSatisfied,
            PlanStepState::Ready,
            "PlanStepReady",
        ),
        (
            PlanStepState::Pending,
            PlanStepTrigger::SkippedByReplan,
            PlanStepState::Skipped,
            "PlanStepSkipped",
        ),
        (
            PlanStepState::Ready,
            PlanStepTrigger::InstanceStarted,
            PlanStepState::Running,
            "PlanStepStarted",
        ),
        (
            PlanStepState::Ready,
            PlanStepTrigger::SkippedByReplan,
            PlanStepState::Skipped,
            "PlanStepSkipped",
        ),
        (
            PlanStepState::Running,
            PlanStepTrigger::SuspendedForApproval,
            PlanStepState::Suspended,
            "AgentSuspended",
        ),
        (
            PlanStepState::Running,
            PlanStepTrigger::ResultAccepted,
            PlanStepState::Success,
            "PlanStepCompleted",
        ),
        (
            PlanStepState::Running,
            PlanStepTrigger::Failed,
            PlanStepState::Failed,
            "PlanStepFailed",
        ),
        (
            PlanStepState::Running,
            PlanStepTrigger::InstanceVanished,
            PlanStepState::Orphaned,
            "PlanStepOrphaned",
        ),
        (
            PlanStepState::Suspended,
            PlanStepTrigger::ApprovalGranted,
            PlanStepState::Running,
            "AgentResumed",
        ),
        (
            PlanStepState::Suspended,
            PlanStepTrigger::ApprovalDenied,
            PlanStepState::Failed,
            "PlanStepFailed",
        ),
        (
            PlanStepState::Orphaned,
            PlanStepTrigger::VerifiedRetryAllowed,
            PlanStepState::Ready,
            "PlanStepReady",
        ),
        (
            PlanStepState::Orphaned,
            PlanStepTrigger::RetryImpossible,
            PlanStepState::Failed,
            "PlanStepFailed",
        ),
        (
            PlanStepState::Failed,
            PlanStepTrigger::RetryScheduled,
            PlanStepState::Ready,
            "PlanStepReady",
        ),
    ];

    const ALL_TRIGGERS: &[PlanStepTrigger] = &[
        PlanStepTrigger::DependenciesSatisfied,
        PlanStepTrigger::SkippedByReplan,
        PlanStepTrigger::InstanceStarted,
        PlanStepTrigger::SuspendedForApproval,
        PlanStepTrigger::ResultAccepted,
        PlanStepTrigger::Failed,
        PlanStepTrigger::InstanceVanished,
        PlanStepTrigger::ApprovalGranted,
        PlanStepTrigger::ApprovalDenied,
        PlanStepTrigger::VerifiedRetryAllowed,
        PlanStepTrigger::RetryImpossible,
        PlanStepTrigger::RetryScheduled,
    ];

    #[test]
    fn every_spec_row_resolves_exactly_as_tabulated() {
        assert_eq!(SPEC_ROWS.len(), 13);
        for (from, trigger, to, event) in SPEC_ROWS {
            let resolved = plan_step_transition(*from, *trigger)
                .unwrap_or_else(|| panic!("missing row: {from} + {trigger:?}"));
            assert_eq!(resolved.to, *to, "wrong target for {from} + {trigger:?}");
            assert_eq!(resolved.event, *event);
        }
    }

    #[test]
    fn transitions_absent_from_the_table_are_refused() {
        let mut refused = 0;
        for from in PlanStepState::ALL {
            for trigger in ALL_TRIGGERS {
                let tabulated = SPEC_ROWS
                    .iter()
                    .any(|(f, t, _, _)| f == from && t == trigger);
                let resolved = plan_step_transition(*from, *trigger);
                assert_eq!(resolved.is_some(), tabulated, "{from} + {trigger:?}");
                if !tabulated {
                    refused += 1;
                }
            }
        }
        assert!(refused > 70, "only {refused} pairings refused");
    }

    #[test]
    fn orphaned_reaches_ready_only_through_verification() {
        // ATH-STATE-029 / ATH-CORE-048: side effects are unknown, so a bare retry is refused.
        for trigger in ALL_TRIGGERS {
            if let Some(t) = plan_step_transition(PlanStepState::Orphaned, *trigger)
                && t.to == PlanStepState::Ready
            {
                assert_eq!(
                    *trigger,
                    PlanStepTrigger::VerifiedRetryAllowed,
                    "ORPHANED reached READY via {trigger:?} without Verification"
                );
            }
        }
        assert!(
            plan_step_transition(PlanStepState::Orphaned, PlanStepTrigger::RetryScheduled)
                .is_none(),
            "an ORPHANED step must not be retried by plain scheduling"
        );
    }

    #[test]
    fn a_running_step_cannot_be_skipped_by_replanning() {
        // ATH-STATE-028: only PENDING and READY may be deleted; RUNNING goes through cancel.
        assert!(
            plan_step_transition(PlanStepState::Running, PlanStepTrigger::SkippedByReplan)
                .is_none()
        );
        assert!(
            plan_step_transition(PlanStepState::Pending, PlanStepTrigger::SkippedByReplan)
                .is_some()
        );
        assert!(
            plan_step_transition(PlanStepState::Ready, PlanStepTrigger::SkippedByReplan).is_some()
        );
    }

    #[test]
    fn a_failed_step_can_be_retried_as_a_new_attempt() {
        // ATH-STATE-030 / ATH-FAIL-007: the retry re-enters READY so a fresh instance starts.
        let t = plan_step_transition(PlanStepState::Failed, PlanStepTrigger::RetryScheduled)
            .expect("row exists");
        assert_eq!(t.to, PlanStepState::Ready);
    }

    #[test]
    fn suspension_resumes_into_running_not_ready() {
        // ATH-STATE-021: resumption continues the same step, it does not re-dispatch it.
        let t = plan_step_transition(PlanStepState::Suspended, PlanStepTrigger::ApprovalGranted)
            .expect("row exists");
        assert_eq!(t.to, PlanStepState::Running);
    }

    #[test]
    fn success_and_skipped_are_absorbing() {
        for from in [PlanStepState::Success, PlanStepState::Skipped] {
            for trigger in ALL_TRIGGERS {
                assert!(
                    plan_step_transition(from, *trigger).is_none(),
                    "terminal {from} accepted {trigger:?}"
                );
            }
        }
    }
}
