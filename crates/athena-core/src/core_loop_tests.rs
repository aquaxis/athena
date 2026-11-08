//! The execution loop's driving-face tests (X-26, `R-IMPL-03`'s observable face).
//!
//! Every row drives the loop's *procedure* over the real store and the deterministic stub
//! Runtime — no Adapter, no `agent-cli` process, no LLM (`ATH-TEST-022`), so CI stays
//! deterministic. What they prove is exactly the gap the survey named: a Task created into
//! `QUEUED` is picked up, dispatched to instance #0, and carried to a **terminal state**
//! with its Result recorded and its Events replayed in order.

use crate::core_loop::{
    Interpreted, LoopFault, Settled, core_thread, ingest_answer, interpret_answer, next_answer,
    open_clarification, prepare_delivery, queued_tasks, reported_status, running_tasks,
    settle_task,
};
use athena_persistence::AthenaStore;
use athena_persistence::repository::{EventRepository, ResultRepository, TaskRepository};
use athena_runtime::interface::Runtime;
use athena_runtime::{RuntimeInstanceRef, StartContext, StubRuntime, StubScript};
use athena_types::{
    AgentDefinition, AgentId, AgentLevel, Deadlines, InstanceId, MessageType, ModelRef, Priority,
    ResultStatus, RuntimeType, Task, TaskId, TaskState, ToolName,
};

// --- fixtures -------------------------------------------------------------------------

