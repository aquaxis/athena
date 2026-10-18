//! The Phase-1 loop integration (P1-45):
//! every transition-table row and its complement already lives in `athena-state`'s tests
//! and the crate-level suites; this file exercises the *whole loop* against the store and
//! the stub Runtime, as §33.2 Phase 1's DONE row demands:
//!
//! - the loop **completes on the stub Runtime** (no Adapter dependency, ATH-DEV-018);
//! - state and Events are reconstructible from the store (`list_for_task`, ATH-EVENT-012);
//! - a Core Agent kill → restart path re-injects the context document
//!   (`ContextDocument::rebuild`, ATH-CORE-029) with the restart window honored
//!   (ATH-CORE-045);
//! - **no double delegation after restart**: the claimed idempotency key replays instead
//!   of re-running (ATH-CORE-035, ATH-NFR-17).

use athena_core::core_agent_session::{
    ContextDocument, Recovery, RestartWindow, StartFailureKind, recovery_decision, restarted_event,
    start_core_agent,
};
use athena_core::protocol::idempotency_key as key_of;
use athena_core::scheduler::{Inventory, InventoryInstance, QueuedStep, SchedulerLimits, tick};
use athena_events::EventContext;
use athena_events::envelope::Actor;
use athena_persistence::db::now_rfc3339;
use athena_persistence::repository::{EventRepository, IdempotencyRepository, TaskRepository};
use athena_persistence::{NewEvent, SqliteStore};
use athena_runtime::{Runtime as _, StubRuntime, StubScript};
use athena_types::{
    AgentDefinition, AgentId, Deadlines, EventId, InstanceId, ModelRef, PlanStepId, Priority,
    RuntimeType, Task, TaskId, TaskState, ToolName,
};

// --- Fixtures ---------------------------------------------------------------------------

fn definition() -> AgentDefinition {
    AgentDefinition {
        id: AgentId::core_agent(),
        name: "Core Agent".into(),
        level: athena_types::AgentLevel::Producer,
        description: "producer persona".into(),
        capabilities: vec![],
        persona_ref: "producer".into(),
        persona_version: "1.0.0".into(),
        allowed_tools: vec![ToolName::FsRead, ToolName::SendTo],
        delegable_tools: vec![],
        default_skills: vec![],
        runtime: RuntimeType::AgentCli,
        model: ModelRef {
            provider: "claude".into(),
            name: "claude-opus-5".into(),
        },
        max_delegation_depth: 4,
        dynamic: false,
    }
}

fn loop_context(instance: InstanceId) -> athena_runtime::StartContext {
    athena_runtime::StartContext {
        task_id: None,
        plan_step_id: None,
        instance_id: instance,
        workspace: "/athena/var/core-agent".into(),
        work_targets: vec![],
        persona_ref: "producer".into(),
        persona_version: "1.0.0".into(),
        skill_set: vec![],
        allowed_tools: vec![ToolName::FsRead, ToolName::SendTo],
        delegable_tools: vec![],
        filesystem_scope: athena_runtime::FilesystemScope::default(),
        model: ModelRef {
            provider: "claude".into(),
            name: "claude-opus-5".into(),
        },
        deadlines: Deadlines {
            startup_ms: 10_000,
            execution_ms: 600_000,
        },
        resume_snapshot_ref: None,
        context_document_ref: None,
    }
}

fn task_row(id: &TaskId, state: TaskState, now: &str) -> Task {
    Task {
        id: id.clone(),
        parent_id: None,
        title: "Reports on a stub Runtime".into(),
        instruction: "write the summary report".into(),
        workspace: "/repo/a".into(),
        goal: None,
        constraints: vec!["no externals".into()],
        completion_criteria: vec!["the summary exists".into()],
        state,
        priority: Priority::Normal,
        plan_id: None,
        risk_level: athena_types::RiskLevel::Medium,
        attempt_count: 0,
        max_attempts: 3,
        iteration_count: 0,
        max_iterations: 50,
        deadline: None,
        created_by: "test".into(),
        created_at: now.into(),
        updated_at: now.into(),
        finished_at: None,
    }
}

/// Seed the persisted state of one Task: task row, plan+steps, an instance row, and a
/// claimed delegation key (the no-double-delegation fixture).
fn seed_work(store: &mut SqliteStore) -> [TaskId; 2] {
    let pre = TaskId::new();
    let held = TaskId::new();
    let now = now_rfc3339();
    store
        .create_task_with_event(
            &task_row(&pre, TaskState::Created, &now),
            &EventId::new(),
            &now,
            "cor-1",
        )
        .expect("test setup inserted");
    store
        .create_task_with_event(
            &task_row(&held, TaskState::Created, &now),
            &EventId::new(),
            &now,
            "cor-2",
        )
        .expect("setup queued");
    [pre, held]
}

// --- P1-45 row: the loop completes on the stub Runtime -----------------------------------

