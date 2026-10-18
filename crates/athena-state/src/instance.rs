//! Agent Instance state machine (spec section 17.2), including the Core Agent special rules
//! of section 17.2.1.

use athena_types::{Actor, AgentInstanceState};

/// What causes an Agent Instance transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InstanceTrigger {
    /// Start requested.
    StartRequested,
    /// Startup completed.
    Started,
    /// Startup timed out or failed.
    StartFailed,
    /// An `instruction` was received.
    InstructionReceived,
    /// Stop requested.
    StopRequested,
    /// The process terminated abnormally.
    ProcessCrashed,
    /// Awaiting a subordinate's reply.
    AwaitingSubordinate,
    /// A `report` was sent.
    ReportSent,
    /// An execution error or execution timeout occurred.
    ExecutionFailed,
    /// A reply arrived.
    ReplyReceived,
    /// Moving to await an approval.
    SuspendForApproval,
    /// The reply deadline passed.
    ReplyDeadlinePassed,
    /// Approval obtained; resume.
    ApprovalGranted,
    /// Approval denied, expired, or cancelled.
    ApprovalDeniedOrCancelled,
    /// Process exit confirmed.
    ExitConfirmed,
    /// Stop deadline passed; force-killed.
    StopTimedOut,
    /// Resources reclaimed after failure.
    ResourcesReclaimed,
}

/// One row of the transition table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstanceTransition {
    /// Resulting state.
    pub to: AgentInstanceState,
    /// Event the transition must emit.
    pub event: &'static str,
    /// Who may request it.
    pub requester: Actor,
}

/// Resolve an Agent Instance transition for a **subordinate** agent.
///
/// The Core Agent Instance does not follow this machine unchanged; use
/// [`core_agent_transition_is_permitted`] to screen a request for instance #0 first
/// (spec section 17.2.1).
#[must_use]
pub fn instance_transition(
    from: AgentInstanceState,
    trigger: InstanceTrigger,
) -> Option<InstanceTransition> {
    use Actor::*;
    use AgentInstanceState as S;
    use InstanceTrigger as T;

    let row = |to, event, requester| {
        Some(InstanceTransition {
            to,
            event,
            requester,
        })
    };

    match (from, trigger) {
        (S::Created, T::StartRequested) => row(S::Starting, "AgentStarting", Runtime),

        (S::Starting, T::Started) => row(S::Idle, "AgentStarted", Runtime),
        (S::Starting, T::StartFailed) => row(S::Failed, "AgentFailed", Runtime),

        (S::Idle, T::InstructionReceived) => row(S::Busy, "AgentMessageReceived", System),
        (S::Idle, T::StopRequested) => row(S::Stopping, "AgentStopping", Runtime),
        (S::Idle, T::ProcessCrashed) => row(S::Failed, "AgentFailed", Runtime),

        (S::Busy, T::AwaitingSubordinate) => row(S::Waiting, "AgentWaiting", Agent),
        (S::Busy, T::ReportSent) => row(S::Idle, "AgentMessageSent", Agent),
        (S::Busy, T::StopRequested) => row(S::Stopping, "AgentStopping", Runtime),
        (S::Busy, T::ExecutionFailed) => row(S::Failed, "AgentFailed", Runtime),

        (S::Waiting, T::ReplyReceived) => row(S::Busy, "AgentMessageReceived", System),
        (S::Waiting, T::SuspendForApproval) => row(S::Suspended, "AgentSuspended", System),
        (S::Waiting, T::StopRequested) => row(S::Stopping, "AgentStopping", Runtime),
        (S::Waiting, T::ReplyDeadlinePassed) => row(S::Failed, "AgentFailed", Runtime),

        (S::Suspended, T::ApprovalGranted) => row(S::Busy, "AgentResumed", System),
        (S::Suspended, T::ApprovalDeniedOrCancelled) => row(S::Stopping, "AgentStopping", Runtime),

        (S::Stopping, T::ExitConfirmed) => row(S::Stopped, "AgentStopped", Runtime),
        (S::Stopping, T::StopTimedOut) => row(S::Stopped, "AgentStopped", System),

        // ATH-STATE-007: FAILED is quasi-terminal; resources are still reclaimed through
        // STOPPING so the instance reaches STOPPED.
        (S::Failed, T::ResourcesReclaimed) => row(S::Stopping, "AgentStopping", Runtime),

        _ => None,
    }
}

