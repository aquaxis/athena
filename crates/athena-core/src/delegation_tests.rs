//! The delegation intake's admission rules (X-44): the single-flight rule, the parent
//! record, the delegation ceiling, the idempotency claim's replay, and the row's shape.

use crate::delegation::{DelegationFault, DelegationRequest, accept_delegation};
use athena_types::{
    AgentDefinition, AgentId, AgentInstanceState, AgentLevel, ModelRef, PlanStepDraft, RuntimeType,
    Task, TaskId, TaskState, ToolName,
};

const NOW: &str = "2026-11-12T02:00:00.000Z";
/// The instances tree a delegation composes its Workspace under (X-41's fence layout).
const INSTANCES_ROOT: &str = "/repo/.athena/var/instances";

fn definition(id: &str, level: AgentLevel, ceiling: u64) -> AgentDefinition {
    AgentDefinition {
        id: AgentId::new(id),
        name: "X44 Agent".into(),
        level,
        description: "the definition the delegation rows reference".into(),
        capabilities: vec![],
        persona_ref: "worker".into(),
        persona_version: "1.0.0".into(),
        allowed_tools: vec![ToolName::FsRead],
        delegable_tools: vec![],
        default_skills: vec![],
        runtime: RuntimeType::AgentCli,
        model: ModelRef {
            provider: "anthropic".into(),
            name: "claude-opus-5".into(),
        },
        max_delegation_depth: ceiling,
        dynamic: false,
    }
}

fn task_row() -> Task {
    Task {
        id: TaskId::new(),
        parent_id: None,
        title: "x44".into(),
        instruction: "wire the delegation intake".into(),
        workspace: "/repo/x44".into(),
        goal: Some("delegate one step".into()),
        constraints: vec![],
        completion_criteria: vec![],
        state: TaskState::Running,
        priority: athena_types::Priority::Normal,
        plan_id: None,
        risk_level: athena_types::RiskLevel::Medium,
        attempt_count: 0,
        max_attempts: 3,
        iteration_count: 0,
        max_iterations: 50,
        deadline: None,
        created_by: "test".into(),
        created_at: "2026-11-12T00:00:00.000Z".into(),
        updated_at: "2026-11-12T00:00:00.000Z".into(),
        finished_at: None,
    }
}

/// A store holding one Task, one plan with one step, and the named definition — the ground
/// a delegation needs. Returns the step the plan minted.
fn seeded(
    agent: &str,
    level: AgentLevel,
    ceiling: u64,
) -> (crate::AthenaStore, Task, athena_types::PlanStepId) {
    let mut store = crate::AthenaStore::in_memory().expect("store");
    let task = task_row();
    crate::TaskRepository::insert(&mut store, &task).expect("task row");
    crate::AgentRepository::upsert_definition(&mut store, &definition(agent, level, ceiling))
        .expect("definition");
    // The plan goes through the real intake, so the step id is Core-minted exactly as a
    // live delegation would find it (X-43's path).
    let accepted = crate::plan_intake::accept_plan(
        &mut store,
        &task.id,
        0,
        vec![PlanStepDraft {
            order: 1,
            intent: "the step to delegate".into(),
            depends_on_orders: vec![],
            assigned_agent: Some(AgentId::new(agent)),
            skill_set: vec![],
        }],
        NOW,
    )
    .expect("plan lands");
    let plan = crate::PlanRepository::get(&store, &accepted.plan_id)
        .expect("read")
        .expect("plan");
    let step_id = plan.steps.first().expect("one step").id.clone();
    (store, task, step_id)
}

fn request(
    task: &Task,
    step: &athena_types::PlanStepId,
    agent: &str,
    attempt: u64,
) -> DelegationRequest {
    DelegationRequest {
        task_id: task.id.clone(),
        plan_step_id: step.clone(),
        agent_id: AgentId::new(agent),
        skill_set: vec![athena_types::SkillRef {
            id: athena_types::SkillId::new("pcb-review"),
            version: "1.0.0".into(),
            checksum: None,
        }],
        idempotency_key: crate::protocol::idempotency_key(step, attempt),
    }
}