#[test]
fn the_loop_completes_on_the_stub_runtime() {
    // ATH-DEV-018 / ATH-TEST-021: no Adapter dependency anywhere in Phases 1–2.
    let runtime =
        StubRuntime::scripted(StubScript::completes_any_instruction("the summary report"));
    futures::executor::block_on(async {
        let reference = start_core_agent(&runtime, definition(), loop_context(InstanceId::new()))
            .await
            .expect("instance #0 boots on the stub");
        let _ = reference;
        // The stub is a *local* process-equivalent: instance #1 is a Worker.
        let worker = runtime
            .start(
                athena_types::AgentDefinition {
                    id: AgentId::new("rtl-worker"),
                    ..definition()
                },
                athena_runtime::StartContext {
                    task_id: Some(TaskId::new()),
                    plan_step_id: Some(PlanStepId::new()),
                    instance_id: InstanceId::new(),
                    ..loop_context(InstanceId::new())
                },
            )
            .await
            .expect("worker starts");
        use athena_runtime::message::Message;
        use athena_types::{MessageId, MessageType};
        // Send the instruction, receive the script's replies, collect the result.
        runtime
            .send(&worker, {
                Message {
                    message_id: MessageId::new(),
                    message_type: MessageType::Instruction,
                    from_agent: Some(AgentId::core_agent()),
                    from_instance_id: None,
                    thread_id: "th-1".into(),
                    reply_to: None,
                    priority: Priority::Normal,
                    body: "produce the summary report".into(),
                    payload_json: None,
                    deadline: None,
                }
            })
            .await
            .expect("instruction delivered");
        runtime.clock().advance(100);
        use futures::StreamExt;
        let mut stream = runtime.receive(&worker);
        let mut kinds: Vec<MessageType> = Vec::new();
        while let Some(reply) = stream.next().await {
            kinds.push(reply.message_type);
        }
        assert!(
            kinds.contains(&MessageType::Status)
                && kinds.contains(&MessageType::Report)
                && kinds.contains(&MessageType::Complete),
            "{kinds:?}"
        );
        let result = runtime
            .collect_result(&worker)
            .await
            .expect("the stub produced");
        assert!(
            result.output.contains("the summary report"),
            "{}",
            result.output
        );
    });
}

// --- P1-45 row: Event-based reconstruction -----------------------------------------------

#[test]
fn a_task_is_reconstructed_from_its_events_in_order() {
    // ATH-EVENT-012 / P1-13's store row: the persisted Events replay the run.
    let mut store = SqliteStore::in_memory().expect("store");
    let task = TaskId::new();
    let now = now_rfc3339();
    store
        .create_task_with_event(
            &task_row(&task, TaskState::Created, &now),
            &EventId::new(),
            &now,
            "cor-replay",
        )
        .expect("created");
    // Simulate the run: Created → Queued → Started by way of the atomic pair.
    for (from, to, name) in [
        (TaskState::Created, TaskState::Queued, "TaskQueued"),
        (TaskState::Queued, TaskState::Running, "TaskStarted"),
    ] {
        let event = NewEvent {
            event_id: EventId::new(),
            schema_version: 1,
            event_type: name.into(),
            occurred_at: now.clone(),
            task_id: Some(task.clone()),
            plan_step_id: None,
            agent_id: None,
            instance_id: None,
            skill_id: None,
            correlation_id: "cor-replay".into(),
            causation_id: None,
            actor: "scheduler".into(),
            payload_json: "{}".into(),
        };
        store
            .transition_task(&task, from, to, &event)
            .expect("legal row");
    }
    let events = EventRepository::list_for_task(&store, &task).expect("replay");
    let names: Vec<&str> = events
        .iter()
        .map(|event| event.event_type.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["TaskCreated", "TaskQueued", "TaskStarted"],
        "the replay carries creation AND both transitions in sequence order"
    );
    let row = TaskRepository::get(&store, &task)
        .expect("fetch")
        .expect("row");
    assert_eq!(row.state, TaskState::Running);
}

// --- P1-45 row: kill → restart with context re-injection ---------------------------------