/// Screen a transition request against the Core Agent Instance special rules
/// (spec section 17.2.1).
///
/// Returns `false` when the request must be refused for instance #0 even though the subordinate
/// table permits it. Two cases matter most: a reply timeout must never fail the Core Agent
/// (ATH-STATE-023), and `SUSPENDED` does not apply to it at all.
#[must_use]
pub fn core_agent_transition_is_permitted(trigger: InstanceTrigger) -> bool {
    !matches!(
        trigger,
        // ATH-STATE-023: a subordinate's silence is an observation delivered as
        // AgentMessageTimeout, not a failure of the Core Agent.
        InstanceTrigger::ReplyDeadlinePassed
            // Spec section 17.2.1: SUSPENDED is not applied to instance #0.
            | InstanceTrigger::SuspendForApproval
            | InstanceTrigger::ApprovalGranted
            | InstanceTrigger::ApprovalDeniedOrCancelled
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use AgentInstanceState as S;
    use InstanceTrigger as T;

    /// Every row of the spec section 17.2 table.
    const SPEC_ROWS: &[(S, T, S, &str)] = &[
        (S::Created, T::StartRequested, S::Starting, "AgentStarting"),
        (S::Starting, T::Started, S::Idle, "AgentStarted"),
        (S::Starting, T::StartFailed, S::Failed, "AgentFailed"),
        (
            S::Idle,
            T::InstructionReceived,
            S::Busy,
            "AgentMessageReceived",
        ),
        (S::Idle, T::StopRequested, S::Stopping, "AgentStopping"),
        (S::Idle, T::ProcessCrashed, S::Failed, "AgentFailed"),
        (S::Busy, T::AwaitingSubordinate, S::Waiting, "AgentWaiting"),
        (S::Busy, T::ReportSent, S::Idle, "AgentMessageSent"),
        (S::Busy, T::StopRequested, S::Stopping, "AgentStopping"),
        (S::Busy, T::ExecutionFailed, S::Failed, "AgentFailed"),
        (
            S::Waiting,
            T::ReplyReceived,
            S::Busy,
            "AgentMessageReceived",
        ),
        (
            S::Waiting,
            T::SuspendForApproval,
            S::Suspended,
            "AgentSuspended",
        ),
        (S::Waiting, T::StopRequested, S::Stopping, "AgentStopping"),
        (S::Waiting, T::ReplyDeadlinePassed, S::Failed, "AgentFailed"),
        (S::Suspended, T::ApprovalGranted, S::Busy, "AgentResumed"),
        (
            S::Suspended,
            T::ApprovalDeniedOrCancelled,
            S::Stopping,
            "AgentStopping",
        ),
        (S::Stopping, T::ExitConfirmed, S::Stopped, "AgentStopped"),
        (S::Stopping, T::StopTimedOut, S::Stopped, "AgentStopped"),
        (
            S::Failed,
            T::ResourcesReclaimed,
            S::Stopping,
            "AgentStopping",
        ),
    ];

    const ALL_TRIGGERS: &[T] = &[
        T::StartRequested,
        T::Started,
        T::StartFailed,
        T::InstructionReceived,
        T::StopRequested,
        T::ProcessCrashed,
        T::AwaitingSubordinate,
        T::ReportSent,
        T::ExecutionFailed,
        T::ReplyReceived,
        T::SuspendForApproval,
        T::ReplyDeadlinePassed,
        T::ApprovalGranted,
        T::ApprovalDeniedOrCancelled,
        T::ExitConfirmed,
        T::StopTimedOut,
        T::ResourcesReclaimed,
    ];

    #[test]
    fn every_spec_row_resolves_exactly_as_tabulated() {
        assert_eq!(SPEC_ROWS.len(), 19);
        for (from, trigger, to, event) in SPEC_ROWS {
            let resolved = instance_transition(*from, *trigger)
                .unwrap_or_else(|| panic!("missing row: {from} + {trigger:?}"));
            assert_eq!(resolved.to, *to, "wrong target for {from} + {trigger:?}");
            assert_eq!(resolved.event, *event);
        }
    }

    #[test]
    fn transitions_absent_from_the_table_are_refused() {
        let mut refused = 0;
        for from in S::ALL {
            for trigger in ALL_TRIGGERS {
                let tabulated = SPEC_ROWS
                    .iter()
                    .any(|(f, t, _, _)| f == from && t == trigger);
                assert_eq!(
                    instance_transition(*from, *trigger).is_some(),
                    tabulated,
                    "{from} + {trigger:?}"
                );
                if !tabulated {
                    refused += 1;
                }
            }
        }
        assert!(refused > 120, "only {refused} pairings refused");
    }

    #[test]
    fn failed_instances_still_reach_stopped() {
        // ATH-STATE-007: resources must be reclaimed, so FAILED is not a dead end.
        let reclaim = instance_transition(S::Failed, T::ResourcesReclaimed).expect("row exists");
        assert_eq!(reclaim.to, S::Stopping);
        let exit = instance_transition(S::Stopping, T::ExitConfirmed).expect("row exists");
        assert_eq!(exit.to, S::Stopped);
    }

    #[test]
    fn stopping_always_terminates() {
        // Both the clean and the forced route reach STOPPED (ATH-FAIL-015).
        assert_eq!(
            instance_transition(S::Stopping, T::ExitConfirmed).map(|t| t.to),
            Some(S::Stopped)
        );
        assert_eq!(
            instance_transition(S::Stopping, T::StopTimedOut).map(|t| t.to),
            Some(S::Stopped)
        );
    }

    #[test]
    fn stopped_is_absorbing() {
        for trigger in ALL_TRIGGERS {
            assert!(instance_transition(S::Stopped, *trigger).is_none());
        }
    }

    #[test]
    fn a_busy_instance_cannot_accept_another_instruction() {
        // ATH-STATE-006: a second instruction must be queued or refused, never applied.
        assert!(instance_transition(S::Busy, T::InstructionReceived).is_none());
    }

    #[test]
    fn core_agent_is_never_failed_by_a_reply_timeout() {
        // ATH-STATE-023: the subordinate table would fail it; the special rule forbids that.
        assert_eq!(
            instance_transition(S::Waiting, T::ReplyDeadlinePassed).map(|t| t.to),
            Some(S::Failed),
            "the subordinate rule does fail on reply timeout"
        );
        assert!(
            !core_agent_transition_is_permitted(T::ReplyDeadlinePassed),
            "instance #0 must not be failed by a reply timeout"
        );
    }

    #[test]
    fn suspension_does_not_apply_to_the_core_agent() {
        // Spec section 17.2.1.
        for trigger in [
            T::SuspendForApproval,
            T::ApprovalGranted,
            T::ApprovalDeniedOrCancelled,
        ] {
            assert!(
                !core_agent_transition_is_permitted(trigger),
                "{trigger:?} must not apply to instance #0"
            );
        }
    }

    #[test]
    fn ordinary_lifecycle_triggers_remain_available_to_the_core_agent() {
        // The special rules are narrow: start, crash and stop still apply, which is what lets
        // Core Host supervise and restart instance #0 (ATH-CORE-021).
        for trigger in [
            T::StartRequested,
            T::Started,
            T::StartFailed,
            T::ProcessCrashed,
            T::StopRequested,
            T::ExitConfirmed,
            T::ResourcesReclaimed,
        ] {
            assert!(core_agent_transition_is_permitted(trigger), "{trigger:?}");
        }
    }
}
