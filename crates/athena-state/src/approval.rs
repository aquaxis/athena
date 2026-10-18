//! Approval state machine (spec section 17.4).
//!
//! Consumption is performed by Core Host; Core Agent may only request it (ATH-STATE-027).

use athena_types::{Actor, ApprovalState};

/// What causes an Approval transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ApprovalTrigger {
    /// A human approved.
    Approved,
    /// A human rejected.
    Rejected,
    /// The response deadline passed.
    Expired,
    /// The covered operation ran and no uses remain.
    ConsumedLastUse,
    /// `scope.expires_at` passed before consumption.
    ScopeExpired,
}

/// One row of the transition table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApprovalTransition {
    /// Resulting state.
    pub to: ApprovalState,
    /// Event the transition must emit.
    pub event: &'static str,
    /// Who may request it.
    pub requester: Actor,
}

/// The state an Approval is created in, together with its Event (spec section 17.4).
///
/// Creation is modelled separately because the table's first row has no origin state.
#[must_use]
pub fn approval_created() -> ApprovalTransition {
    ApprovalTransition {
        to: ApprovalState::Requested,
        event: "ApprovalRequested",
        requester: Actor::System,
    }
}

/// Resolve an Approval transition. `None` means the table has no such row (ATH-STATE-002).
#[must_use]
pub fn approval_transition(
    from: ApprovalState,
    trigger: ApprovalTrigger,
) -> Option<ApprovalTransition> {
    use Actor::*;
    use ApprovalState as S;
    use ApprovalTrigger as T;

    let row = |to, event, requester| {
        Some(ApprovalTransition {
            to,
            event,
            requester,
        })
    };

    match (from, trigger) {
        (S::Requested, T::Approved) => row(S::Granted, "ApprovalGranted", Human),
        (S::Requested, T::Rejected) => row(S::Rejected, "ApprovalRejected", Human),
        (S::Requested, T::Expired) => row(S::Expired, "ApprovalExpired", System),

        // ATH-STATE-027: Core Host performs consumption.
        (S::Granted, T::ConsumedLastUse) => row(S::Consumed, "ApprovalConsumed", System),
        (S::Granted, T::ScopeExpired) => row(S::Expired, "ApprovalExpired", System),

        _ => None,
    }
}

/// Whether a granted pattern-scoped approval still has uses left (ATH-STATE-010).
///
/// `operation_only` approvals carry exactly one use, so the first consumption exhausts them.
#[must_use]
pub fn has_remaining_uses(uses_remaining: u64) -> bool {
    uses_remaining > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use ApprovalState as S;
    use ApprovalTrigger as T;

    /// Every row of the spec section 17.4 table, excluding creation.
    const SPEC_ROWS: &[(S, T, S, &str)] = &[
        (S::Requested, T::Approved, S::Granted, "ApprovalGranted"),
        (S::Requested, T::Rejected, S::Rejected, "ApprovalRejected"),
        (S::Requested, T::Expired, S::Expired, "ApprovalExpired"),
        (
            S::Granted,
            T::ConsumedLastUse,
            S::Consumed,
            "ApprovalConsumed",
        ),
        (S::Granted, T::ScopeExpired, S::Expired, "ApprovalExpired"),
    ];

    const ALL_TRIGGERS: &[T] = &[
        T::Approved,
        T::Rejected,
        T::Expired,
        T::ConsumedLastUse,
        T::ScopeExpired,
    ];

    #[test]
    fn every_spec_row_resolves_exactly_as_tabulated() {
        assert_eq!(SPEC_ROWS.len(), 5);
        for (from, trigger, to, event) in SPEC_ROWS {
            let resolved = approval_transition(*from, *trigger)
                .unwrap_or_else(|| panic!("missing row: {from} + {trigger:?}"));
            assert_eq!(resolved.to, *to, "wrong target for {from} + {trigger:?}");
            assert_eq!(resolved.event, *event);
        }
    }

    #[test]
    fn creation_enters_requested() {
        let created = approval_created();
        assert_eq!(created.to, S::Requested);
        assert_eq!(created.event, "ApprovalRequested");
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
                    approval_transition(*from, *trigger).is_some(),
                    tabulated,
                    "{from} + {trigger:?}"
                );
                if !tabulated {
                    refused += 1;
                }
            }
        }
        assert!(refused >= 20, "only {refused} pairings refused");
    }

    #[test]
    fn a_granted_approval_cannot_be_approved_again() {
        // ATH-HITL-007: one approval covers one requested operation.
        assert!(approval_transition(S::Granted, T::Approved).is_none());
        assert!(approval_transition(S::Granted, T::Rejected).is_none());
    }

    #[test]
    fn terminal_approvals_reject_every_operation() {
        // ATH-API-017: the API answers 409 for these, which relies on the table refusing them.
        for from in S::TERMINAL {
            for trigger in ALL_TRIGGERS {
                assert!(
                    approval_transition(*from, *trigger).is_none(),
                    "terminal {from} accepted {trigger:?}"
                );
            }
        }
    }

    #[test]
    fn consumption_is_performed_by_core_host_not_the_core_agent() {
        // ATH-STATE-027.
        let consumed = approval_transition(S::Granted, T::ConsumedLastUse).expect("row exists");
        assert_eq!(consumed.requester, Actor::System);
        assert_ne!(consumed.requester, Actor::CoreAgent);
    }

    #[test]
    fn expiry_is_reachable_from_both_requested_and_granted() {
        // A grant that is never consumed still expires at scope.expires_at.
        assert_eq!(
            approval_transition(S::Requested, T::Expired).map(|t| t.to),
            Some(S::Expired)
        );
        assert_eq!(
            approval_transition(S::Granted, T::ScopeExpired).map(|t| t.to),
            Some(S::Expired)
        );
    }

    #[test]
    fn remaining_uses_gate_consumption() {
        // ATH-STATE-010: a pattern scope stays GRANTED until uses_remaining hits zero.
        assert!(has_remaining_uses(3));
        assert!(has_remaining_uses(1));
        assert!(!has_remaining_uses(0));
    }
}
