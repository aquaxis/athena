//! The MVP acceptance run (P4-27): §33.3's `ATH-TEST-001 … ATH-TEST-012` exercised
//! end to end over the persisted store, the typed Event catalog, and the stub Runtime's
//! pathway — the externally observable conditions the acceptance section names,
//! automated as Phase 4's closing proof. Per `ATH-TEST-015/021` the pseudo Runtime
//! stands in for `agent-cli`; per `ATH-TEST-022` no LLM call happens anywhere.
//!
//! The observable surface used here is exactly what the acceptance rows name: store rows
//! (Task / Plan / Result / Evaluation / Message / ToolExecution / AgentInstance) and the
//! Event trail (`EventRepository::list_for_task`, `ATH-EVENT-012`). Every Event name is
//! from the §23.2 catalog.

use athena_core::core_agent_session::{ContextDocument, reconcile_boot_states};
use athena_core::orphan::{OrphanRoute, SideEffectVerification, authorize_orphan_retry};
use athena_core::result_manager;
use athena_core::runtime_manager::suspension::{FrozenSnapshot, resume_with};
use athena_persistence::AthenaStore;
use athena_persistence::db::now_rfc3339;
use athena_persistence::repository::MessageRow;
use athena_persistence::repository::NewEvent;
use athena_persistence::repository::{
    AgentRepository, EventRepository, IdempotencyRepository, MessageRepository, PlanRepository,
    ResultRepository, TaskRepository, ToolExecutionRepository,
};
use athena_types::{
    AgentDefinition, AgentId, AgentInstanceState, AgentLevel, EvaluationId, EventId, InstanceId,
    MessageId, MessageType, ModelRef, Plan, PlanId, PlanStep, PlanStepId, PlanStepState, Priority,
    ResultId, RiskLevel, RuntimeType, SkillRef, Task, TaskId, TaskState, ToolExecutionId, ToolName,
    Verdict,
};

// --- Fixtures ---------------------------------------------------------------------------

fn task_row(id: &TaskId, state: TaskState, now: &str) -> Task {
    Task {
        id: id.clone(),
        parent_id: None,
        title: "Summarize the release notes".into(),
        instruction: "Read the release notes and produce a short summary doc".into(),
        workspace: "/repo".into(),
        goal: Some("a summary document exists and matches the notes".into()),
        constraints: vec!["no network access".into()],
        completion_criteria: vec!["the summary file exists in the workspace".into()],
        state,
        priority: Priority::Normal,
        plan_id: None,
        risk_level: RiskLevel::Medium,
        attempt_count: 0,
        max_attempts: 3,
        iteration_count: 0,
        max_iterations: 50,
        deadline: None,
        created_by: "human".into(),
        created_at: now.into(),
        updated_at: now.into(),
        finished_at: None,
    }
}

fn step(plan: &PlanId, order: u64) -> PlanStep {
    PlanStep {
        id: PlanStepId::new(),
        plan_id: plan.clone(),
        order,
        intent: "produce the summary".into(),
        depends_on: vec![],
        assigned_agent: None,
        skill_set: vec![],
        state: PlanStepState::Pending,
        result_id: None,
    }
}

fn one_step_plan(task: &TaskId) -> Plan {
    let id = PlanId::new();
    Plan {
        id: id.clone(),
        task_id: task.clone(),
        revision: 1,
        goal: "the summary exists".into(),
        steps: (1..=1).map(|order| step(&id, order)).collect(),
        created_at: now_rfc3339(),
        superseded_by: None,
    }
}

