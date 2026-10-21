//! Inspection-path behaviour (ATH-CORE-026/034/035/037/038, R-CORE-25).

use crate::inspection::{Inspection, InspectionFacts, Inspector, Reason, deny_envelope};
use crate::protocol::{CoreRequest, ErrorClass, WritableMemoryTier};
use athena_config::policy::{CompiledPolicy, Operation};
use athena_persistence::AthenaStore;
use athena_types::{PlanStepId, TaskId, ToolName};

/// Empty rules: everything classifies as the default deny (`high`, approval required).
fn empty_policy() -> CompiledPolicy {
    let section = serde_yaml::from_str::<athena_config::schema::PolicySection>(
        "rules: []\napproval_scopes: []\nmax_approval_ttl: 24h",
    )
    .expect("a bare policy section parses");
    CompiledPolicy::compile(&section).expect("an empty rule set compiles")
}

fn runtime() -> AthenaStore {
    AthenaStore::in_memory().expect("in-memory claim store")
}

fn facts(tools: &[ToolName]) -> InspectionFacts {
    InspectionFacts {
        allowed_tools: tools.to_vec(),
    }
}

/// The core agent's minimal grant, per ATH-SEC-025.
fn core_agent_facts() -> InspectionFacts {
    facts(&[ToolName::FsRead, ToolName::FsWrite, ToolName::SendTo])
}

#[test]
fn every_kind_passes_inspection_without_side_effects() {
    // ATH-CORE-026: inspection covers *every* kind; kinds without grant / key material
    // proceed on their own merits.
    let policy = empty_policy();
    let inspector = Inspector::new(&policy);
    let mut claims = runtime();
    let facts = core_agent_facts();

    let requests: Vec<CoreRequest> = {
        let task = TaskId::new();
        vec![
            CoreRequest::SubmitUnderstanding {
                task_id: task.clone(),
                understanding: athena_types::Understanding {
                    goal: "goal".into(),
                    constraints: vec![],
                    completion_criteria: vec![],
                    unknowns: vec![],
                    risk_level: athena_types::RiskLevel::Medium,
                },
            },
            CoreRequest::SubmitPlan {
                task_id: task.clone(),
                base_revision: 0,
                steps: vec![],
            },
            CoreRequest::ComposeHierarchy {
                task_id: task.clone(),
                nodes: vec![],
            },
            CoreRequest::RequestApproval {
                task_id: task.clone(),
                operation_ref: "fs_write:/project/src/lib.rs".into(),
                reason: "needed".into(),
                impact: "rewrites a file".into(),
            },
            CoreRequest::AskHuman {
                task_id: task.clone(),
                question: "which layout?".into(),
                options: None,
            },
            CoreRequest::SubmitEvaluation {
                task_id: task.clone(),
                result_id: athena_types::ResultId::new(),
                verdict: athena_types::Verdict::Accept,
                rationale: "criteria met".into(),
            },
            CoreRequest::TransitionTask {
                task_id: task.clone(),
                to: athena_types::TaskState::Running,
                expected_state: athena_types::TaskState::Queued,
            },
            CoreRequest::WriteMemory {
                tier: WritableMemoryTier::Working,
                content: "context note".into(),
            },
            CoreRequest::CompleteTask {
                task_id: task.clone(),
                summary: "done".into(),
                result_ids: vec![],
            },
        ]
    };
    for request in &requests {
        let verdict = inspector.inspect(request, &facts, &mut claims);
        assert!(
            matches!(
                verdict,
                Inspection::Proceed {
                    classification: None,
                    replay: false
                },
            ),
            "{verdict:?} for {:?}",
            request.kind_tag()
        );
    }
}

#[test]
fn an_in_grant_tool_classifies_from_the_request_facts() {
    // ATH-CFG-009: first match, or default deny (`high`). `fs_write` is in the Core
    // Agent's grant here, but the *rule set* says medium / approval-required.
    let section = serde_yaml::from_str::<athena_config::schema::PolicySection>(
        r#"
        rules:
          - match:
              tool: fs_write
            risk: medium
            requires_approval: true
        approval_scopes: []
        max_approval_ttl: 24h
        "#,
    )
    .expect("the rule section parses");
    let policy = CompiledPolicy::compile(&section).expect("compiles");
    let inspector = Inspector::new(&policy);
    let mut claims = runtime();

    let request = CoreRequest::ExecuteTool {
        task_id: TaskId::new(),
        tool: ToolName::FsWrite,
        args: serde_json::json!({ "path": "/project/src/lib.rs" }),
        idempotency_key: crate::protocol::idempotency_key(&PlanStepId::new(), 0),
    };
    let verdict = inspector.inspect(&request, &core_agent_facts(), &mut claims);
    let (classification, replay) = match verdict {
        Inspection::Proceed {
            classification,
            replay,
        } => (classification.expect("classified"), replay),
        other => panic!("{other:?}"),
    };
    assert_eq!(classification.risk, athena_types::RiskLevel::Medium);
    assert!(classification.requires_approval);
    assert_eq!(classification.matched_rule, Some(0));
    assert!(!replay);
}