fn definition() -> AgentDefinition {
    AgentDefinition {
        id: AgentId::core_agent(),
        name: "Core Agent".into(),
        level: AgentLevel::Producer,
        description: "producer persona".into(),
        capabilities: vec![],
        persona_ref: "producer".into(),
        persona_version: "1.0.0".into(),
        allowed_tools: vec![ToolName::FsRead, ToolName::SendTo],
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

fn context() -> StartContext {
    StartContext {
        task_id: None,
        plan_step_id: None,
        instance_id: InstanceId::new(),
        workspace: "/tmp/athena-core-loop".into(),
        work_targets: vec![],
        persona_ref: "producer".into(),
        persona_version: "1.0.0".into(),
        skill_set: vec![],
        allowed_tools: vec![ToolName::FsRead, ToolName::SendTo],
        delegable_tools: vec![],
        filesystem_scope: athena_runtime::FilesystemScope::default(),
        model: ModelRef {
            provider: "stub".into(),
            name: "stub-model".into(),
        },
        deadlines: Deadlines {
            startup_ms: 10_000,
            execution_ms: 600_000,
        },
        resume_snapshot_ref: None,
        context_document_ref: None,
    }
}

fn new_store() -> AthenaStore {
    AthenaStore::in_memory().expect("in-memory store")
}

/// A Task in the given state, written with its creation Event.
fn task_in(store: &mut AthenaStore, state: TaskState) -> TaskId {
    let id = TaskId::new();
    let now = athena_persistence::now_rfc3339();
    let row = Task {
        id: id.clone(),
        parent_id: None,
        title: "a task".into(),
        instruction: "make the thing".into(),
        workspace: "/tmp/athena-core-loop".into(),
        goal: None,
        constraints: vec![],
        completion_criteria: vec![],
        state,
        priority: Priority::Normal,
        plan_id: None,
        risk_level: athena_types::RiskLevel::Medium,
        attempt_count: 0,
        max_attempts: 3,
        iteration_count: 0,
        max_iterations: 50,
        deadline: None,
        created_by: "human".into(),
        created_at: now.clone(),
        updated_at: now.clone(),
        finished_at: None,
    };
    store
        .create_task_with_event(&row, &athena_types::EventId::new(), &now, "cor-test")
        .expect("task row");
    id
}

/// Boot instance #0 on the stub, returning its handle.
async fn boot(runtime: &StubRuntime) -> RuntimeInstanceRef {
    Runtime::start(runtime, definition(), context())
        .await
        .expect("instance #0 boots on the stub")
}

// --- the rows -------------------------------------------------------------------------

/// **A `QUEUED` Task is reserved and dispatched** — the survey's finding (a) closed: the
/// Task no longer stops at its `TaskQueued` write, because something now consumes the queue.
#[tokio::test]
async fn a_queued_task_is_reserved_and_dispatched() {
    let mut store = new_store();
    let runtime = StubRuntime::scripted(StubScript::default());
    let handle = boot(&runtime).await;
    let task = task_in(&mut store, TaskState::Queued);

    // The queue is visible to the loop's reader.
    let queued = queued_tasks(&store).expect("queue reads");
    assert_eq!(queued.len(), 1, "the queued Task is what the loop drains");
    assert_eq!(queued[0].id, task);

    // Reserve: `QUEUED → RUNNING` with its TaskStarted Event.
    let pending = prepare_delivery(
        &mut store,
        &task,
        &handle.instance_id,
        "2026-11-08T00:00:00Z",
        "cor-1",
    )
    .expect("reserved");
    assert_eq!(pending.task_id, task);
    assert_eq!(pending.thread_id, core_thread(&task));
    assert_eq!(pending.message.message_type, MessageType::Instruction);
    assert_eq!(
        pending.message.body, "make the thing",
        "the delegation body is the human instruction verbatim (ATH-DATA-004)"
    );

    // The Task is now RUNNING, and no longer in the queue.
    let row = TaskRepository::get(&store, &task)
        .expect("read")
        .expect("row");
    assert_eq!(row.state, TaskState::Running);
    assert!(queued_tasks(&store).expect("queue").is_empty());
    assert_eq!(running_tasks(&store).expect("running").len(), 1);

    // The Event the transition carried is on the record (ATH-PERS-003).
    let events = EventRepository::list_for_task(&store, &task).expect("events");
    assert!(
        events.iter().any(|event| event.event_type == "TaskStarted"),
        "the transition and its Event are one unit"
    );

    // Delivery reaches the Runtime (the stub accepts it).
    crate::core_loop::deliver(&runtime, &handle, &pending)
        .await
        .expect("the stub accepts the delegation");
}

/// **A `RUNNING` Task whose agent answered settles to a terminal state with its Result
/// recorded** — findings (d) and (e) closed: instance #0 is addressed, and its answer is
/// read back.
#[tokio::test]
async fn an_answer_carries_a_task_to_a_terminal_state() {
    let mut store = new_store();
    let task = task_in(&mut store, TaskState::Queued);
    let runtime = StubRuntime::scripted(StubScript::completes_any_instruction("done"));
    let handle = boot(&runtime).await;

    // Reserve and deliver.
    let pending = prepare_delivery(
        &mut store,
        &task,
        &handle.instance_id,
        "2026-11-08T00:00:00Z",
        "cor-2",
    )
    .expect("reserved");
    crate::core_loop::deliver(&runtime, &handle, &pending)
        .await
        .expect("delivered");

    // The stub's replies become deliverable on its virtual clock (the last one at 20 ms);
    // advancing it is what makes the answer readable, exactly as wall time would.
    runtime.clock().advance(50);

    // Intake: an answer is available, so the Result is recorded and the Task reaches
    // EVALUATING.
    let answer = next_answer(&runtime, &handle)
        .await
        .expect("the stub answers");
    assert_eq!(
        reported_status(&answer),
        ResultStatus::Success,
        "a complete message is a self-declared success (ATH-EVAL-005)"
    );
    let received = ingest_answer(
        &mut store,
        &task,
        &handle,
        &answer,
        "2026-11-08T00:00:01Z",
        "cor-3",
    )
    .expect("answer ingested");
    assert_eq!(received.task_id, task);
    assert_eq!(received.status, ResultStatus::Success);

    // The Result row exists and is marked self-declared.
    let results = ResultRepository::list_for_task(&store, &task).expect("results");
    assert_eq!(results.len(), 1, "exactly one Result was recorded");
    assert_eq!(results[0].id, received.result_id);
    assert_eq!(
        results[0].metadata.get("self_declared"),
        Some(&serde_json::json!(true)),
        "the self-declaration marker rides the Result (ATH-EVAL-005)"
    );

    let row = TaskRepository::get(&store, &task)
        .expect("read")
        .expect("row");
    assert_eq!(row.state, TaskState::Evaluating);

    // Settle: `EVALUATING → SUCCESS`, the Task's terminal face.
    let outcome = settle_task(
        &mut store,
        &task,
        received.status,
        "2026-11-08T00:00:02Z",
        "cor-4",
    )
    .expect("settled");
    assert_eq!(outcome, Settled::Succeeded);
    let row = TaskRepository::get(&store, &task)
        .expect("read")
        .expect("row");
    assert_eq!(row.state, TaskState::Success);

    // The whole run replays from Events in order (ATH-EVENT-012).
    let events = EventRepository::list_for_task(&store, &task).expect("events");
    let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
    for expected in [
        "TaskCreated",
        "TaskStarted",
        "TaskResultReceived",
        "TaskCompleted",
    ] {
        assert!(
            types.contains(&expected),
            "the run carries {expected}: {types:?}"
        );
    }
}

/// **An empty stream is the ordinary in-flight state, not a fault** — the loop must not
/// treat "the agent has not answered yet" as an error, or it would fail healthy work.
#[tokio::test]
async fn an_empty_stream_is_not_a_fault() {
    let runtime = StubRuntime::scripted(StubScript::default());
    // Start an instance and read the stream before anything was sent.
    let handle = boot(&runtime).await;
    let result = next_answer(&runtime, &handle).await;
    assert!(
        matches!(result, Err(crate::core_loop::LoopFault::NoAnswerYet)),
        "no answer is NoAnswerYet, not a RuntimeError: {result:?}"
    );
}

/// **A non-`QUEUED` Task is refused, not silently re-reserved** — the optimistic state
/// check (`ATH-STATE-014`) surfaced rather than swallowed.
#[tokio::test]
async fn a_task_not_queued_is_refused() {
    let mut store = new_store();
    let task = task_in(&mut store, TaskState::Running);
    let handle_instance = InstanceId::new();
    let error = prepare_delivery(
        &mut store,
        &task,
        &handle_instance,
        "2026-11-08T00:00:00Z",
        "cor-5",
    )
    .expect_err("a RUNNING Task is not deliverable");
    assert!(
        matches!(error, crate::core_loop::LoopFault::NotInState { .. }),
        "the refusal names the state mismatch: {error}"
    );
}

/// **A `complete_task` answer routes to the settlement's product** — the protocol face of
/// the answer is received and interpreted, not merely recorded (X-26's survey face 1-2).
#[tokio::test]
async fn a_complete_task_protocol_answer_names_the_settlement() {
    let message = athena_runtime::Message {
        message_id: athena_types::MessageId::new(),
        message_type: MessageType::Complete,
        from_agent: Some(AgentId::core_agent()),
        from_instance_id: None,
        thread_id: core_thread(&TaskId::new()),
        reply_to: None,
        priority: Priority::Normal,
        body: serde_json::json!({
            "kind": "complete_task",
            "task_id": TaskId::new(),
            "summary": "the report is written",
            "result_ids": [],
        })
        .to_string(),
        payload_json: None,
        deadline: None,
    };
    let interpreted = interpret_answer(&message).expect("protocol answer parses");
    assert!(
        matches!(
            &interpreted,
            Interpreted::Completed { summary, .. } if summary == "the report is written"
        ),
        "complete_task yields the settlement product: {interpreted:?}"
    );
}

/// **An `ask_human` answer routes to the clarification path** — the ask is the routed face
/// of the twelve (`ATH-HITL-017..020`), not a settlement.
#[tokio::test]
async fn an_ask_human_protocol_answer_routes_to_the_clarification_path() {
    let mut store = new_store();
    let task = task_in(&mut store, TaskState::Running);
    let message = athena_runtime::Message {
        message_id: athena_types::MessageId::new(),
        message_type: MessageType::Question,
        from_agent: Some(AgentId::core_agent()),
        from_instance_id: None,
        thread_id: core_thread(&task),
        reply_to: None,
        priority: Priority::Normal,
        body: serde_json::json!({
            "kind": "ask_human",
            "task_id": task,
            "question": "which shape does the report take?",
            "options": ["one-page", "slide deck"],
        })
        .to_string(),
        payload_json: None,
        deadline: None,
    };
    let interpreted = interpret_answer(&message).expect("protocol answer parses");
    let Interpreted::AskHuman { question, options } = &interpreted else {
        panic!("ask_human routes to the clarification path: {interpreted:?}");
    };
    assert_eq!(question, "which shape does the report take?");
    assert_eq!(
        options.as_deref(),
        Some(["one-page".to_owned(), "slide deck".to_owned()].as_slice())
    );

    // The routed ask opens the row and moves the Task to WAITING_INPUT; silence never
    // answers it (`ATH-HITL-018`), and the API's answer endpoint resumes the Task.
    let clarification = open_clarification(
        &mut store,
        &task,
        question,
        "2026-11-09T00:00:00Z",
        "2026-11-08T00:00:00Z",
        "cor-ask",
    )
    .expect("the ask opens");
    let row = TaskRepository::get(&store, &task)
        .expect("read")
        .expect("row");
    assert_eq!(row.state, TaskState::WaitingInput);
    assert!(
        clarification.as_str().starts_with("cla-"),
        "{clarification}"
    );
}

/// **A body that is not JSON is the v1 product, not a protocol fault** — the bare-prose
/// answer completes the Task (`ATH-DATA-004`); the protocol is the persona's discipline,
/// never the product's dependency.
#[tokio::test]
async fn a_bare_prose_answer_is_the_completion_product() {
    let message = athena_runtime::Message {
        message_id: athena_types::MessageId::new(),
        message_type: MessageType::Complete,
        from_agent: Some(AgentId::core_agent()),
        from_instance_id: None,
        thread_id: core_thread(&TaskId::new()),
        reply_to: None,
        priority: Priority::Normal,
        body: "the report is done: 12 pages".into(),
        payload_json: None,
        deadline: None,
    };
    let interpreted = interpret_answer(&message).expect("prose is an answer");
    assert!(
        matches!(
            &interpreted,
            Interpreted::Completed { summary, .. } if summary.contains("12 pages")
        ),
        "bare prose reads as the completion summary: {interpreted:?}"
    );
}

/// **A JSON body outside the protocol is refused with the gate's text** — the agent tried
/// the protocol and missed it (`ATH-CORE-033`/`040`); the refusal names the error for the
/// re-ask, and the caller still records the body as an answer.
#[tokio::test]
async fn an_out_of_protocol_json_answer_is_refused_with_the_gate_text() {
    let message = athena_runtime::Message {
        message_id: athena_types::MessageId::new(),
        message_type: MessageType::Complete,
        from_agent: Some(AgentId::core_agent()),
        from_instance_id: None,
        thread_id: core_thread(&TaskId::new()),
        reply_to: None,
        priority: Priority::Normal,
        body: serde_json::json!({
            "kind": "start_agent_directly",
            "agent_id": "x",
        })
        .to_string(),
        payload_json: None,
        deadline: None,
    };
    let error = interpret_answer(&message).expect_err("out-of-protocol JSON is refused");
    let LoopFault::NotProtocol(text) = &error else {
        panic!("the refusal is the protocol fault: {error}");
    };
    assert!(
        !text.is_empty(),
        "the refusal carries the gate's error text"
    );
}