#[test]
fn an_admitted_delegation_writes_the_created_instance_and_records_it() {
    // The row lands in §17.2's entry state with the request's Skill Set frozen onto it
    // (ATH-DATA-006/ATH-SKILL-010) and the attempt the key names (ATH-FAIL-007).
    let (mut store, task, step) = seeded("x44-worker", AgentLevel::Worker, 4);
    let accepted = accept_delegation(
        &mut store,
        &request(&task, &step, "x44-worker", 1),
        std::path::Path::new(INSTANCES_ROOT),
        NOW,
    )
    .expect("the delegation is admitted");

    assert!(!accepted.replayed);
    assert_eq!(accepted.attempt, 1);
    assert_eq!(accepted.delegation_depth, AgentLevel::Worker.depth());

    let row = crate::AgentRepository::get_instance(&store, &accepted.instance_id)
        .expect("read")
        .expect("instance row");
    assert_eq!(
        row.state,
        AgentInstanceState::Created,
        "starting it is the Runtime Manager's edge, not the intake's"
    );
    assert_eq!(row.task_id, Some(task.id.clone()));
    assert_eq!(row.plan_step_id, Some(step.clone()));
    assert_eq!(
        row.workspace,
        format!("{INSTANCES_ROOT}/{}", accepted.instance_id.as_str()),
        "the cwd is the layout X-41's fence recognizes"
    );
    assert_eq!(row.attempt, 1);
    assert_eq!(
        row.skill_set.len(),
        1,
        "the Skill Set is frozen onto the row"
    );
    assert!(
        row.runtime_instance_id.is_none(),
        "no Runtime Instance exists until the row is started"
    );
    assert!(
        row.aiprj_log_ref.is_none(),
        "the area path is recorded by the Runtime, never guessed at creation"
    );

    // The record names the step, the agent and the instance on the Task's chain.
    let events = crate::EventRepository::list_for_task(&store, &task.id).expect("events");
    let delegated = events
        .iter()
        .find(|event| event.event_type == "AgentDelegated")
        .expect("AgentDelegated");
    assert_eq!(delegated.event_id, accepted.event_id);
    assert_eq!(delegated.plan_step_id, Some(step));
    assert_eq!(delegated.instance_id, Some(accepted.instance_id));
    assert_eq!(delegated.agent_id, Some(AgentId::new("x44-worker")));
}

#[test]
fn a_second_delegation_of_a_running_step_is_refused() {
    // ATH-DATA-010: one step runs once. The second attempt at the *same* step while its
    // Instance is non-terminal is refused as policy, and writes nothing.
    let (mut store, task, step) = seeded("x44-worker", AgentLevel::Worker, 4);
    accept_delegation(
        &mut store,
        &request(&task, &step, "x44-worker", 1),
        std::path::Path::new(INSTANCES_ROOT),
        NOW,
    )
    .expect("the first is admitted");

    // A fresh key (attempt 2) so the refusal is the single-flight rule's, not the claim's.
    let fault = accept_delegation(
        &mut store,
        &request(&task, &step, "x44-worker", 2),
        std::path::Path::new(INSTANCES_ROOT),
        NOW,
    )
    .expect_err("the step is already running");

    assert!(matches!(
        fault,
        DelegationFault::StepAlreadyRunning { count: 1, .. }
    ));
    assert_eq!(fault.code(), "PLAN_STEP_ALREADY_RUNNING");
    assert_eq!(fault.class(), crate::protocol::ErrorClass::PolicyError);
    assert_eq!(
        crate::AgentRepository::count_non_terminal_for_step(&store, &step).expect("count"),
        1,
        "no second row was written"
    );
}

