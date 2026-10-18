//! Protocol and skeleton contract tests for the Core Host.

use crate::ownership::{Entity, ModuleRole, OWNERSHIP};
use crate::protocol::{CoreRequest, CoreResponse, ErrorClass, ErrorEnvelope, WritableMemoryTier};
use crate::{
    agent_manager, approval_manager, broker, hierarchy_manager, memory_manager, result_manager,
    runtime_manager, state_manager, tool_manager,
};
use athena_types::{
    AgentId, HierarchyNodeDraft, PlanStepDraft, PlanStepId, ResultId, SkillRef, TaskId, TaskState,
    ToolName, Understanding, Verdict,
};

// --- P1-20: the skeleton owns exactly what §26.2 grants it -------------------------------

/// Contract: each manager's `OWNED` slice is exactly the table's rows for its role.
#[test]
fn each_manager_owns_exactly_its_table_slice() {
    fn rows(role: ModuleRole) -> Vec<Entity> {
        let mut owned: Vec<Entity> = OWNERSHIP
            .iter()
            .filter(|(_, owner)| *owner == role)
            .map(|(entity, _)| *entity)
            .collect();
        owned.sort();
        owned
    }

    assert_eq!(
        manager_of(state_manager::OWNED),
        rows(ModuleRole::StateManager)
    );
    assert_eq!(
        manager_of(agent_manager::OWNED),
        rows(ModuleRole::AgentManager)
    );
    assert_eq!(
        manager_of(hierarchy_manager::OWNED),
        rows(ModuleRole::HierarchyManager)
    );
    assert_eq!(manager_of(broker::OWNED), rows(ModuleRole::Broker));
    assert_eq!(
        manager_of(runtime_manager::OWNED),
        rows(ModuleRole::RuntimeManager)
    );
    assert_eq!(
        manager_of(tool_manager::OWNED),
        rows(ModuleRole::ToolManager)
    );
    assert_eq!(
        manager_of(memory_manager::OWNED),
        rows(ModuleRole::MemoryManager)
    );
    assert_eq!(
        manager_of(approval_manager::OWNED),
        rows(ModuleRole::ApprovalManager)
    );
    assert_eq!(
        manager_of(result_manager::OWNED),
        rows(ModuleRole::ResultManager)
    );

    fn manager_of(slice: &[(Entity, ModuleRole)]) -> Vec<Entity> {
        let mut owned: Vec<Entity> = slice.iter().map(|(e, _)| *e).collect();
        owned.sort();
        owned
    }
}

// --- P1-21: the protocol ----------------------------------------------------------------

fn sample_understanding() -> Understanding {
    Understanding {
        goal: "produce the report".into(),
        constraints: vec!["no externals".into()],
        completion_criteria: vec!["report exists".into()],
        unknowns: vec![],
        risk_level: athena_types::RiskLevel::High,
    }
}

/// One sample request per catalog kind, for tests across this crate.
pub(crate) fn wire_samples() -> Vec<(CoreRequest, &'static str)> {
    let task = TaskId::new();
    let step = PlanStepId::new();
    let wire: Vec<(CoreRequest, &'static str)> = vec![
        (
            CoreRequest::SubmitUnderstanding {
                task_id: task.clone(),
                understanding: sample_understanding(),
            },
            "submit_understanding",
        ),
        (
            CoreRequest::SubmitPlan {
                task_id: task.clone(),
                base_revision: 1,
                steps: vec![PlanStepDraft {
                    order: 0,
                    intent: "write the report".into(),
                    depends_on_orders: vec![],
                    assigned_agent: Some(AgentId::new("rtl-worker")),
                    skill_set: vec![],
                }],
            },
            "submit_plan",
        ),
        (
            CoreRequest::ComposeHierarchy {
                task_id: task.clone(),
                nodes: vec![HierarchyNodeDraft {
                    agent_id: AgentId::new("rtl-worker"),
                    level: athena_types::AgentLevel::Worker,
                    superior_agent_id: None,
                    scope_ref: None,
                    sibling_acl: vec![],
                }],
            },
            "compose_hierarchy",
        ),
        (
            CoreRequest::Delegate {
                task_id: task.clone(),
                plan_step_id: step.clone(),
                agent_id: AgentId::new("rtl-worker"),
                skill_set: vec![],
                instruction: "run the step".into(),
                idempotency_key: crate::protocol::idempotency_key(&step, 0),
            },
            "delegate",
        ),
        (
            CoreRequest::ExecuteTool {
                task_id: task.clone(),
                tool: ToolName::FsRead,
                args: serde_json::json!({ "path": "/project/README.md" }),
                idempotency_key: crate::protocol::idempotency_key(&step, 1),
            },
            "execute_tool",
        ),
        (
            CoreRequest::RequestApproval {
                task_id: task.clone(),
                operation_ref: "fs_write:/project/src/lib.rs".into(),
                reason: "needed for the change".into(),
                impact: "file rewritten".into(),
            },
            "request_approval",
        ),
        (
            CoreRequest::ConsumeApproval {
                approval_id: athena_types::ApprovalId::new(),
                operation_ref: "fs_write:/project/src/lib.rs".into(),
            },
            "consume_approval",
        ),
        (
            CoreRequest::AskHuman {
                task_id: task.clone(),
                question: "Which of these two layouts?".into(),
                options: Some(vec!["A".into(), "B".into()]),
            },
            "ask_human",
        ),
        (
            CoreRequest::SubmitEvaluation {
                task_id: task.clone(),
                result_id: ResultId::new(),
                verdict: Verdict::Accept,
                rationale: "criteria met".into(),
            },
            "submit_evaluation",
        ),
        (
            CoreRequest::TransitionTask {
                task_id: task.clone(),
                to: TaskState::Running,
                expected_state: TaskState::Queued,
            },
            "transition_task",
        ),
        (
            CoreRequest::WriteMemory {
                tier: WritableMemoryTier::Working,
                content: "learned: build takes 90 s".into(),
            },
            "write_memory",
        ),
        (
            CoreRequest::CompleteTask {
                task_id: task.clone(),
                summary: "done".into(),
                result_ids: vec![ResultId::new(), ResultId::new()],
            },
            "complete_task",
        ),
    ];
    wire
}

