//! Skill lifecycle state machine (spec section 17.3).
//!
//! State is tracked per `(id, version)` (ATH-STATE-026). Core Agent must never appear as the
//! requester: it may only *select* an `ENABLED` skill (ATH-STATE-025).

use super::enums::{Actor, SkillState};

/// What causes a Skill transition. Every variant is an administrative or Skill Manager action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SkillTrigger {
    /// Validation started.
    ValidationStarted,
    /// Validation succeeded.
    ValidationSucceeded,
    /// Validation failed.
    ValidationFailed,
    /// Enabled by an administrator.
    Enabled,
    /// Disabled by an administrator.
    Disabled,
    /// An update was requested.
    UpdateRequested,
    /// A new version was fetched.
    NewVersionFetched,
    /// The fetch failed.
    FetchFailed,
    /// The update completed.
    UpdateCompleted,
    /// Re-validation was requested.
    Revalidate,
    /// Removal was requested.
    Removed,
}

/// One row of the transition table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkillTransition {
    /// Resulting state.
    pub to: SkillState,
    /// Event the transition must emit.
    pub event: &'static str,
    /// Who may request it. Never `CoreAgent` (ATH-STATE-025).
    pub requester: Actor,
}

/// Resolve a Skill transition. `None` means the table has no such row (ATH-STATE-002).
#[must_use]
pub fn skill_transition(from: SkillState, trigger: SkillTrigger) -> Option<SkillTransition> {
    use Actor::*;
    use SkillState as S;
    use SkillTrigger as T;

    let row = |to, event, requester| {
        Some(SkillTransition {
            to,
            event,
            requester,
        })
    };

    match (from, trigger) {
        (S::Discovered, T::ValidationStarted) => row(S::Validating, "SkillValidating", System),

        (S::Validating, T::ValidationSucceeded) => row(S::Available, "SkillValidated", System),
        (S::Validating, T::ValidationFailed) => row(S::Failed, "SkillValidationFailed", System),

        // ATH-API-014: registration must not reach ENABLED before validation completes, which
        // holds because ENABLED is reachable only from AVAILABLE or DISABLED.
        (S::Available, T::Enabled) => row(S::Enabled, "SkillEnabled", Human),
        (S::Available, T::UpdateRequested) => row(S::Updating, "SkillUpdating", Human),
        (S::Available, T::Removed) => row(S::Removed, "SkillRemoved", Human),

        (S::Enabled, T::Disabled) => row(S::Disabled, "SkillDisabled", Human),
        (S::Enabled, T::UpdateRequested) => row(S::Updating, "SkillUpdating", System),
        (S::Enabled, T::Removed) => row(S::Removed, "SkillRemoved", Human),

        (S::Disabled, T::Enabled) => row(S::Enabled, "SkillEnabled", Human),
        (S::Disabled, T::UpdateRequested) => row(S::Updating, "SkillUpdating", System),
        (S::Disabled, T::Removed) => row(S::Removed, "SkillRemoved", Human),

        (S::Updating, T::NewVersionFetched) => row(S::Validating, "SkillValidating", System),
        (S::Updating, T::FetchFailed) => row(S::Failed, "SkillValidationFailed", System),
        (S::Updating, T::UpdateCompleted) => row(S::Available, "SkillUpdated", System),

        (S::Failed, T::Revalidate) => row(S::Validating, "SkillValidating", Human),
        (S::Failed, T::Removed) => row(S::Removed, "SkillRemoved", Human),

        _ => None,
    }
}

#[cfg(test)]
mod skill_state_tests {
    use super::{Actor, SkillState as S, SkillTrigger as T, skill_transition};

    /// Every row of the spec section 17.3 table.
    const SPEC_ROWS: &[(S, T, S, &str)] = &[
        (
            S::Discovered,
            T::ValidationStarted,
            S::Validating,
            "SkillValidating",
        ),
        (
            S::Validating,
            T::ValidationSucceeded,
            S::Available,
            "SkillValidated",
        ),
        (
            S::Validating,
            T::ValidationFailed,
            S::Failed,
            "SkillValidationFailed",
        ),
        (S::Available, T::Enabled, S::Enabled, "SkillEnabled"),
        (S::Enabled, T::Disabled, S::Disabled, "SkillDisabled"),
        (S::Disabled, T::Enabled, S::Enabled, "SkillEnabled"),
        (S::Enabled, T::UpdateRequested, S::Updating, "SkillUpdating"),
        (
            S::Disabled,
            T::UpdateRequested,
            S::Updating,
            "SkillUpdating",
        ),
        (
            S::Available,
            T::UpdateRequested,
            S::Updating,
            "SkillUpdating",
        ),
        (
            S::Updating,
            T::NewVersionFetched,
            S::Validating,
            "SkillValidating",
        ),
        (
            S::Updating,
            T::FetchFailed,
            S::Failed,
            "SkillValidationFailed",
        ),
        (
            S::Updating,
            T::UpdateCompleted,
            S::Available,
            "SkillUpdated",
        ),
        (S::Failed, T::Revalidate, S::Validating, "SkillValidating"),
        (S::Available, T::Removed, S::Removed, "SkillRemoved"),
        (S::Enabled, T::Removed, S::Removed, "SkillRemoved"),
        (S::Disabled, T::Removed, S::Removed, "SkillRemoved"),
        (S::Failed, T::Removed, S::Removed, "SkillRemoved"),
    ];

    const ALL_TRIGGERS: &[T] = &[
        T::ValidationStarted,
        T::ValidationSucceeded,
        T::ValidationFailed,
        T::Enabled,
        T::Disabled,
        T::UpdateRequested,
        T::NewVersionFetched,
        T::FetchFailed,
        T::UpdateCompleted,
        T::Revalidate,
        T::Removed,
    ];

    #[test]
    fn every_spec_row_resolves_exactly_as_tabulated() {
        assert_eq!(SPEC_ROWS.len(), 17);
        for (from, trigger, to, event) in SPEC_ROWS {
            let resolved = skill_transition(*from, *trigger)
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
                    skill_transition(*from, *trigger).is_some(),
                    tabulated,
                    "{from} + {trigger:?}"
                );
                if !tabulated {
                    refused += 1;
                }
            }
        }
        assert!(refused > 60, "only {refused} pairings refused");
    }

    #[test]
    fn core_agent_never_requests_a_lifecycle_transition() {
        // ATH-STATE-025: enable, disable, update and remove are admin operations.
        for from in S::ALL {
            for trigger in ALL_TRIGGERS {
                if let Some(t) = skill_transition(*from, *trigger) {
                    assert_ne!(
                        t.requester,
                        Actor::CoreAgent,
                        "{from} + {trigger:?} lists Core Agent as requester"
                    );
                }
            }
        }
    }

    #[test]
    fn enabled_is_unreachable_without_passing_validation() {
        // ATH-API-014 / ATH-SKILL-009: a freshly discovered skill cannot be enabled directly.
        assert!(skill_transition(S::Discovered, T::Enabled).is_none());
        assert!(skill_transition(S::Validating, T::Enabled).is_none());
        assert!(skill_transition(S::Failed, T::Enabled).is_none());
        // The only predecessors of ENABLED are AVAILABLE and DISABLED.
        for from in S::ALL {
            for trigger in ALL_TRIGGERS {
                if let Some(t) = skill_transition(*from, *trigger)
                    && t.to == S::Enabled
                {
                    assert!(
                        matches!(from, S::Available | S::Disabled),
                        "ENABLED reached from {from}"
                    );
                }
            }
        }
    }
}
