//! Tests for the `ORPHANED` re-run gate and the one-instance-per-attempt rule (P4-25).

use super::*;

fn verified() -> SideEffectVerification {
    SideEffectVerification {
        evaluator: athena_types::Evaluator::Tool,
        method: "test".into(),
        verdict: Verdict::Accept,
        rationale: "test suite re-run against the workspace establishes no writes were committed"
            .into(),
    }
}

#[test]
fn a_verified_orphan_supported_row_steps_ready() {
    for state in [
        PlanStepState::Pending,
        PlanStepState::Ready,
        PlanStepState::Running,
        PlanStepState::Suspended,
        PlanStepState::Success,
        PlanStepState::Failed,
        PlanStepState::Skipped,
    ] {
        let route = authorize_orphan_retry(state, Some(&verified()));
        assert_eq!(
            route,
            OrphanRoute::Refused(OrphanFault::NotOrphaned),
            "the gate claims {state:?}"
        );
    }
    assert!(matches!(
        authorize_orphan_retry(PlanStepState::Orphaned, Some(&verified())),
        OrphanRoute::Rearmed(_)
    ));
}

#[test]
fn the_rearm_row_is_the_transition_tables_row() {
    // The route the wiring must fire is the same §17.5 row Core Host owns:
    // ORPHANED -> READY via `VerifiedRetryAllowed`, `PlanStepReady`, Core Host.
    let row = athena_state::plan_step_transition(
        PlanStepState::Orphaned,
        athena_state::PlanStepTrigger::VerifiedRetryAllowed,
    )
    .expect("the ORPHANED->READY row exists");
    assert_eq!(row.to, PlanStepState::Ready);
    assert_eq!(row.event, "PlanStepReady");
}

#[test]
fn no_verification_record_blocks_the_rerun() {
    assert_eq!(
        authorize_orphan_retry(PlanStepState::Orphaned, None),
        OrphanRoute::Refused(OrphanFault::Unverified)
    );
}

#[test]
fn a_self_attestation_is_never_an_objective_record() {
    let blank = SideEffectVerification {
        evaluator: athena_types::Evaluator::Agent {
            agent_id: athena_types::AgentId::new("worker-1"),
        },
        method: "   ".into(),
        verdict: Verdict::Accept,
        rationale: "trust me".into(),
    };
    assert_eq!(
        authorize_orphan_retry(PlanStepState::Orphaned, Some(&blank)),
        OrphanRoute::Refused(OrphanFault::SelfAttestation)
    );
    // `ATH-EVAL-005`: a self-declared success never stands for the objective record.
    assert_eq!(
        authorize_orphan_retry(
            PlanStepState::Orphaned,
            Some(&SideEffectVerification {
                evaluator: athena_types::Evaluator::Agent {
                    agent_id: athena_types::AgentId::new("worker-1"),
                },
                method: "review".into(),
                verdict: Verdict::Accept,
                rationale: "   ".into(),
            })
        ),
        OrphanRoute::Refused(OrphanFault::SelfAttestation)
    );
}

#[test]
fn a_non_accept_verdict_is_a_fact_not_a_substitution() {
    for verdict in [
        Verdict::Retry,
        Verdict::Replan,
        Verdict::Fail,
        Verdict::Escalate,
    ] {
        let record = SideEffectVerification {
            evaluator: athena_types::Evaluator::Tool,
            method: "build".into(),
            verdict,
            rationale: "the workspace state contradicts the reported completion".into(),
        };
        match authorize_orphan_retry(PlanStepState::Orphaned, Some(&record)) {
            OrphanRoute::Refused(OrphanFault::NotAccepted(v)) => assert_eq!(v, verdict),
            other => panic!("expected the NotAccepted refusal, got {other:?}"),
        }
    }
}

#[test]
fn failing_the_step_requires_the_orphaned_state_and_coerces_the_row() {
    assert!(
        fail_orphaned_step(PlanStepState::Orphaned).is_ok(),
        "the ORPHANED->FAILED row exists and names the Core Agent"
    );
    for state in [
        PlanStepState::Pending,
        PlanStepState::Running,
        PlanStepState::Success,
        PlanStepState::Failed,
    ] {
        assert!(fail_orphaned_step(state).is_err());
    }
    // The row the call rides: FAILED through `RetryImpossible`, `PlanStepFailed`, Core Agent.
    let row = athena_state::plan_step_transition(
        PlanStepState::Orphaned,
        athena_state::PlanStepTrigger::RetryImpossible,
    )
    .expect("row exists");
    assert_eq!(row.to, PlanStepState::Failed);
    assert_eq!(row.event, "PlanStepFailed");
}

fn instance(step: athena_types::PlanStepId, attempt: u64) -> athena_types::AgentInstance {
    athena_types::AgentInstance {
        id: athena_types::InstanceId::new(),
        agent_id: athena_types::AgentId::new("worker-1"),
        task_id: Some(athena_types::TaskId::new()),
        plan_step_id: Some(step),
        superior_instance_id: None,
        delegation_depth: 2,
        state: athena_types::AgentInstanceState::Failed,
        runtime_instance_id: None,
        workspace: "/workspace".into(),
        skill_set: vec![],
        aiprj_log_ref: None,
        attempt,
        started_at: None,
        finished_at: None,
    }
}

#[test]
fn every_attempt_grows_the_instance_list_strictly() {
    let step = athena_types::PlanStepId::new();
    fresh_instance_for_attempt(&step, 1, &[]).expect("the first attempt is free");
    let history = [instance(step.clone(), 1)];
    fresh_instance_for_attempt(&step, 2, &history).expect("the next attempt is a fresh instance");
    // Same attempt again: a second instance for one attempt is refused (`ATH-FAIL-007`).
    assert_eq!(
        fresh_instance_for_attempt(&step, 1, &history),
        Err(FreshInstanceFault::AttemptAlreadyBound {
            step: step.clone(),
            attempt: 1,
            instance: history[0].id.clone(),
        })
    );
    // An out-of-order number: the history runs at or beyond it.
    let ahead = [instance(step.clone(), 2)];
    assert!(matches!(
        fresh_instance_for_attempt(&step, 1, &ahead),
        Err(FreshInstanceFault::AttemptNotNew {
            recorded: 2,
            requested: 1,
            ..
        })
    ));
    assert!(matches!(
        fresh_instance_for_attempt(&step, 0, &[]),
        Err(FreshInstanceFault::AttemptNumber(0))
    ));
}

#[test]
fn instances_of_other_steps_do_not_bind_this_attempts_number() {
    let step = athena_types::PlanStepId::new();
    let other = [instance(athena_types::PlanStepId::new(), 5)];
    fresh_instance_for_attempt(&step, 1, &other)
        .expect("another step's attempt history is irrelevant here");
}
