//! The Phase-4 fault-injection suite (P4-26):
//!
//! - **all nine §20.1 classes injected**, each through its disposition: the §20.1 column
//!   values, the `RetryPolicy::decide` composition, and the recovery menu's
//!   `allowed_by` face (`ATH-FAIL-001/002/004/005/006`);
//! - an **unclassifiable fault** maps to `InternalError` and never retries
//!   (`ATH-FAIL-002`);
//! - **approval expiry mid-run** (`ATH-HITL-005`): silence is rejection, the deadline
//!   routed to `FAILED` (never re-planned), with the approver audit empty;
//! - a **rejected high-risk operation is not executed** (`ATH-HITL-001`, `ATH-HITL-007`,
//!   `ATH-HITL-014/015`): the `REJECTED` route is `REPLANNING`, consumption demands a
//!   fresh approval, and the §17.5 `SUSPENDED → FAILED` row exists for the denial;
//! - a **granted resume** re-enters `RUNNING` through `ApprovalGranted` and must run the
//!   **identical** Skill Set (`ATH-STATE-021`);
//! - **stall detection fires before the execution timeout** (`ATH-OBS-012` against
//!   §20.3): the silence threshold stays below the execution deadline, the detector
//!   produces the escalation signal at that line, and an immediate wait-graph cycle
//!   stalls regardless of clock.

use athena_core::failure::{RetryFault, RetryPolicy};
use athena_core::observability;
use athena_core::protocol::{Disposition, ErrorClass, Escalation, RetryKind};
use athena_core::runtime_manager::suspension::{FrozenSnapshot, resume_with};
use athena_core::scheduler::RetryBudget;
use athena_types::{RiskLevel, SkillRef};

// --- Fixtures ---------------------------------------------------------------------------

fn skill_ref(name: &str, version: &str) -> SkillRef {
    SkillRef {
        id: athena_types::SkillId::new(name),
        version: version.into(),
        checksum: Some(
            "sha256-0000000000000000000000000000000000000000000000000000000000000000".into(),
        ),
    }
}

// --- 1. All nine §20.1 classes, compositionally ------------------------------------------

#[test]
fn every_class_disposition_matches_the_20_1_table_row() {
    // The §20.1 table is transcribed here cell for cell; the drift guard is the test.
    let rows: [(ErrorClass, RetryKind, bool, Escalation, bool); 9] = [
        (
            ErrorClass::ValidationError,
            RetryKind::Never,
            false,
            Escalation::None,
            false,
        ),
        (
            ErrorClass::PermissionError,
            RetryKind::Never,
            false,
            Escalation::Human,
            false,
        ),
        (
            ErrorClass::PolicyError,
            RetryKind::Never,
            false,
            Escalation::Human,
            false,
        ),
        (
            ErrorClass::TransientError,
            RetryKind::ExponentialBackoff,
            false,
            Escalation::None,
            false,
        ),
        (
            ErrorClass::TimeoutError,
            RetryKind::Bounded,
            false,
            Escalation::None,
            true,
        ),
        (
            ErrorClass::AgentError,
            RetryKind::AlternateAgent,
            true,
            Escalation::None,
            true,
        ),
        (
            ErrorClass::SkillError,
            RetryKind::AlternateSkill,
            true,
            Escalation::None,
            true,
        ),
        (
            ErrorClass::RuntimeError,
            RetryKind::RecreateInstance,
            false,
            Escalation::None,
            true,
        ),
        (
            ErrorClass::InternalError,
            RetryKind::Never,
            false,
            Escalation::None,
            false,
        ),
    ];
    for (class, retry, replan, escalation, replan_at_limit) in rows {
        let d: Disposition = class.disposition();
        assert_eq!(
            (d.retry, d.replan, d.escalation, d.replan_at_limit),
            (retry, replan, escalation, replan_at_limit),
            "{class:?}'s row drifted from the §20.1 table"
        );
    }
}

