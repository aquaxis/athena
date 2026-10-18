//! Task state machine (spec section 17.1).

use athena_types::{Actor, TaskState};

/// What causes a Task transition. The `(from, trigger)` pair is the table key, because several
/// rows share a `from`/`to` pair and differ only in trigger — `CANCELLING -> CANCELLED` arrives
/// either by clean stop or by forced kill, and the Result must record which (ATH-STATE-016).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TaskTrigger {
    /// Task submitted.
    Submitted,
    /// Cancelled before execution.
    CancelledBeforeStart,
    /// Execution slot acquired.
    SlotAcquired,
    /// Cancellation requested.
    CancelRequested,
    /// An approval request arose.
    ApprovalRequired,
    /// Human clarification needed.
    ClarificationRequired,
    /// A Result arrived.
    ResultReceived,
    /// Unrecoverable error.
    UnrecoverableError,
    /// Approval granted.
    ApprovalGranted,
    /// Approval rejected; alternatives are considered (ATH-STATE-015).
    ApprovalRejected,
    /// Approval expired, which denies like rejection (ATH-HITL-005).
    ApprovalExpired,
    /// Clarification answered.
    ClarificationAnswered,
    /// The answer requires a plan change.
    AnswerRequiresReplan,
    /// Clarification deadline passed.
    ClarificationExpired,
    /// Completion criteria satisfied.
    CriteriaMet,
    /// Criteria unmet but budget remains.
    CriteriaUnmetWithBudget,
    /// A retryable failure.
    RetryableFailure,
    /// Budget exhausted.
    BudgetExhausted,
    /// A new Plan revision was settled.
    NewPlanSettled,
    /// Human must choose between alternatives.
    AlternativeNeedsHuman,
    /// No alternative plan exists.
    NoAlternativePlan,
    /// Re-execution began.
    RetryStarted,
    /// Attempt ceiling reached.
    AttemptsExhausted,
    /// Targeted instances stopped cleanly.
    InstancesStopped,
    /// Stop deadline passed; instances were force-killed (ATH-STATE-016).
    StopTimedOut,
}

/// One row of the transition table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskTransition {
    /// Resulting state.
    pub to: TaskState,
    /// Event the transition must emit (ATH-STATE-003).
    pub event: &'static str,
    /// Who may request it. Recorded so the requester can be validated, since an agent may
    /// request but never perform a transition (ATH-STATE-001).
    pub requester: Actor,
}