#[test]
fn the_twelve_request_kinds_round_trip_on_the_wire() {
    let wire = wire_samples();
    let mut seen_kinds = std::collections::HashSet::new();
    for (request, kind) in &wire {
        let json = serde_json::to_value(request).expect("serializes");
        assert_eq!(json["kind"], *kind, "wire tag for {kind}");
        let back: CoreRequest = serde_json::from_value(json.clone()).expect("deserializes");
        assert_eq!(&back, request, "round trip for {kind}");
        seen_kinds.insert((*kind).to_string());
    }
    // Exactly the twelve named kinds, no drift (§7.5.5).
    let catalog: std::collections::HashSet<String> = crate::protocol::REQUEST_KINDS
        .iter()
        .map(|k| (*k).to_string())
        .collect();
    assert_eq!(seen_kinds, catalog);
}

/// The same samples as payload JSON, in the same order.
pub(crate) fn wire_sample_payloads() -> Vec<(String, &'static str)> {
    wire_samples()
        .into_iter()
        .map(|(request, kind)| {
            (
                serde_json::to_value(request)
                    .expect("serializes")
                    .to_string(),
                kind,
            )
        })
        .collect()
}

#[test]
fn an_unknown_request_kind_is_refused_on_the_wire() {
    let json = serde_json::json!({ "kind": "start_agent_directly", "agent_id": "x" });
    let err = serde_json::from_value::<CoreRequest>(json).expect_err("out of protocol");
    let text = err.to_string();
    assert!(
        text.contains("start_agent_directly") || text.contains("unknown variant"),
        "{text}"
    );
}

#[test]
fn a_submitplan_cannot_carry_agent_minted_identifiers() {
    // Drafts are structurally id-free: an `id` the agent injects is not a field, so the
    // wire carries only the drafted content (ATH-CORE-041, D-5).
    let draft = PlanStepDraft {
        order: 3,
        intent: "step".into(),
        depends_on_orders: vec![1],
        assigned_agent: None,
        skill_set: vec![SkillRef {
            id: athena_types::SkillId::new("s"),
            version: "1.0.0".into(),
            checksum: None,
        }],
    };
    let json = serde_json::to_value(&draft).expect("serializes");
    assert!(
        json.get("id").is_none(),
        "drafts must not carry identifiers"
    );
    assert!(json.get("plan_step_id").is_none());
}

#[test]
fn idempotency_keys_are_built_core_host_side() {
    // ATH-CORE-035: `plan_step_id + ":" + attempt`.
    let step = PlanStepId::new();
    let key = crate::protocol::idempotency_key(&step, 2);
    assert_eq!(key, format!("{}:2", step.as_str()));

    let request = CoreRequest::ExecuteTool {
        task_id: TaskId::new(),
        tool: ToolName::FsRead,
        args: serde_json::json!({}),
        idempotency_key: key.clone(),
    };
    assert_eq!(request.idempotency_key(), Some(key.as_str()));

    // Only `delegate` and `execute_tool` carry keys.
    let without = CoreRequest::SubmitUnderstanding {
        task_id: TaskId::new(),
        understanding: sample_understanding(),
    };
    assert_eq!(without.idempotency_key(), None);
}

#[test]
fn error_classes_cover_the_nine_taxonomy_values_with_http_mapping() {
    assert_eq!(ErrorClass::ALL.len(), 9);
    let names: Vec<String> = ErrorClass::ALL
        .iter()
        .map(|c| {
            serde_json::to_value(c)
                .expect("logged")
                .as_str()
                .expect("string")
                .into()
        })
        .collect();
    assert_eq!(
        names,
        vec![
            "validation_error",
            "permission_error",
            "policy_error",
            "transient_error",
            "timeout_error",
            "agent_error",
            "skill_error",
            "runtime_error",
            "internal_error",
        ]
    );

    assert!(ErrorClass::ValidationError.http_statuses().contains(&400));
    assert!(ErrorClass::PermissionError.http_statuses().contains(&403));
    assert!(ErrorClass::TransientError.http_statuses().contains(&429));
    assert!(ErrorClass::InternalError.http_statuses().contains(&500));
    assert!(ErrorClass::RuntimeError.http_statuses().contains(&503));
    assert!(ErrorClass::TimeoutError.http_statuses().contains(&504));
}