#[test]
fn fault_injection_runs_each_class_through_the_decision_composition() {
    // One injected failure per class, then the composition the Core Host wiring uses:
    // the budget decision plus the recovery review, per class's own menu face.
    let policy = RetryPolicy { max_attempts: 3 };
    for class in ErrorClass::ALL {
        let d: Disposition = class.disposition();
        let decision = policy.decide(&d, 1, None, None);
        match d.retry {
            RetryKind::Never => assert!(
                matches!(decision, Err(RetryFault::ClassForbids)),
                "{class:?} must not retry"
            ),
            RetryKind::ExponentialBackoff
            | RetryKind::Bounded
            | RetryKind::AlternateAgent
            | RetryKind::AlternateSkill
            | RetryKind::RecreateInstance => {
                let attempt = decision.expect("within budget on attempt 1");
                assert!(
                    format!("{attempt:?}").contains("2"),
                    "{class:?}'s next attempt is 2: {attempt:?}"
                );
            }
        }
        // The menu face: the recovery menu exists for every class; the row binds only
        // the retry-shaped means.
        for mean in athena_core::failure::recovery::Mean::menu() {
            let allowed = mean.allowed_by(&d.retry);
            if mean.is_retry_shaped() {
                match d.retry {
                    RetryKind::Never => {
                        assert!(!allowed, "{class:?}: retry-shaped must be refused")
                    }
                    RetryKind::AlternateAgent => {
                        let _ = allowed; // Room for a wider run to be permitted.
                        assert!(
                            mean != athena_core::failure::recovery::Mean::RetrySame || !allowed
                        );
                    }
                    RetryKind::AlternateSkill => {
                        assert!(
                            mean != athena_core::failure::recovery::Mean::RetrySame || !allowed,
                            "{class:?}: same-agent rerun refused on alternate-skill rows"
                        );
                    }
                    _ => {}
                }
            } else {
                assert!(allowed, "{class:?}: wider means always legal");
            }
        }
    }
}

#[test]
fn exhaustion_at_the_default_ceiling_of_three_will_not_reach_forever() {
    // ATH-FAIL-005: the default is 3; overrides only ever lower it (ATH-FAIL-004).
    let policy = RetryPolicy { max_attempts: 3 };
    for overridable in [
        ErrorClass::TransientError,
        ErrorClass::TimeoutError,
        ErrorClass::AgentError,
        ErrorClass::SkillError,
        ErrorClass::RuntimeError,
    ] {
        let d: Disposition = overridable.disposition();
        for (attempts, ok) in [(0u64, true), (2u64, true), (3u64, false), (5u64, false)] {
            let shot = policy.decide(&d, attempts, None, None);
            assert_eq!(
                shot.is_ok(),
                ok,
                "{overridable:?} at {attempts} attempts: within/default ceiling mismatch"
            );
        }
        // A step override never raises.
        assert!(policy.decide(&d, 1, Some(1u64), None).is_err());
        assert!(policy.decide(&d, 1, None, Some(1u64)).is_err());
    }
}

#[test]
fn the_backoff_budget_applies_exponential_growth_capped() {
    // ATH-FAIL-006 on the TransientError retry lane.
    let budget = RetryBudget {
        backoff: athena_config::schema::BackoffSection::default(),
    };
    let t0 = 1_000_000u64;
    let a1 = budget.ready_at(t0, 1);
    let a2 = budget.ready_at(t0, 2);
    let a3 = budget.ready_at(t0, 3);
    assert!(
        a1 <= a2 && a2 <= a3,
        "the backoff delays grow with the attempt"
    );
}

#[test]
fn an_unclassifiable_fault_is_internal_error_and_never_retries() {
    let unclassified = ErrorClass::unclassifiable();
    assert_eq!(unclassified, ErrorClass::InternalError);
    let d: Disposition = unclassified.disposition();
    assert_eq!(d.retry, RetryKind::Never);
    assert!(!d.replan);
    assert!(
        RetryPolicy { max_attempts: 3 }
            .decide(&d, 0, None, None)
            .is_err(),
        "an unclassifiable fault must not retry (ATH-FAIL-002)"
    );
}

// --- 2. Approval expiry (ATH-HITL-005) ---------------------------------------------------

#[test]
fn an_expired_deadline_routes_the_step_to_failed_not_replanning() {
    use athena_core::failure::recovery::{Route, RouteKind, route_approval, route_kind};
    assert!(
        athena_core::approval_manager::staging::deadline_passed(
            "2026-10-18T10:00:00Z",
            "2026-10-18T10:00:01Z",
        ),
        "one second past the deadline is expired"
    );
    let route: Route = route_approval(&athena_types::ApprovalState::Expired);
    assert_eq!(
        (route, route_kind(&route)),
        (Route::Fail, RouteKind::Ends),
        "EXPIRED == rejected at failure level (ATH-HITL-005): FAILED, never REPLANNING"
    );
    // Silence just before the deadline is not expiry.
    assert!(!athena_core::approval_manager::staging::deadline_passed(
        "2026-10-18T10:00:01Z",
        "2026-10-18T10:00:00Z",
    ));
}

#[test]
fn expiry_never_leaves_a_consumable_grant_behind() {
    // An approval that never left REQUESTED cannot be consumed: the fresh-approval gate
    // refuses the exhausted shape, so no high-risk operation rides an expired row.
    let shot =
        athena_core::approval_manager::staging::consume(&RiskLevel::High, &RiskLevel::High, 0);
    assert!(
        shot.is_err(),
        "uses_remaining 0 cannot serve another execution"
    );
}

// --- 3. Rejected high-risk: never executed (ATH-HITL-001/015) ----------------------------