/// Resolve a Task transition.
///
/// Returns `None` when the table has no such row, which the caller must surface as
/// `ValidationError` (ATH-STATE-002). Written as an exhaustive `match` so that adding a state
/// to `TaskState` fails to compile until every pairing is considered.
#[must_use]
pub fn task_transition(from: TaskState, trigger: TaskTrigger) -> Option<TaskTransition> {
    use Actor::*;
    use TaskState as S;
    use TaskTrigger as T;

    let row = |to, event, requester| {
        Some(TaskTransition {
            to,
            event,
            requester,
        })
    };

    match (from, trigger) {
        (S::Created, T::Submitted) => row(S::Queued, "TaskQueued", CoreAgent),
        (S::Created, T::CancelledBeforeStart) => row(S::Cancelled, "TaskCancelled", Human),

        (S::Queued, T::SlotAcquired) => row(S::Running, "TaskStarted", Scheduler),
        (S::Queued, T::CancelRequested) => row(S::Cancelling, "TaskCancelling", Human),

        (S::Running, T::ApprovalRequired) => row(S::WaitingApproval, "ApprovalRequested", System),
        (S::Running, T::ClarificationRequired) => {
            row(S::WaitingInput, "ClarificationRequested", CoreAgent)
        }
        (S::Running, T::ResultReceived) => row(S::Evaluating, "TaskResultReceived", CoreAgent),
        (S::Running, T::CancelRequested) => row(S::Cancelling, "TaskCancelling", Human),
        (S::Running, T::UnrecoverableError) => row(S::Failed, "TaskFailed", CoreAgent),

        (S::WaitingApproval, T::ApprovalGranted) => row(S::Running, "ApprovalGranted", Human),
        (S::WaitingApproval, T::ApprovalRejected) => {
            // ATH-STATE-015: rejection reaches REPLANNING so an alternative can be found; it
            // must not go straight to FAILED.
            row(S::Replanning, "TaskReplanning", CoreAgent)
        }
        (S::WaitingApproval, T::ApprovalExpired) => row(S::Failed, "ApprovalExpired", System),
        (S::WaitingApproval, T::CancelRequested) => row(S::Cancelling, "TaskCancelling", Human),

        (S::WaitingInput, T::ClarificationAnswered) => {
            row(S::Running, "ClarificationAnswered", Human)
        }
        (S::WaitingInput, T::AnswerRequiresReplan) => {
            row(S::Replanning, "TaskReplanning", CoreAgent)
        }
        (S::WaitingInput, T::CancelRequested) => row(S::Cancelling, "TaskCancelling", Human),
        (S::WaitingInput, T::ClarificationExpired) => row(S::Failed, "TaskFailed", System),

        (S::Evaluating, T::CriteriaMet) => row(S::Success, "TaskCompleted", CoreAgent),
        (S::Evaluating, T::ApprovalRequired) => {
            row(S::WaitingApproval, "ApprovalRequested", System)
        }
        (S::Evaluating, T::CriteriaUnmetWithBudget) => {
            row(S::Replanning, "TaskReplanning", CoreAgent)
        }
        (S::Evaluating, T::RetryableFailure) => row(S::Retrying, "TaskRetrying", CoreAgent),
        (S::Evaluating, T::BudgetExhausted) => row(S::Failed, "TaskFailed", CoreAgent),
        (S::Evaluating, T::CancelRequested) => row(S::Cancelling, "TaskCancelling", Human),

        (S::Replanning, T::NewPlanSettled) => row(S::Running, "TaskStarted", CoreAgent),
        (S::Replanning, T::AlternativeNeedsHuman) => {
            row(S::WaitingInput, "ClarificationRequested", CoreAgent)
        }
        (S::Replanning, T::NoAlternativePlan) => row(S::Failed, "TaskFailed", CoreAgent),
        (S::Replanning, T::CancelRequested) => row(S::Cancelling, "TaskCancelling", Human),

        (S::Retrying, T::RetryStarted) => row(S::Running, "TaskStarted", Scheduler),
        (S::Retrying, T::AttemptsExhausted) => row(S::Failed, "TaskFailed", CoreAgent),
        (S::Retrying, T::CancelRequested) => row(S::Cancelling, "TaskCancelling", Human),

        (S::Cancelling, T::InstancesStopped) => row(S::Cancelled, "TaskCancelled", Runtime),
        (S::Cancelling, T::StopTimedOut) => row(S::Cancelled, "TaskCancelled", System),

        // Terminal states accept nothing; every other pairing is absent from the table.
        _ => None,
    }
}