#[test]
fn the_response_contract_answers_every_request() {
    // ATH-CORE-034: one response per request; refusals carry the §22.2 envelope.
    let envelope = ErrorEnvelope {
        class: ErrorClass::PermissionError,
        code: "PRIVILEGED_REQUEST_DENIED".into(),
        message: "tool outside the agent grant".into(),
        details: serde_json::json!({ "tool": "fs_write" }),
        correlation_id: "cor-1".into(),
    };
    assert!(
        crate::protocol::denial_is_recordable(&envelope),
        "recordable = the refusal carries its correlation id (ATH-API-009)"
    );

    let ok: CoreResponse<serde_json::Value> = CoreResponse::Ok {
        request_id: "req-1".into(),
        data: serde_json::json!({ "plan_id": "plan-x" }),
    };
    let err: CoreResponse<serde_json::Value> = CoreResponse::Err {
        request_id: "req-1".into(),
        error: envelope,
    };
    assert_eq!(ok.request_id(), "req-1");
    assert_eq!(err.request_id(), "req-1");
    // Both arms ride the same payload shape for the Core Agent's parser.
    let _ = ok.to_payload_json().expect("serializes");
    let _ = err.to_payload_json().expect("serializes");

    // A denial without a correlation chain is not recordable.
    let anonymous = ErrorEnvelope {
        class: ErrorClass::InternalError,
        code: "X".into(),
        message: "m".into(),
        details: serde_json::json!({}),
        correlation_id: String::new(),
    };
    assert!(!crate::protocol::denial_is_recordable(&anonymous));

    let _ = Ok::<(), ()>(());
}

#[test]
fn the_producer_persona_procedure_files_describe_valid_wire_form() {
    // P1-44: `agents/producer/core-requests.md` enumerates exactly the twelve kinds in
    // their wire tags; the protocol's own catalog is the authority.
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../agents/producer/core-requests.md"
    ))
    .expect("the Core Agent command definition exists");
    for kind in crate::protocol::REQUEST_KINDS {
        assert!(
            text.contains(&format!("\"{kind}\"")),
            "the wire tag {kind} must appear in the procedure file"
        );
    }
    // The idempotency and base-revision rules carry their anchors here too (the agent's
    // invariants and Core Host's are each other's sentence).
    assert!(
        text.contains("idempotency_key"),
        "the key rule rides in the file"
    );
    assert!(
        text.contains("PLAN_BASE_REVISION_CONFLICT"),
        "the conflict code rides"
    );
}

// --- P4-20: the §20.1 disposition table (ATH-FAIL-001/002) -----------------------------------

#[test]
fn every_class_carries_its_fixed_disposition() {
    use crate::protocol::{ErrorClass, Escalation, RetryKind};
    let expected: [(ErrorClass, RetryKind, bool, Escalation); 9] = [
        (
            ErrorClass::ValidationError,
            RetryKind::Never,
            false,
            Escalation::None,
        ),
        (
            ErrorClass::PermissionError,
            RetryKind::Never,
            false,
            Escalation::Human,
        ),
        (
            ErrorClass::PolicyError,
            RetryKind::Never,
            false,
            Escalation::Human,
        ),
        (
            ErrorClass::TransientError,
            RetryKind::ExponentialBackoff,
            false,
            Escalation::None,
        ),
        (
            ErrorClass::TimeoutError,
            RetryKind::Bounded,
            false,
            Escalation::None,
        ),
        (
            ErrorClass::AgentError,
            RetryKind::AlternateAgent,
            true,
            Escalation::None,
        ),
        (
            ErrorClass::SkillError,
            RetryKind::AlternateSkill,
            true,
            Escalation::None,
        ),
        (
            ErrorClass::RuntimeError,
            RetryKind::RecreateInstance,
            false,
            Escalation::None,
        ),
        (
            ErrorClass::InternalError,
            RetryKind::Never,
            false,
            Escalation::None,
        ),
    ];
    for (class, retry, replan, escalation) in expected {
        let d = class.disposition();
        assert_eq!(d.retry, retry, "{class:?}");
        assert_eq!(d.replan, replan, "{class:?}");
        assert_eq!(d.escalation, escalation, "{class:?}");
    }
    // And every class has a row (the table is exhaustive).
    assert_eq!(expected.len(), ErrorClass::ALL.len());
}

#[test]
fn unclassifiable_errors_become_internal_error_with_no_retry() {
    use crate::protocol::{ErrorClass, RetryKind};
    // The face: an unclassifiable error maps to the class that never retries
    // (`ATH-FAIL-002`).
    let mapped = ErrorClass::unclassifiable();
    assert_eq!(mapped, ErrorClass::InternalError);
    assert!(matches!(mapped.disposition().retry, RetryKind::Never));
    // The escalation route too: the retry *budget* must never fire for it (P4-21's
    // budget reads this disposition).
    assert!(!mapped.disposition().replan);
}