#[test]
fn an_unmatched_tool_falls_back_to_default_deny() {
    // ATH-CFG-009: no rule ⇒ `high` + approval, with `matched_rule: None`.
    let policy = empty_policy();
    let inspector = Inspector::new(&policy);
    let mut claims = runtime();
    let request = CoreRequest::ExecuteTool {
        task_id: TaskId::new(),
        tool: ToolName::FsRead,
        args: serde_json::json!({}),
        idempotency_key: crate::protocol::idempotency_key(&PlanStepId::new(), 0),
    };
    let verdict = inspector.inspect(&request, &core_agent_facts(), &mut claims);
    let classification = match verdict {
        Inspection::Proceed {
            classification,
            replay,
        } => {
            assert!(!replay);
            classification.expect("classified")
        }
        other => panic!("{other:?}"),
    };
    assert_eq!(
        classification,
        athena_config::policy::Classification::default_deny()
    );
}

#[test]
fn an_out_of_grant_tool_is_refused_as_permission_error() {
    // R-TOOL-02 / ATH-SEC-025: `shell` is not in the Core Agent's default grant; granting
    // it to itself is impossible (R-SEC-09).
    let policy = empty_policy();
    let inspector = Inspector::new(&policy);
    let mut claims = runtime();
    let request = CoreRequest::ExecuteTool {
        task_id: TaskId::new(),
        tool: ToolName::Shell,
        args: serde_json::json!({ "cmd": "rm -rf /" }),
        idempotency_key: crate::protocol::idempotency_key(&PlanStepId::new(), 0),
    };
    let verdict = inspector.inspect(&request, &core_agent_facts(), &mut claims);
    let reason = match verdict {
        Inspection::Rejected(reason) => reason,
        other => panic!("{other:?}"),
    };
    assert!(
        matches!(
            reason,
            Reason::OutOfGrantTool {
                tool: ToolName::Shell
            }
        ),
        "{reason}"
    );
    assert_eq!(reason.class(), ErrorClass::PermissionError);
    assert_eq!(reason.code(), "GRANT_TOOL_NOT_ALLOWED");
    assert_eq!(reason.policy_id(), "agent.grant");

    // The refusal materializes the §22.2 envelope AND the §23.2 denial Event payload.
    let envelope = deny_envelope(&reason, "cor-1");
    assert_eq!(envelope.class, ErrorClass::PermissionError);
    assert_eq!(envelope.details["policy_id"], "agent.grant");
    let event = reason.denial_event("execute_tool");
    assert_eq!(event.request_kind, "execute_tool");
    assert_eq!(event.error_class, "permission_error");
    assert_eq!(event.policy_id, "agent.grant");
}

#[test]
fn the_same_idempotency_key_cannot_be_claimed_twice() {
    // ATH-CORE-035: the double-delegation guard behind a restart replay.
    let policy = empty_policy();
    let inspector = Inspector::new(&policy);
    let mut claims = runtime();
    let step = PlanStepId::new();
    let key = crate::protocol::idempotency_key(&step, 0);
    let request = CoreRequest::ExecuteTool {
        task_id: TaskId::new(),
        tool: ToolName::FsRead,
        args: serde_json::json!({}),
        idempotency_key: key,
    };
    let first = inspector.inspect(&request, &core_agent_facts(), &mut claims);
    assert!(
        matches!(first, Inspection::Proceed { replay: false, .. }),
        "{first:?}"
    );
    let second = inspector.inspect(&request, &core_agent_facts(), &mut claims);
    match second {
        Inspection::Proceed { replay: true, .. } => {}
        other => panic!("replay must not be refused nor re-run: {other:?}"),
    }
}