/// Derive a Task's state from its PlanSteps' states (spec section 17.1).
///
/// Task state is an aggregate, so the precedence order must be total for the derivation to be
/// single-valued. `None` means no step state maps into the precedence chain, which happens when
/// every step is terminal and the Task's own state is decided by evaluation instead.
#[must_use]
pub fn derive_task_state(step_states: &[TaskState]) -> Option<TaskState> {
    step_states
        .iter()
        .copied()
        .filter(|s| !s.is_terminal() && *s != TaskState::Created)
        .min_by_key(TaskState::aggregation_rank)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every row of the spec section 17.1 table: `(from, trigger, to, event)`.
    ///
    /// ATH-TEST-013 requires covering all of them, so the table is restated here rather than
    /// derived from the implementation — a shared helper would let one typo satisfy both.
    const SPEC_ROWS: &[(TaskState, TaskTrigger, TaskState, &str)] = &[
        (
            TaskState::Created,
            TaskTrigger::Submitted,
            TaskState::Queued,
            "TaskQueued",
        ),
        (
            TaskState::Created,
            TaskTrigger::CancelledBeforeStart,
            TaskState::Cancelled,
            "TaskCancelled",
        ),
        (
            TaskState::Queued,
            TaskTrigger::SlotAcquired,
            TaskState::Running,
            "TaskStarted",
        ),
        (
            TaskState::Queued,
            TaskTrigger::CancelRequested,
            TaskState::Cancelling,
            "TaskCancelling",
        ),
        (
            TaskState::Running,
            TaskTrigger::ApprovalRequired,
            TaskState::WaitingApproval,
            "ApprovalRequested",
        ),
        (
            TaskState::Running,
            TaskTrigger::ClarificationRequired,
            TaskState::WaitingInput,
            "ClarificationRequested",
        ),
        (
            TaskState::Running,
            TaskTrigger::ResultReceived,
            TaskState::Evaluating,
            "TaskResultReceived",
        ),
        (
            TaskState::Running,
            TaskTrigger::CancelRequested,
            TaskState::Cancelling,
            "TaskCancelling",
        ),
        (
            TaskState::Running,
            TaskTrigger::UnrecoverableError,
            TaskState::Failed,
            "TaskFailed",
        ),
        (
            TaskState::WaitingApproval,
            TaskTrigger::ApprovalGranted,
            TaskState::Running,
            "ApprovalGranted",
        ),
        (
            TaskState::WaitingApproval,
            TaskTrigger::ApprovalRejected,
            TaskState::Replanning,
            "TaskReplanning",
        ),
        (
            TaskState::WaitingApproval,
            TaskTrigger::ApprovalExpired,
            TaskState::Failed,
            "ApprovalExpired",
        ),
        (
            TaskState::WaitingApproval,
            TaskTrigger::CancelRequested,
            TaskState::Cancelling,
            "TaskCancelling",
        ),
        (
            TaskState::WaitingInput,
            TaskTrigger::ClarificationAnswered,
            TaskState::Running,
            "ClarificationAnswered",
        ),
        (
            TaskState::WaitingInput,
            TaskTrigger::AnswerRequiresReplan,
            TaskState::Replanning,
            "TaskReplanning",
        ),
        (
            TaskState::WaitingInput,
            TaskTrigger::CancelRequested,
            TaskState::Cancelling,
            "TaskCancelling",
        ),
        (
            TaskState::WaitingInput,
            TaskTrigger::ClarificationExpired,
            TaskState::Failed,
            "TaskFailed",
        ),
        (
            TaskState::Evaluating,
            TaskTrigger::CriteriaMet,
            TaskState::Success,
            "TaskCompleted",
        ),
        (
            TaskState::Evaluating,
            TaskTrigger::ApprovalRequired,
            TaskState::WaitingApproval,
            "ApprovalRequested",
        ),
        (
            TaskState::Evaluating,
            TaskTrigger::CriteriaUnmetWithBudget,
            TaskState::Replanning,
            "TaskReplanning",
        ),
        (
            TaskState::Evaluating,
            TaskTrigger::RetryableFailure,
            TaskState::Retrying,
            "TaskRetrying",
        ),
        (
            TaskState::Evaluating,
            TaskTrigger::BudgetExhausted,
            TaskState::Failed,
            "TaskFailed",
        ),
        (
            TaskState::Evaluating,
            TaskTrigger::CancelRequested,
            TaskState::Cancelling,
            "TaskCancelling",
        ),
        (
            TaskState::Replanning,
            TaskTrigger::NewPlanSettled,
            TaskState::Running,
            "TaskStarted",
        ),
        (
            TaskState::Replanning,
            TaskTrigger::AlternativeNeedsHuman,
            TaskState::WaitingInput,
            "ClarificationRequested",
        ),
        (
            TaskState::Replanning,
            TaskTrigger::NoAlternativePlan,
            TaskState::Failed,
            "TaskFailed",
        ),
        (
            TaskState::Replanning,
            TaskTrigger::CancelRequested,
            TaskState::Cancelling,
            "TaskCancelling",
        ),
        (
            TaskState::Retrying,
            TaskTrigger::RetryStarted,
            TaskState::Running,
            "TaskStarted",
        ),
        (
            TaskState::Retrying,
            TaskTrigger::AttemptsExhausted,
            TaskState::Failed,
            "TaskFailed",
        ),
        (
            TaskState::Retrying,
            TaskTrigger::CancelRequested,
            TaskState::Cancelling,
            "TaskCancelling",
        ),
        (
            TaskState::Cancelling,
            TaskTrigger::InstancesStopped,
            TaskState::Cancelled,
            "TaskCancelled",
        ),
        (
            TaskState::Cancelling,
            TaskTrigger::StopTimedOut,
            TaskState::Cancelled,
            "TaskCancelled",
        ),
    ];

    #[test]
    fn every_spec_row_resolves_exactly_as_tabulated() {
        for (from, trigger, to, event) in SPEC_ROWS {
            let resolved = task_transition(*from, *trigger)
                .unwrap_or_else(|| panic!("missing table row: {from} + {trigger:?}"));
            assert_eq!(resolved.to, *to, "wrong target for {from} + {trigger:?}");
            assert_eq!(
                resolved.event, *event,
                "wrong event for {from} + {trigger:?}"
            );
        }
    }

    #[test]
    fn the_spec_table_has_the_expected_row_count() {
        // Guards against a row being silently dropped from SPEC_ROWS.
        assert_eq!(SPEC_ROWS.len(), 32);
    }

    #[test]
    fn transitions_absent_from_the_table_are_refused() {
        // ATH-STATE-002: an undefined transition is a ValidationError, not a no-op.
        let mut refused = 0;
        for from in TaskState::ALL {
            for trigger in ALL_TRIGGERS {
                let tabulated = SPEC_ROWS
                    .iter()
                    .any(|(f, t, _, _)| f == from && t == trigger);
                let resolved = task_transition(*from, *trigger);
                if tabulated {
                    assert!(resolved.is_some(), "{from} + {trigger:?} should resolve");
                } else {
                    assert!(
                        resolved.is_none(),
                        "{from} + {trigger:?} is not in the spec table yet resolved to {resolved:?}"
                    );
                    refused += 1;
                }
            }
        }
        // The complement is far larger than the table, so a vacuous pass is implausible.
        assert!(refused > 200, "only {refused} pairings refused");
    }

    #[test]
    fn running_cannot_reach_success_without_evaluating() {
        // ATH-STATE-004: an unevaluated Result must never be treated as success.
        for trigger in ALL_TRIGGERS {
            if let Some(t) = task_transition(TaskState::Running, *trigger) {
                assert_ne!(
                    t.to,
                    TaskState::Success,
                    "RUNNING reached SUCCESS via {trigger:?}, bypassing EVALUATING"
                );
            }
        }
        // The only route into SUCCESS is from EVALUATING.
        for from in TaskState::ALL {
            for trigger in ALL_TRIGGERS {
                if let Some(t) = task_transition(*from, *trigger)
                    && t.to == TaskState::Success
                {
                    assert_eq!(*from, TaskState::Evaluating, "SUCCESS reached from {from}");
                }
            }
        }
    }

    #[test]
    fn approval_rejection_replans_while_expiry_fails() {
        // ATH-STATE-015: rejection seeks an alternative; expiry preserves the default-deny.
        let rejected = task_transition(TaskState::WaitingApproval, TaskTrigger::ApprovalRejected)
            .expect("row exists");
        assert_eq!(rejected.to, TaskState::Replanning);

        let expired = task_transition(TaskState::WaitingApproval, TaskTrigger::ApprovalExpired)
            .expect("row exists");
        assert_eq!(expired.to, TaskState::Failed);
    }

    #[test]
    fn cancelling_always_terminates_and_never_stalls() {
        // ATH-STATE-016: CANCELLING must not remain there; both routes reach CANCELLED.
        let clean = task_transition(TaskState::Cancelling, TaskTrigger::InstancesStopped)
            .expect("row exists");
        let forced =
            task_transition(TaskState::Cancelling, TaskTrigger::StopTimedOut).expect("row exists");
        assert_eq!(clean.to, TaskState::Cancelled);
        assert_eq!(forced.to, TaskState::Cancelled);
        // The requester differs, which is what lets the Result record whether side effects
        // are undetermined.
        assert_ne!(clean.requester, forced.requester);
    }

    #[test]
    fn terminal_states_accept_no_transition() {
        for from in TaskState::TERMINAL {
            for trigger in ALL_TRIGGERS {
                assert!(
                    task_transition(*from, *trigger).is_none(),
                    "terminal {from} accepted {trigger:?}"
                );
            }
        }
    }

    #[test]
    fn every_non_terminal_state_can_be_cancelled_once_started() {
        // ATH-STATE-005 presumes cancellation is reachable from the active states.
        for from in [
            TaskState::Queued,
            TaskState::Running,
            TaskState::WaitingApproval,
            TaskState::WaitingInput,
            TaskState::Evaluating,
            TaskState::Replanning,
            TaskState::Retrying,
        ] {
            let t = task_transition(from, TaskTrigger::CancelRequested)
                .unwrap_or_else(|| panic!("{from} must accept cancellation"));
            assert_eq!(t.to, TaskState::Cancelling);
        }
    }

    #[test]
    fn derivation_applies_the_precedence_order() {
        // Spec section 17.1: CANCELLING outranks everything, QUEUED is weakest.
        assert_eq!(
            derive_task_state(&[TaskState::Running, TaskState::Cancelling]),
            Some(TaskState::Cancelling)
        );
        assert_eq!(
            derive_task_state(&[TaskState::Queued, TaskState::Running]),
            Some(TaskState::Running)
        );
        assert_eq!(
            derive_task_state(&[TaskState::Evaluating, TaskState::WaitingApproval]),
            Some(TaskState::WaitingApproval)
        );
        assert_eq!(
            derive_task_state(&[TaskState::WaitingInput, TaskState::Retrying]),
            Some(TaskState::WaitingInput)
        );
    }

    #[test]
    fn derivation_ignores_terminal_step_states() {
        assert_eq!(derive_task_state(&[TaskState::Success]), None);
        assert_eq!(derive_task_state(&[]), None);
        assert_eq!(
            derive_task_state(&[TaskState::Success, TaskState::Running]),
            Some(TaskState::Running)
        );
    }

    /// Every trigger, for complement testing.
    const ALL_TRIGGERS: &[TaskTrigger] = &[
        TaskTrigger::Submitted,
        TaskTrigger::CancelledBeforeStart,
        TaskTrigger::SlotAcquired,
        TaskTrigger::CancelRequested,
        TaskTrigger::ApprovalRequired,
        TaskTrigger::ClarificationRequired,
        TaskTrigger::ResultReceived,
        TaskTrigger::UnrecoverableError,
        TaskTrigger::ApprovalGranted,
        TaskTrigger::ApprovalRejected,
        TaskTrigger::ApprovalExpired,
        TaskTrigger::ClarificationAnswered,
        TaskTrigger::AnswerRequiresReplan,
        TaskTrigger::ClarificationExpired,
        TaskTrigger::CriteriaMet,
        TaskTrigger::CriteriaUnmetWithBudget,
        TaskTrigger::RetryableFailure,
        TaskTrigger::BudgetExhausted,
        TaskTrigger::NewPlanSettled,
        TaskTrigger::AlternativeNeedsHuman,
        TaskTrigger::NoAlternativePlan,
        TaskTrigger::RetryStarted,
        TaskTrigger::AttemptsExhausted,
        TaskTrigger::InstancesStopped,
        TaskTrigger::StopTimedOut,
    ];
}