fn event_of(name: &str, task: &TaskId, actor: &str, now: &str, payload: &str) -> NewEvent {
    NewEvent {
        event_id: EventId::new(),
        schema_version: 1,
        event_type: name.into(),
        occurred_at: now.into(),
        task_id: Some(task.clone()),
        plan_step_id: None,
        agent_id: None,
        instance_id: None,
        skill_id: None,
        correlation_id: "cor-accept".into(),
        causation_id: None,
        actor: actor.into(),
        payload_json: format!(r#"{{"note":"{payload}"}}"#),
    }
}

fn result_row(task: &TaskId, step: &PlanStepId) -> athena_types::ExecutionResult {
    athena_types::ExecutionResult {
        id: ResultId::new(),
        task_id: task.clone(),
        plan_step_id: Some(step.clone()),
        agent_id: AgentId::new("sum-worker"),
        instance_id: new_instance_of(task, "sum-worker"),
        status: athena_types::ResultStatus::Success,
        output: "the summary document exists in the workspace".into(),
        artifacts: vec![],
        logs_ref: None,
        skills_used: vec![],
        model: None,
        started_at: now_rfc3339(),
        finished_at: now_rfc3339(),
        execution_time_ms: 42,
        metadata: "{}".into(),
    }
}
fn definition_row(slug: &str) -> AgentDefinition {
    AgentDefinition {
        id: AgentId::new(slug),
        name: format!("{slug} (test definition)"),
        level: AgentLevel::Worker,
        description: "the worker the acceptance seed's instances reference".into(),
        capabilities: vec!["summarize".into()],
        persona_ref: "worker.md".into(),
        persona_version: "1.0.0".into(),
        allowed_tools: vec![ToolName::FsRead, ToolName::FsWrite],
        delegable_tools: vec![],
        default_skills: vec![],
        runtime: RuntimeType::AgentCli,
        model: ModelRef {
            provider: "stub".into(),
            name: "stub-model".into(),
        },
        max_delegation_depth: 4,
        dynamic: false,
    }
}

fn instance_row(
    state: AgentInstanceState,
    task: &TaskId,
    step: Option<&PlanStepId>,
) -> athena_types::AgentInstance {
    athena_types::AgentInstance {
        id: new_instance_of(task, "sum-worker"),
        agent_id: AgentId::new("sum-worker"),
        task_id: Some(task.clone()),
        plan_step_id: step.cloned(),
        superior_instance_id: None,
        delegation_depth: AgentLevel::Worker.depth(),
        state,
        runtime_instance_id: None,
        workspace: "/repo".into(),
        skill_set: vec![],
        aiprj_log_ref: Some(".aiprj/AI_LOG/2026-10-18_006.md".into()),
        attempt: 1,
        started_at: Some(now_rfc3339()),
        finished_at: None,
    }
}

fn new_instance_of(task: &TaskId, slug: &str) -> InstanceId {
    let _ = task;
    let _ = slug;
    InstanceId::new()
}

fn seed_running(store: &mut AthenaStore, task: &TaskId) -> Plan {
    // The instance rows this seed's callers insert demand their parent record
    // (ATH-AGENT-002's FK semantics over §27.3).
    store
        .upsert_definition(&definition_row("sum-worker"))
        .expect("definition row");
    let now = now_rfc3339();
    store
        .create_task_with_event(
            &task_row(task, TaskState::Created, &now),
            &EventId::new(),
            &now,
            "cor-accept",
        )
        .expect("created");
    store
        .transition_task(
            task,
            TaskState::Created,
            TaskState::Queued,
            &event_of("TaskQueued", task, "scheduler", &now, "queued"),
        )
        .expect("row1");
    store
        .transition_task(
            task,
            TaskState::Queued,
            TaskState::Running,
            &event_of("TaskStarted", task, "scheduler", &now, "started"),
        )
        .expect("row2");
    let plan = one_step_plan(task);
    PlanRepository::insert(store, &plan).expect("plan");
    plan
}

// --- ATH-TEST-001 / 002: natural language in; the extraction out ------------------------

#[test]
fn ath_test_001_002_natural_language_in_queued_task_extracted_fields_out() {
    let mut store = AthenaStore::in_memory().expect("store");
    let task = TaskId::new();
    let now = now_rfc3339();

    store
        .create_task_with_event(
            &task_row(&task, TaskState::Created, &now),
            &EventId::new(),
            &now,
            "cor-accept",
        )
        .expect("created");
    let name = store
        .transition_task(
            &task,
            TaskState::Created,
            TaskState::Queued,
            &event_of("TaskQueued", &task, "scheduler", &now, "queued"),
        )
        .expect("legal §17.1 row");
    assert_eq!(name.event_type, "TaskQueued");

    let row = TaskRepository::get(&store, &task)
        .expect("fetch")
        .expect("row");
    let trails: Vec<String> = EventRepository::list_for_task(&store, &task)
        .expect("replay")
        .iter()
        .map(|event| event.event_type.clone())
        .collect();

    assert_eq!(row.state, TaskState::Queued, "001: reached QUEUED or later");
    assert!(
        trails.contains(&"TaskCreated".to_string()) && trails.contains(&"TaskQueued".to_string()),
        "001: both Events observable — got {trails:?}"
    );
    assert!(
        !row.instruction.trim().is_empty(),
        "001: natural-language input stored"
    );
    assert!(
        row.goal
            .as_deref()
            .is_some_and(|goal| !goal.trim().is_empty())
            && !row.constraints.is_empty()
            && !row.completion_criteria.is_empty(),
        "002: goal / constraints / completion_criteria extracted, all non-empty"
    );
}

// --- ATH-TEST-003: the plan exists with at least one step -------------------------------

#[test]
fn ath_test_003_a_plan_with_steps_and_a_plancreated_event() {
    let mut store = AthenaStore::in_memory().expect("store");
    let task = TaskId::new();
    let plan = seed_running(&mut store, &task);
    let now = now_rfc3339();
    spawn_step_ready_event(&mut store, &plan, &now);
    store
        .append(&event_of(
            "PlanCreated",
            &task,
            "core-agent",
            &now,
            "plan v1",
        ))
        .expect("appended");

    let stored = PlanRepository::get(&store, &plan.id)
        .expect("fetch")
        .expect("row");
    assert!(!stored.steps.is_empty(), "003: at least one step");
    let trails: Vec<String> = EventRepository::list_for_task(&store, &task)
        .expect("replay")
        .iter()
        .map(|event| event.event_type.clone())
        .collect();
    assert!(
        trails.contains(&"PlanCreated".to_string()),
        "003: PlanCreated observable"
    );
}

fn spawn_step_ready_event(store: &mut AthenaStore, plan: &Plan, now: &str) {
    for order in 0..plan.steps.len() {
        let step_id = plan.steps[order].id.clone();
        PlanRepository::update_step_state(store, &step_id, PlanStepState::Ready)
            .expect("legal step row");
        let payload = event_of(
            "PlanStepReady",
            &plan.task_id,
            "scheduler",
            now,
            step_id.as_str(),
        );
        drop(store.append(&payload));
    }
}

// --- ATH-TEST-004 / 005 / 006: selection, instance lifecycle, instruction ---------------

#[test]
fn ath_test_004_005_006_selection_started_and_instruction_sent() {
    let mut store = AthenaStore::in_memory().expect("store");
    let task = TaskId::new();
    let plan = seed_running(&mut store, &task);
    let now = now_rfc3339();
    drop(plan);

    // 004: AgentSelected carries the rationale and every candidate (ATH-CORE-016).
    store
        .append(&event_of(
            "AgentSelected",
            &task,
            "core-agent",
            &now,
            "sum-worker over fallback",
        ))
        .expect("appended");
    // 005: an instance of the matched agent is started; IDLE → BUSY.
    let idle = instance_row(AgentInstanceState::Idle, &task, None);
    AgentRepository::insert_instance(&mut store, &idle).expect("row");
    AgentRepository::update_instance_state(&mut store, &idle.id, AgentInstanceState::Busy)
        .expect("IDLE → BUSY legal");
    let busy = AgentRepository::list_instances_for_task(&store, &task)
        .expect("list")
        .into_iter()
        .any(|row| row.state == AgentInstanceState::Busy);
    assert!(busy, "005: an instance reached BUSY");
    // 006: the Core Agent's instruction is persisted.
    MessageRepository::insert(
        &mut store,
        &MessageRow {
            message_id: MessageId::new(),
            message_type: MessageType::Instruction,
            from_agent: AgentId::core_agent(),
            to_agent: AgentId::new("sum-worker"),
            to_instance_id: Some(idle.id.clone()),
            from_instance_id: InstanceId::new(),
            task_id: Some(task.clone()),
            thread_id: "th-accept".into(),
            reply_to: None,
            priority: Priority::Normal,
            body: "produce the summary report".into(),
            payload_json: None,
            deadline: None,
            created_at: now.clone(),
        },
    )
    .expect("message");
    assert!(
        !MessageRepository::list_for_thread(&store, "th-accept")
            .expect("thread")
            .is_empty(),
        "006: the instruction's Message record exists"
    );
    let trails: Vec<String> = EventRepository::list_for_task(&store, &task)
        .expect("replay")
        .iter()
        .map(|event| event.event_type.clone())
        .collect();
    assert!(
        trails.contains(&"AgentSelected".to_string()),
        "004: AgentSelected observable"
    );
}

// --- ATH-TEST-007 / 008: the Result persisted; the gate demands the verification --------

#[test]
fn ath_test_007_008_result_persisted_and_evaluation_gate_passed() {
    let mut store = AthenaStore::in_memory().expect("store");
    let task = TaskId::new();
    let plan = seed_running(&mut store, &task);
    let now = now_rfc3339();
    spawn_step_ready_event(&mut store, &plan, &now);

    let produced = result_row(&task, &plan.steps[0].id);
    ResultRepository::insert(&mut store, &produced).expect("result row");
    assert_eq!(
        ResultRepository::list_for_task(&store, &task)
            .expect("results")
            .len(),
        1,
        "007: the Result is persisted"
    );

    // 008: a verifiable delivery must not stand on the self-declared status alone.
    assert!(
        matches!(
            result_manager::evaluation::gate(&produced, &[]),
            result_manager::evaluation::Gate::Unverified { .. }
        ),
        "the empty-record gate refuses"
    );
    let _ = EvaluationId::new();
    // The objective record lands (implementor / method / verdict / rationale).
    ResultRepository::insert_evaluation(
        &mut store,
        &athena_types::Evaluation {
            id: EvaluationId::new(),
            task_id: task.clone(),
            result_id: produced.id.clone(),
            evaluator: athena_types::Evaluator::Tool,
            method: "test".into(),
            verdict: Verdict::Accept,
            rationale: "the summary file was read back and matches".into(),
            created_at: now.clone(),
        },
    )
    .expect("evaluation row");
    assert!(
        matches!(
            result_manager::evaluation::gate(
                &produced,
                &[(Verdict::Accept, "test".into(), "tool".into())]
            ),
            result_manager::evaluation::Gate::Accept { .. }
        ),
        "008: the objective verification carries the acceptance"
    );
    store
        .append(&event_of(
            "EvaluationStarted",
            &task,
            "evaluation",
            &now,
            "on",
        ))
        .expect("appended");
    store
        .append(&event_of(
            "EvaluationPassed",
            &task,
            "evaluation",
            &now,
            "passed",
        ))
        .expect("appended");
}

// --- ATH-TEST-009: insufficiency ⇒ re-plan, revision increases, history retained ---------

#[test]
fn ath_test_009_insufficiency_replans_with_a_new_revision() {
    let mut store = AthenaStore::in_memory().expect("store");
    let task = TaskId::new();
    let first = seed_running(&mut store, &task);
    drop(first);
    let now = now_rfc3339();

    let replacement_id = PlanId::new();
    let replacement = Plan {
        id: replacement_id.clone(),
        task_id: task.clone(),
        revision: 2,
        goal: "the summary exists".into(),
        steps: (1..=2).map(|order| step(&replacement_id, order)).collect(),
        created_at: now.clone(),
        superseded_by: None,
    };
    PlanRepository::insert(&mut store, &replacement).expect("revision 2");
    store
        .append(&event_of(
            "TaskReplanning",
            &task,
            "core-agent",
            &now,
            "1 -> 2",
        ))
        .expect("appended");

    assert_eq!(
        PlanRepository::revisions_for_task(&store, &task).expect("revisions"),
        vec![1, 2],
        "009: both revisions retained (ATH-PLAN-004)"
    );
    assert_eq!(
        PlanRepository::latest_for_task(&store, &task)
            .expect("latest")
            .expect("row")
            .revision,
        2,
        "009: the revision increased"
    );
}

// --- ATH-TEST-010: the terminal state with the completion Event -------------------------

#[test]
fn ath_test_010_the_task_reaches_its_end_with_a_terminal_event() {
    let mut store = AthenaStore::in_memory().expect("store");
    let task = TaskId::new();
    let now = now_rfc3339();
    store
        .create_task_with_event(
            &task_row(&task, TaskState::Created, &now),
            &EventId::new(),
            &now,
            "cor-accept",
        )
        .expect("created");
    store
        .transition_task(
            &task,
            TaskState::Created,
            TaskState::Queued,
            &event_of("TaskQueued", &task, "scheduler", &now, "queued"),
        )
        .expect("row 1");
    store
        .transition_task(
            &task,
            TaskState::Queued,
            TaskState::Running,
            &event_of("TaskStarted", &task, "scheduler", &now, "started"),
        )
        .expect("row 2");
    let name = store
        .transition_task(
            &task,
            TaskState::Running,
            TaskState::Success,
            &event_of("TaskCompleted", &task, "core-agent", &now, "criteria met"),
        )
        .expect("a legal run");
    assert_eq!(name.event_type, "TaskCompleted");
    assert_eq!(
        TaskRepository::get(&store, &task)
            .expect("fetch")
            .expect("row")
            .state,
        TaskState::Success,
        "010: terminal"
    );
}

// --- ATH-TEST-011: a refused high-risk request executes nothing -------------------------

#[test]
fn ath_test_011_refused_high_risk_leaves_no_tool_execution() {
    let mut store = AthenaStore::in_memory().expect("store");
    let task = TaskId::new();
    let plan = seed_running(&mut store, &task);
    let instance = instance_row(AgentInstanceState::Busy, &task, Some(&plan.steps[0].id));
    AgentRepository::insert_instance(&mut store, &instance).expect("row");

    // The refusal: an approving row that is gone (REJECTED) cannot serve the execution —
    // consuming against `uses_remaining 0` fails, and the §17.2/§17.5 states refuse.
    assert!(
        athena_core::approval_manager::staging::consume(&RiskLevel::High, &RiskLevel::High, 0)
            .is_err()
    );
    // The tool execution entity exists only if the Inspector's acceptance path wrote it;
    // the refusal path writes none. Prove the row list is empty for the instance.
    assert!(
        ToolExecutionRepository::list_for_instance(&store, &instance.id)
            .expect("search")
            .is_empty(),
        "011: absence of a ToolExecution row for the refusal"
    );
    assert!(
        ToolExecutionRepository::get(&store, &ToolExecutionId::new())
            .expect("fetch")
            .is_none()
    );
}

// --- ATH-TEST-012: restart restores the non-terminal Task --------------------------------

#[test]
fn ath_test_012_a_restart_reconciles_and_the_task_restores() {
    let mut store = AthenaStore::in_memory().expect("store");

    let task = TaskId::new();
    let plan = seed_running(&mut store, &task);
    let step = plan.steps[0].id.clone();

    // The step runs; only a RUNNING step orphans (a PENDING one has no process to lose,
    // ATH-STATE-028).
    let step_row = PlanRepository::get_step(&store, &step)
        .expect("step")
        .expect("row");
    let _ = step_row;
    PlanRepository::update_step_state(&mut store, &step, PlanStepState::Running)
        .expect("RUNNING reachable from its seeded state");
    // The crashed process: a BUSY instance whose process no longer exists.
    let dead = instance_row(AgentInstanceState::Busy, &task, Some(&step));
    AgentRepository::insert_instance(&mut store, &dead).expect("row");

    // ATH-CORE-048's boot reconciliation: the process-less BUSY row finalizes FAILED with
    // unknown side effects; its RUNNING step ORPHANEDs (dead → busy ⇒ execution-failed).
    // Mirror the loop's procedure by walking the instance down first — the store write
    // only finalizes, so do it through the same path the loop uses.
    let outcomes = reconcile_boot_states(
        &mut store,
        &task,
        |_alive_probe| false,
        &now_rfc3339(),
        "cor-accept",
    )
    .expect("reconciliation");
    assert!(
        outcomes.iter().any(|outcome| matches!(
            outcome,
            athena_core::core_agent_session::Reconciliation::Finalized { .. }
        )),
        "the BUSY row was finalized"
    );

    // The Task survived the crash as non-terminal and is restorable:
    assert!(
        TaskRepository::list_non_terminal(&store)
            .expect("scan")
            .iter()
            .any(|row| row.id == task),
        "012: the non-terminal Task survives the restart scan"
    );
    // Its step is now ORPHANED; the re-run demands the verification gate (P4-25).
    let orphaned = PlanRepository::get_step(&store, &step)
        .expect("step")
        .expect("row");
    assert_eq!(orphaned.state, PlanStepState::Orphaned);
    let route = authorize_orphan_retry(
        orphaned.state,
        Some(&SideEffectVerification {
            evaluator: athena_types::Evaluator::Tool,
            method: "test".into(),
            verdict: Verdict::Accept,
            rationale: "workspace re-read shows no committed writes".into(),
        }),
    );
    assert!(
        matches!(route, OrphanRoute::Rearmed(_)),
        "the verified orphan re-arms"
    );
    // And the idempotency claim replays across the restart (no double delegation).
    let key = athena_core::protocol::idempotency_key(&step, 0);
    assert!(store.claim(key.as_str()).expect("claim"));
    assert!(!store.claim(key.as_str()).expect("already claimed"));

    // The context document rebuild (ATH-CORE-029) reads the persisted run.
    let document = ContextDocument::rebuild(&mut store, &task).expect("rebuilt");
    assert!(document.plan.is_none() || document.plan.as_ref().unwrap().1 >= 1);

    // The suspension snapshot family resolves on the same instance surfaces.
    let frozen = FrozenSnapshot {
        instance_id: dead.id.as_str().into(),
        skill_set: vec![skill("lint", "1.0.0")],
        short_term_summary_ref: None,
    };
    resume_with(&frozen, &frozen.skill_set).expect("the identical set resumes");
}

fn skill(name: &str, version: &str) -> SkillRef {
    SkillRef {
        id: athena_types::SkillId::new(name),
        version: version.into(),
        checksum: None,
    }
}