#[test]
fn malformed_or_foreign_keys_are_refused_before_the_claim() {
    // ATH-CORE-035: the key IS `plan_step_id + ":" + attempt`. A malformed key cannot
    // claim anything.
    let policy = empty_policy();
    let inspector = Inspector::new(&policy);
    let mut claims = runtime();
    let step = PlanStepId::new();
    let facts = core_agent_facts();

    for key in [
        "not-a-key",
        "task-abc:0",
        "step-abc:zero",
        "step-abc:",
        ":0",
        "step-abc",
    ] {
        let request = CoreRequest::ExecuteTool {
            task_id: TaskId::new(),
            tool: ToolName::FsRead,
            args: serde_json::json!({}),
            idempotency_key: key.to_string(),
        };
        let verdict = inspector.inspect(&request, &facts, &mut claims);
        let reason = match verdict {
            Inspection::Rejected(reason) => reason,
            other => panic!("key {key:?} must be refused, got {other:?}"),
        };
        assert!(
            matches!(reason, Reason::MalformedIdempotencyKey(_)),
            "{reason}"
        );
        assert_eq!(reason.class(), ErrorClass::ValidationError);
    }

    // The key must belong to the PlanStep that carries it.
    let foreign = CoreRequest::Delegate {
        task_id: TaskId::new(),
        plan_step_id: step.clone(),
        agent_id: athena_types::AgentId::new("rtl-worker"),
        skill_set: vec![],
        instruction: "run it".into(),
        idempotency_key: crate::protocol::idempotency_key(&PlanStepId::new(), 0),
    };
    let verdict = inspector.inspect(&foreign, &facts, &mut claims);
    assert!(
        matches!(
            verdict,
            Inspection::Rejected(Reason::ForeignIdempotencyKey(_))
        ),
        "{verdict:?}"
    );

    // A well-formed key of the owning step claims and does not replay.
    let own = CoreRequest::Delegate {
        task_id: TaskId::new(),
        plan_step_id: step.clone(),
        agent_id: athena_types::AgentId::new("rtl-worker"),
        skill_set: vec![],
        instruction: "run it".into(),
        idempotency_key: crate::protocol::idempotency_key(&step, 0),
    };
    let verdict = inspector.inspect(&own, &facts, &mut claims);
    assert!(
        matches!(verdict, Inspection::Proceed { replay: false, .. }),
        "{verdict:?}"
    );
}

#[test]
fn an_empty_delegation_instruction_is_refused() {
    let policy = empty_policy();
    let inspector = Inspector::new(&policy);
    let mut claims = runtime();
    let step = PlanStepId::new();
    let request = CoreRequest::Delegate {
        task_id: TaskId::new(),
        plan_step_id: step.clone(),
        agent_id: athena_types::AgentId::new("rtl-worker"),
        skill_set: vec![],
        instruction: "   ".into(),
        idempotency_key: crate::protocol::idempotency_key(&step, 0),
    };
    let verdict = inspector.inspect(&request, &core_agent_facts(), &mut claims);
    let reason = match verdict {
        Inspection::Rejected(reason) => reason,
        other => panic!("{other:?}"),
    };
    assert!(matches!(reason, Reason::EmptyInstruction), "{reason}");
    assert_eq!(reason.class(), ErrorClass::ValidationError);
    assert_eq!(reason.code(), "INSPECTION_EMPTY_INSTRUCTION");
}

#[test]
fn the_agent_prose_never_enters_classification() {
    // ATH-CORE-038 / R-CORE-25: the [`Operation`] the classifier sees has no prose fields,
    // so prose cannot influence any verdict. Demonstrate at the projection seam: the same
    // requested tool and args always classify identically, and `reason` / `impact` are
    // nowhere in the classifier's input type.
    let section = serde_yaml::from_str::<athena_config::schema::PolicySection>(
        r#"
        rules:
          - match:
              tool: fs_read
            risk: low
            requires_approval: false
        approval_scopes: []
        max_approval_ttl: 24h
        "#,
    )
    .expect("the rule section parses");
    let policy = CompiledPolicy::compile(&section).expect("compiles");
    let first = policy.classify(&Operation::new(ToolName::FsRead, "/project/a"));
    let second = policy.classify(&Operation::new(ToolName::FsRead, "/project/a"));
    assert_eq!(
        first, second,
        "classification is a pure function of the operation"
    );
    // The reason the agent wrote is invisible to it by construction; the request kind that
    // carries prose (`request_approval`) produces no classification at all in the engine.
    let inspector = Inspector::new(&policy);
    let mut claims = runtime();
    let prose_request = CoreRequest::RequestApproval {
        task_id: TaskId::new(),
        operation_ref: "fs_write:/victim".into(),
        reason: "the most exculpatory paragraph ever written".into(),
        impact: "insignificant".into(),
    };
    let verdict = inspector.inspect(&prose_request, &core_agent_facts(), &mut claims);
    assert!(
        matches!(
            verdict,
            Inspection::Proceed {
                classification: None,
                ..
            }
        ),
        "{verdict:?}"
    );
}