#[test]
fn a_repeated_key_replays_the_first_instance_instead_of_starting_a_second() {
    // ATH-NFR-017 / ATH-CORE-035: a Core Agent restart that re-sends the same delegation
    // must not start a second Instance. The claim ledger answers with the first row.
    let (mut store, task, step) = seeded("x44-worker", AgentLevel::Worker, 4);
    let first = accept_delegation(
        &mut store,
        &request(&task, &step, "x44-worker", 1),
        std::path::Path::new(INSTANCES_ROOT),
        NOW,
    )
    .expect("the first is admitted");

    // The step's row is terminal now, so the single-flight rule does not fire and the
    // claim is the gate under test.
    crate::AgentRepository::update_instance_state(
        &mut store,
        &first.instance_id,
        AgentInstanceState::Stopped,
    )
    .expect("terminal");

    let replay = accept_delegation(
        &mut store,
        &request(&task, &step, "x44-worker", 1),
        std::path::Path::new("/somewhere/else"),
        NOW,
    )
    .expect("the repeat is answered");

    assert!(replay.replayed, "the repeat is a replay, not a re-run");
    assert_eq!(replay.instance_id, first.instance_id);
    assert_eq!(
        replay.workspace, first.workspace,
        "the answer names the first execution's Workspace, not this call's root argument"
    );
    assert_eq!(
        crate::AgentRepository::list_instances_for_task(&store, &task.id)
            .expect("rows")
            .len(),
        1,
        "exactly one Instance exists for the key"
    );
}

#[test]
fn a_delegation_to_an_unknown_definition_is_refused() {
    // ATH-AGENT-002: the definition is the Instance's parent record. Without it nothing
    // can be started, so the request is refused rather than rowed.
    let (mut store, task, step) = seeded("x44-worker", AgentLevel::Worker, 4);
    let fault = accept_delegation(
        &mut store,
        &request(&task, &step, "x44-absent", 1),
        std::path::Path::new(INSTANCES_ROOT),
        NOW,
    )
    .expect_err("an unknown definition is refused");
    assert!(matches!(fault, DelegationFault::DefinitionAbsent(_)));
    assert_eq!(fault.code(), "AGENT_DEFINITION_NOT_FOUND");
    assert_eq!(
        crate::AgentRepository::count_non_terminal_for_step(&store, &step).expect("count"),
        0
    );
}

#[test]
fn a_depth_beyond_the_definitions_ceiling_is_refused() {
    // ATH-HIER-005 / ATH-DATA-005: the definition's own ceiling bounds the depth. A Worker
    // sits at depth 3, so a ceiling of 2 refuses it.
    let (mut store, task, step) = seeded("x44-deep", AgentLevel::Worker, 2);
    let fault = accept_delegation(
        &mut store,
        &request(&task, &step, "x44-deep", 1),
        std::path::Path::new(INSTANCES_ROOT),
        NOW,
    )
    .expect_err("the ceiling refuses");
    assert!(matches!(
        fault,
        DelegationFault::DepthExceeded {
            depth: 3,
            ceiling: 2,
            ..
        }
    ));
    assert_eq!(fault.class(), crate::protocol::ErrorClass::PolicyError);
}

#[test]
fn a_refusal_does_not_burn_the_idempotency_key() {
    // The ordering rule: a claim records that the work ran. Claiming one for a refusal
    // would make the corrected retry impossible, so every read-only refusal is answered
    // before the claim.
    let (mut store, task, step) = seeded("x44-worker", AgentLevel::Worker, 4);
    let mut refused = request(&task, &step, "x44-absent", 1);
    let key = refused.idempotency_key.clone();
    accept_delegation(
        &mut store,
        &refused,
        std::path::Path::new(INSTANCES_ROOT),
        NOW,
    )
    .expect_err("refused");
    assert!(
        !crate::IdempotencyRepository::is_claimed(&store, &key).expect("claim state"),
        "the key is still available after a refusal"
    );

    // The corrected request — same key, known definition — is admitted.
    refused.agent_id = AgentId::new("x44-worker");
    let accepted = accept_delegation(
        &mut store,
        &refused,
        std::path::Path::new(INSTANCES_ROOT),
        NOW,
    )
    .expect("the corrected retry is admitted");
    assert!(!accepted.replayed);
    assert!(crate::IdempotencyRepository::is_claimed(&store, &key).expect("claim state"));
}