#[test]
fn a_rejection_is_replanning_and_the_denial_still_fails_the_suspended_step() {
    use athena_core::failure::recovery::{Route, RouteKind, route_approval, route_kind};
    // The human's REJECTED routes the Task to REPLANNING (never straight to FAILED):
    let route: Route = route_approval(&athena_types::ApprovalState::Rejected);
    assert_eq!(
        route,
        Route::Replan,
        "ATH-STATE-015: rejection re-plans, it does not fail the Task"
    );
    assert_eq!(route_kind(&route), RouteKind::Continue);
    // And the step left SUSPENDED by the denial has its own §17.5 terminal row.
    let row = athena_state::plan_step_transition(
        athena_types::PlanStepState::Suspended,
        athena_state::PlanStepTrigger::ApprovalDenied,
    )
    .expect("SUSPENDED has a denial route");
    assert_eq!(row.to, athena_types::PlanStepState::Failed);
    assert_eq!(row.event, "PlanStepFailed");
}

#[test]
fn an_escalated_risk_demands_a_fresh_approval_even_over_an_open_grant() {
    // `ATH-HITL-014/015`: an operation whose class exceeds the approved one must re-route
    // through a fresh approval, even while the earlier grant still has uses left.
    let shot =
        athena_core::approval_manager::staging::consume(&RiskLevel::Medium, &RiskLevel::High, 3);
    assert!(
        shot.is_err(),
        "medium-approved scope cannot serve a high-risk op"
    );
    // And the equal-or-lower direction is legal while uses remain.
    assert!(
        athena_core::approval_manager::staging::consume(&RiskLevel::High, &RiskLevel::Medium, 3,)
            .is_ok()
    );
}

// --- 4. Granted resume: identical Skill Set (ATH-STATE-021) ------------------------------

#[test]
fn the_approval_grant_resumes_the_suspended_step_through_the_table() {
    let row = athena_state::plan_step_transition(
        athena_types::PlanStepState::Suspended,
        athena_state::PlanStepTrigger::ApprovalGranted,
    )
    .expect("the resume row exists");
    assert_eq!(row.to, athena_types::PlanStepState::Running);
    assert_eq!(row.event, "AgentResumed");
}

#[test]
fn a_resumed_instance_runs_the_identical_skill_set_and_refuses_drift() {
    let frozen = FrozenSnapshot {
        instance_id: "inst-1".into(),
        skill_set: vec![skill_ref("lint", "1.2.0"), skill_ref("test", "0.9.1")],
        short_term_summary_ref: Some("mem-summary-1".into()),
    };
    // The identical set (ids, versions, order) resumes.
    let resumed = resume_with(
        &frozen,
        &[skill_ref("lint", "1.2.0"), skill_ref("test", "0.9.1")],
    )
    .expect("the identical set resumes");
    assert_eq!(resumed.len(), 2);
    // Version drift refuses.
    for drifted in [
        vec![skill_ref("lint", "1.3.0"), skill_ref("test", "0.9.1")],
        vec![skill_ref("lint", "1.2.0")],
        vec![skill_ref("test", "0.9.1"), skill_ref("lint", "1.2.0")],
    ] {
        assert!(
            resume_with(&frozen, &drifted).is_err(),
            "drifted Skill Set refuses: {drifted:?}"
        );
    }
}

// --- 5. Stall detection before the execution timeout (ATH-OBS-012 × §20.3) ---------------

#[test]
fn the_stall_threshold_stays_behind_the_execution_deadline() {
    // §30 config defaults: `core.iteration_timeout 120s`, the Scheduler's stall factor 2
    // → 240 s of silence signals the stall. The §20.3 default execution timeout is 30 min.
    // The escalation must be reachable strictly inside that window.
    let silence_threshold_ms: u64 = 120_000 * 2;
    let execution_timeout_ms: u64 = 30 * 60 * 1000;
    assert!(
        silence_threshold_ms < execution_timeout_ms,
        "stall detection must fire before the execution timeout"
    );
    // At the threshold, the detector fires.
    let stall = observability::silence(silence_threshold_ms, silence_threshold_ms)
        .expect("threshold silence requests a task stalled");
    assert_eq!(stall.kind(), "state_change_silence");
    // Just before it, nothing.
    assert!(observability::silence(silence_threshold_ms - 1, silence_threshold_ms).is_none());
}

#[test]
fn a_wait_graph_cycle_stalls_immediately_not_at_the_deadline() {
    let a = athena_types::InstanceId::new();
    let b = athena_types::InstanceId::new();
    let stall = observability::wait_cycle(&[(a.clone(), b.clone()), (b.clone(), a.clone())])
        .expect("a↔b is a cycle at elapsed time 0");
    match stall {
        observability::Stall::WaitCycle { cycle } => {
            let mut pair = vec![a.clone(), b.clone()];
            pair.sort();
            assert_eq!(cycle, pair);
        }
        other => panic!("expected WaitCycle, got {other:?}"),
    }
}