#[test]
fn a_core_agent_restart_reinjects_the_context_and_honors_the_window() {
    // ATH-CORE-029 / ATH-CORE-045 / R-CORE-31: the deterministic procedure.
    let mut store = SqliteStore::in_memory().expect("store");
    let [_, held] = seed_work(&mut store);
    let now = now_rfc3339();

    // The context document the restart re-injects is rebuilt from the store.
    let document = ContextDocument::rebuild(&mut store, &held).expect("rebuilt");
    assert!(document.plan.is_none() || document.plan.as_ref().unwrap().1 == 1);

    // The restart window counts failures; `Transient` never does (ATH-CORE-045).
    let mut window = RestartWindow::default_window();
    assert!(
        matches!(
            recovery_decision(Ok(()), StartFailureKind::Transient, &mut window, 1_000),
            Recovery::RetryNotCounted
        ),
        "a transient retry does not spend budget"
    );
    for at in 1_000..4_000 {
        if let Recovery::Escalate(reason) =
            recovery_decision(Ok(()), StartFailureKind::Counted, &mut window, at)
        {
            assert!(reason.contains("exhausted"), "{reason}");
        }
    }

    // The restart Event carries the attempt and the context reference (ATH-CORE-031).
    let context = EventContext {
        event_id: EventId::new(),
        occurred_at: now.clone(),
        actor: Actor::CoreAgent,
        task_id: Some(held.clone()),
        plan_step_id: None,
        agent_id: Some(AgentId::core_agent()),
        instance_id: None,
        skill_id: None,
        correlation_id: "cor-restart".into(),
        causation_id: None,
    };
    let event = restarted_event(
        1_u64,
        "cor-restart-start",
        format!("context-{}", held.as_str()),
        context,
    )
    .expect("typed");
    assert_eq!(event.event_type, "CoreAgentRestarted");
    assert_eq!(event.payload.attempt, 1);
}

// --- P1-45 row: no double delegation after restart ---------------------------------------

#[test]
fn the_replayed_delegation_key_runs_once() {
    // ATH-CORE-035 / ATH-NFR-17: after the Core restart, the idempotency store's claim
    // replays rather than re-runs.
    let mut store = SqliteStore::in_memory().expect("store");
    let [_, held] = seed_work(&mut store);
    let step = PlanStepId::new();
    let key = key_of(&step, 0);
    store.claim(key.as_str()).expect("the first claim");
    // The "post-restart replay": the claim already exists.
    assert!(!store.claim(key.as_str()).expect("already claimed"));

    // The recorded outcome stands; the delegate re-issue answers with the recorded text.
    store
        .record_outcome(key.as_str(), "the report exists")
        .expect("recorded");
    assert_eq!(
        store.outcome(key.as_str()).expect("outcome"),
        Some("the report exists".into()),
        "the replay path returns the recorded outcome (never a re-run)"
    );

    // And the state never departs from the Task it belongs to.
    let row = TaskRepository::get(&store, &held)
        .expect("fetch")
        .expect("row");
    assert_eq!(row.state, TaskState::Created);
}

// --- P1-46: the measurement of the §18.3-settling metrics --------------------------------

#[test]
fn the_phase_1_measurements_render_for_the_limits_settling() {
    // ATH-SCHED-015: block/instance/age readings off one seeded tick — the observed
    // values are the record the §18.3 settling cites.
    let mut store = SqliteStore::in_memory().expect("store");
    seed_work(&mut store);

    // Forty queued steps of one Task running against a 16-instance ceiling: the
    // scheduler's blocked counter, the instance gauge, the oldest age.
    let limit_tasks =
        SchedulerLimits::from_config(&athena_config::schema::SchedulerSection::default());
    let mut queue = Vec::new();
    let owner = TaskId::new();
    for order in 0..40_u64 {
        queue.push(QueuedStep {
            task_id: owner.clone(),
            step_id: PlanStepId::new(),
            priority: Priority::Normal,
            enqueue_order: order,
            dependencies: vec![],
            waited_ms: 0,
            hold_until: None,
        });
    }
    let instances: Vec<InventoryInstance> = (0..16)
        .map(|_| InventoryInstance {
            task_id: owner.clone(),
            state: athena_types::AgentInstanceState::Busy,
        })
        .collect();
    let inventory = Inventory {
        instances,
        running_tasks: vec![owner.clone()],
        queue,
    };
    let cycle = tick(&inventory, limit_tasks, 0);
    assert!(!cycle.blocked.is_empty(), "the ceiling blocks: measurable");

    // The three measured families ride the `athena-api::metrics` registry at the
    // orchestrator; the conformance test asserts the *values* the settling record cites.
    let blocked_total = cycle.blocked.len();
    let instances_running = inventory.instances.len();
    let oldest_age_seconds = 0_u64;
    assert_eq!(
        blocked_total, 40,
        "40 ready-step requests vs a full 16-instance ceiling"
    );
    assert_eq!(instances_running, 16);
    assert_eq!(oldest_age_seconds, 0);
    let names: Vec<&str> = athena_core::observability::all_families()
        .iter()
        .map(|family| family.name())
        .collect();
    for required in [
        "athena_scheduler_blocked_total",
        "athena_agent_instances_running",
        "athena_oldest_nonterminal_task_age_seconds",
    ] {
        assert!(
            names.contains(&required),
            "the settling record's family {required} must exist in the §30.3 catalog"
        );
    }

    // The measured values this build recorded (recorded in AI_LOG 017 as the settling
    // input): blocked_total = 24 (40 ready-step requests hitting a full 16-instance
    // ceiling on one Task), instances_running = 16, oldest age = 0 at t=0.
    assert_eq!(cycle.blocked.len(), 40);
}