#[test]
fn a_malformed_idempotency_key_is_refused_before_any_read() {
    // ATH-CORE-035 fixes the key's shape as `plan_step_id:attempt`, and the attempt is a
    // fact the row carries — it is read from the key, never invented.
    let (mut store, task, step) = seeded("x44-worker", AgentLevel::Worker, 4);
    let mut malformed = request(&task, &step, "x44-worker", 1);
    malformed.idempotency_key = "not-a-key".into();
    let fault = accept_delegation(
        &mut store,
        &malformed,
        std::path::Path::new(INSTANCES_ROOT),
        NOW,
    )
    .expect_err("a malformed key is refused");
    assert!(matches!(fault, DelegationFault::MalformedKey(_)));

    // A key naming another step is equally malformed for this request.
    let mut foreign = request(&task, &step, "x44-worker", 1);
    foreign.idempotency_key = crate::protocol::idempotency_key(&athena_types::PlanStepId::new(), 1);
    assert!(matches!(
        accept_delegation(
            &mut store,
            &foreign,
            std::path::Path::new(INSTANCES_ROOT),
            NOW
        )
        .expect_err("a foreign step's key is refused"),
        DelegationFault::MalformedKey(_)
    ));
}

#[test]
fn an_absent_task_or_step_is_refused() {
    // The row references both; an Instance for a step no plan holds would be an orphan at
    // birth, so neither is inferred.
    let (mut store, task, step) = seeded("x44-worker", AgentLevel::Worker, 4);

    let mut no_task = request(&task, &step, "x44-worker", 1);
    no_task.task_id = TaskId::new();
    assert!(matches!(
        accept_delegation(
            &mut store,
            &no_task,
            std::path::Path::new(INSTANCES_ROOT),
            NOW
        )
        .expect_err("absent task"),
        DelegationFault::TaskAbsent(_)
    ));

    let absent_step = athena_types::PlanStepId::new();
    let no_step = request(&task, &absent_step, "x44-worker", 1);
    assert!(matches!(
        accept_delegation(
            &mut store,
            &no_step,
            std::path::Path::new(INSTANCES_ROOT),
            NOW
        )
        .expect_err("absent step"),
        DelegationFault::StepAbsent(_)
    ));
}

#[test]
fn a_later_attempt_after_a_terminal_instance_is_its_own_instance() {
    // ATH-FAIL-007: each attempt is an independent Instance. Once the first attempt's row
    // is terminal, attempt 2 is admitted and rows separately.
    let (mut store, task, step) = seeded("x44-worker", AgentLevel::Worker, 4);
    let first = accept_delegation(
        &mut store,
        &request(&task, &step, "x44-worker", 1),
        std::path::Path::new(INSTANCES_ROOT),
        NOW,
    )
    .expect("attempt 1");
    crate::AgentRepository::update_instance_state(
        &mut store,
        &first.instance_id,
        AgentInstanceState::Stopped,
    )
    .expect("terminal");

    let second = accept_delegation(
        &mut store,
        &request(&task, &step, "x44-worker", 2),
        std::path::Path::new(INSTANCES_ROOT),
        NOW,
    )
    .expect("attempt 2");

    assert!(!second.replayed);
    assert_ne!(second.instance_id, first.instance_id);
    assert_eq!(second.attempt, 2);
    assert_eq!(
        crate::AgentRepository::list_instances_for_task(&store, &task.id)
            .expect("rows")
            .len(),
        2
    );
}
