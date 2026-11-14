//! The execution loop's driving-face tests (X-26, `R-IMPL-03`'s observable face).
//!
//! Every row drives the loop's *procedure* over the real store and the deterministic stub
//! Runtime — no Adapter, no `agent-cli` process, no LLM (`ATH-TEST-022`), so CI stays
//! deterministic. What they prove is exactly the gap the survey named: a Task created into
//! `QUEUED` is picked up, dispatched to instance #0, and carried to a **terminal state**
//! with its Result recorded and its Events replayed in order.

use crate::core_loop::{
    Interpreted, LoopFault, Settled, core_thread, fail_task, ingest_answer, interpret_answer,
    next_answer, open_clarification, prepare_delivery, queued_tasks, reported_status,
    running_tasks, settle_task,
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

/// X-66: the Intake's bound answer. A test that expects this delegation's own reply says so
/// here, so a `Foreign` reading fails loudly instead of flowing on as if it were the answer.
fn matched(intake: crate::core_loop::AnswerIntake) -> athena_runtime::Message {
    match intake {
        crate::core_loop::AnswerIntake::Matched(message) => message,
        other => panic!("expected this delegation's own reply, got {other:?}"),
    }
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
    let answer = matched(
        next_answer(&runtime, &handle, &pending.thread_id)
            .await
            .expect("the stub answers"),
    );
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
        &received.result_id,
        // X-70: these rows walk the *self-declared* settlement, which stays available when
        // the operator does not require a Verification. The required case is its own test.
        false,
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
    let result = next_answer(&runtime, &handle, "core:nothing-was-sent").await;
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

// --- X-53: the subordinate reception face ---------------------------------------------------
//
// The inbound leg's own tests. Whatever a delegated subordinate Instance emits through the
// Runtime boundary must land here: persisted as a Broker row, accepted, and only then given
// the kind's own face — `report`'s observation derivation, `complete`'s collected Result,
// `error`'s bare observation (`ATH-MSG-012`'s never-dropped trio). The stub Runtime carries
// the face exactly as every other core-loop test does (ATH-TEST-022); the `ReplayRuntime`
// double below carries the *redelivery* only the Broker's dedup can answer (ATH-MSG-014).
//
// This section adds the imports its tests need (the file's own stand at the top).
use crate::core_loop::{
    OutstandingReply, Received2, ReceptionFault, expire_outstanding_replies,
    receive_subordinate_messages, record_relay_observations,
};
use athena_persistence::repository::{AgentRepository, MessageRepository};
use athena_runtime::StubReaction;
use athena_runtime::StubReply;
use athena_types::{AgentInstance, AgentInstanceState, MessageId};
use std::collections::HashMap;
use std::path::Path;

/// The reception tests' fixed instant (the row's `created_at` on everything they land).
const X53_NOW: &str = "2026-11-13T00:00:00Z";

/// The worker a delegating Task was split to — the Agent facts the anchors read.
fn x53_worker_definition() -> AgentDefinition {
    AgentDefinition {
        id: AgentId::new("x53-worker"),
        name: "X53 Worker".into(),
        level: AgentLevel::Worker,
        description: "the delegated worker the inbound leg reads".into(),
        capabilities: vec![],
        persona_ref: "worker.md".into(),
        persona_version: "1.0.0".into(),
        allowed_tools: vec![ToolName::FsRead],
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

fn x53_subordinate_row(instance: &InstanceId, task: &TaskId) -> AgentInstance {
    AgentInstance {
        id: instance.clone(),
        agent_id: AgentId::new("x53-worker"),
        task_id: Some(task.clone()),
        plan_step_id: None,
        superior_instance_id: None,
        delegation_depth: 1,
        state: AgentInstanceState::Idle,
        runtime_instance_id: None,
        workspace: "/tmp/athena-x53-reception".into(),
        skill_set: vec![],
        aiprj_log_ref: None,
        attempt: 1,
        started_at: Some(X53_NOW.into()),
        finished_at: None,
    }
}

fn x53_seed_subordinate(store: &mut AthenaStore, instance: &InstanceId, task: &TaskId) {
    AgentRepository::upsert_definition(store, &x53_worker_definition())
        .expect("the worker's definition");
    AgentRepository::insert_instance(store, &x53_subordinate_row(instance, task))
        .expect("the subordinate's row");
}

/// A script whose default reaction answers every message with one reply of the given kind.
fn x53_script_that_replies(kind: MessageType, body: &str) -> StubScript {
    StubScript {
        reactions: vec![],
        default: StubReaction {
            replies: vec![StubReply {
                message_type: kind,
                body: body.into(),
                delay_ms: 0,
            }],
            result: None,
            status_after: None,
        },
    }
}

/// The delegation instruction the resident sent (the reply's thread is the Task's own).
fn x53_trigger(task: &TaskId) -> athena_runtime::Message {
    athena_runtime::Message {
        message_id: MessageId::new(),
        message_type: MessageType::Instruction,
        from_agent: Some(AgentId::core_agent()),
        from_instance_id: None,
        thread_id: core_thread(task),
        reply_to: None,
        priority: Priority::Normal,
        body: "make the thing".into(),
        payload_json: None,
        deadline: None,
    }
}

/// One reception poll, with the facts the resident holds fixed across the tests.
async fn x53_receive<R: Runtime + ?Sized>(
    runtime: &R,
    store: &std::sync::Mutex<AthenaStore>,
    subordinate: &RuntimeInstanceRef,
    superior: &InstanceId,
    subordinate_outputs: &[(String, String)],
    task: &TaskId,
) -> Result<Vec<Received2>, ReceptionFault> {
    receive_subordinate_messages(
        runtime,
        store,
        subordinate,
        superior,
        // No artifact ever rides a stub result, so the copy face never touches this path.
        Path::new("/tmp/athena-x53-reception"),
        Path::new("/tmp/athena-x53-reception"),
        Some(task),
        subordinate_outputs,
        X53_NOW,
    )
    .await
}

/// A Runtime double that replays the same messages on **every** poll — the redelivery the
/// stub cannot answer (its outbox drains once) and only the Broker's recorded delivery
/// may (`ATH-MSG-014`).
struct ReplayRuntime {
    messages: std::sync::Mutex<Vec<athena_runtime::Message>>,
}

impl ReplayRuntime {
    fn replaying(messages: Vec<athena_runtime::Message>) -> Self {
        Self {
            messages: std::sync::Mutex::new(messages),
        }
    }
}

#[async_trait::async_trait]
impl Runtime for ReplayRuntime {
    fn runtime_type(&self) -> RuntimeType {
        RuntimeType::AgentCli
    }

    async fn start(
        &self,
        _definition: AgentDefinition,
        _context: StartContext,
    ) -> athena_runtime::Result<RuntimeInstanceRef> {
        Err(athena_runtime::RuntimeError::UnknownInstance(
            "the replay double never starts".into(),
        ))
    }

    async fn send(
        &self,
        _instance: &RuntimeInstanceRef,
        _message: athena_runtime::Message,
    ) -> athena_runtime::Result<()> {
        Ok(())
    }

    fn receive(
        &self,
        _instance: &RuntimeInstanceRef,
    ) -> futures::stream::BoxStream<'static, athena_runtime::Message> {
        Box::pin(futures::stream::iter(
            self.messages.lock().expect("replay poisoned").clone(),
        ))
    }

    async fn status(
        &self,
        _instance: &RuntimeInstanceRef,
    ) -> athena_runtime::Result<athena_runtime::RuntimeInstanceStatus> {
        Ok(athena_runtime::RuntimeInstanceStatus::Running)
    }

    async fn stop(
        &self,
        _instance: &RuntimeInstanceRef,
        _reason: athena_types::StopReason,
    ) -> athena_runtime::Result<()> {
        Ok(())
    }

    async fn collect_result(
        &self,
        _instance: &RuntimeInstanceRef,
    ) -> athena_runtime::Result<athena_types::ExecutionResult> {
        Err(athena_runtime::RuntimeError::UnknownInstance(
            "the replay double never completes".into(),
        ))
    }

    fn capabilities(&self) -> athena_runtime::RuntimeCapabilities {
        athena_runtime::RuntimeCapabilities {
            runtime_type: self.runtime_type(),
            isolation: athena_runtime::IsolationBoundary::Process,
            supported_tools: vec![],
            max_concurrent_instances: 4,
        }
    }
}

/// **An empty inbox is the ordinary in-flight state** — the poll over a subordinate who
/// has emitted nothing is `Ok`, not a fault, and nothing was recorded for nothing.
#[tokio::test]
async fn an_empty_subordinate_inbox_is_not_a_fault() {
    let store = std::sync::Mutex::new(new_store());
    let runtime = StubRuntime::scripted(StubScript::default());
    let handle = boot(&runtime).await;
    let received = x53_receive(
        &runtime,
        &store,
        &handle,
        &InstanceId::new(),
        &[],
        &TaskId::new(),
    )
    .await
    .expect("an empty inbox polls clean");
    assert!(received.is_empty(), "nothing emitted, nothing received");
    assert_eq!(
        EventRepository::max_sequence(&*store.lock().expect("locked")).expect("seq"),
        0,
        "no Event was written for the silence"
    );
}

/// **A subordinate's `status` is Runtime chatter, never a Broker message** — only the
/// never-dropped trio carries reception records (`ATH-MSG-012`).
#[tokio::test]
async fn a_subordinates_status_is_runtime_chatter_not_a_broker_message() {
    let store = std::sync::Mutex::new(new_store());
    let runtime = StubRuntime::scripted(x53_script_that_replies(MessageType::Status, "accepted"));
    let handle = boot(&runtime).await;
    let trigger = x53_trigger(&TaskId::new());
    let thread = trigger.thread_id.clone();
    Runtime::send(&runtime, &handle, trigger)
        .await
        .expect("the stub takes the instruction");
    runtime.clock().advance(10);

    let received = x53_receive(
        &runtime,
        &store,
        &handle,
        &InstanceId::new(),
        &[],
        &TaskId::new(),
    )
    .await
    .expect("chatter still polls clean");
    assert!(
        received.is_empty(),
        "a status is the boundary's bookkeeping"
    );
    assert!(
        MessageRepository::list_for_thread(&*store.lock().expect("locked"), &thread)
            .expect("rows")
            .is_empty(),
        "no Broker row rides the thread"
    );
    assert_eq!(
        EventRepository::max_sequence(&*store.lock().expect("locked")).expect("seq"),
        0,
        "nothing was recorded"
    );
}

/// **A subordinate's `report` lands its row and its acceptance, and decides nothing** —
/// the never-dropped kind's own face (`ATH-MSG-012`/`014`); `ATH-HIER-015`'s derivation
/// stays with the row, the caller records observations on its own walk.
#[tokio::test]
async fn a_subordinates_report_lands_its_row_and_acceptance_and_decides_nothing() {
    let store = std::sync::Mutex::new(new_store());
    let task = {
        let mut guard = store.lock().expect("locked");
        task_in(&mut guard, TaskState::Running)
    };
    let runtime = StubRuntime::scripted(x53_script_that_replies(
        MessageType::Report,
        "the report is written",
    ));
    let handle = boot(&runtime).await;
    let superior = InstanceId::new();
    x53_seed_subordinate(
        &mut store.lock().expect("locked"),
        &handle.instance_id,
        &task,
    );
    Runtime::send(&runtime, &handle, x53_trigger(&task))
        .await
        .expect("the stub takes the instruction");
    runtime.clock().advance(10);

    let received = x53_receive(&runtime, &store, &handle, &superior, &[], &task)
        .await
        .expect("the report lands");
    let Received2::Report {
        message,
        relay_observations,
    } = received.into_iter().next().expect("the report rode in")
    else {
        panic!("a report is the reception's product");
    };
    assert!(
        relay_observations.is_empty(),
        "no subordinate outputs were supplied, so no verbatim relay derives"
    );
    let rows =
        MessageRepository::list_for_thread(&*store.lock().expect("locked"), &message.thread_id)
            .expect("rows");
    assert_eq!(rows.len(), 1, "exactly one Broker row");
    let row = &rows[0];
    assert_eq!(row.message_type, MessageType::Report);
    assert_eq!(
        row.from_instance_id, handle.instance_id,
        "the Instance is the correspondent (ATH-MSG-011)"
    );
    assert_eq!(row.to_agent, AgentId::core_agent());
    assert_eq!(row.to_instance_id, Some(superior.clone()));
    assert_eq!(row.task_id, Some(task.clone()));
    assert!(
        row.deadline.is_none(),
        "the inbound kinds carry no reply deadline (ATH-MSG-005)"
    );
    assert!(
        MessageRepository::was_delivered(
            &*store.lock().expect("locked"),
            &superior,
            &message.message_id
        )
        .expect("delivery"),
        "the ack contract's pair is on the record (ATH-MSG-012)"
    );
    let events =
        EventRepository::list_for_task(&*store.lock().expect("locked"), &task).expect("events");
    let acceptance = events
        .iter()
        .find(|event| event.event_type == "AgentMessageReceived")
        .expect("the acceptance rode the record");
    assert_eq!(
        acceptance.instance_id,
        Some(handle.instance_id.clone()),
        "the sender is named"
    );
    assert_eq!(
        EventRepository::max_sequence(&*store.lock().expect("locked")).expect("seq"),
        2,
        "the Task's creation + the acceptance, nothing more"
    );
}

/// **The resident records the verbatim relay its worker's report rode** — the observation
/// derived at reception is recorded on the caller's own walk, and an empty set records
/// nothing (`ATH-HIER-015`'s observation face; `ATH-CORE-022`'s deciding-agent discipline).
#[tokio::test]
async fn the_resident_records_the_verbatim_relay_his_workers_report_rode() {
    let store = std::sync::Mutex::new(new_store());
    let task = {
        let mut guard = store.lock().expect("locked");
        task_in(&mut guard, TaskState::Running)
    };
    let runtime = StubRuntime::scripted(x53_script_that_replies(
        MessageType::Report,
        "the worker shipped the body verbatim",
    ));
    let handle = boot(&runtime).await;
    let superior = InstanceId::new();
    x53_seed_subordinate(
        &mut store.lock().expect("locked"),
        &handle.instance_id,
        &task,
    );
    Runtime::send(&runtime, &handle, x53_trigger(&task))
        .await
        .expect("the stub takes the instruction");
    runtime.clock().advance(10);

    let outputs = vec![(
        "x53-worker".to_string(),
        "the worker shipped the body verbatim".to_string(),
    )];
    let received = x53_receive(&runtime, &store, &handle, &superior, &outputs, &task)
        .await
        .expect("the report lands");
    let Received2::Report {
        message,
        relay_observations,
    } = received.into_iter().next().expect("the report rode in")
    else {
        panic!("a report is the reception's product");
    };
    assert_eq!(
        relay_observations.len(),
        1,
        "the body rode the worker's output verbatim (ATH-HIER-015)"
    );
    assert_eq!(relay_observations[0].subordinate, "x53-worker");

    {
        let mut guard = store.lock().expect("locked");
        record_relay_observations(
            &mut guard,
            &relay_observations,
            &message,
            Some(&task),
            &superior,
            X53_NOW,
        )
        .expect("the observation is recorded");
    }
    let events =
        EventRepository::list_for_task(&*store.lock().expect("locked"), &task).expect("events");
    let observation = events
        .iter()
        .find(|event| event.event_type == "EventAppended")
        .expect("one raw Event per observation");
    let payload: serde_json::Value =
        serde_json::from_str(&observation.payload_json).expect("the payload parses");
    assert_eq!(payload["observation"]["code"], "AGGREGATION_VERBATIM_RELAY");
    assert_eq!(payload["observation"]["policy"], "hier.aggregation");
    assert_eq!(payload["observation"]["subordinate"], "x53-worker");
    assert_eq!(observation.task_id, Some(task.clone()));
    assert_eq!(observation.instance_id, Some(superior.clone()));

    {
        let mut guard = store.lock().expect("locked");
        record_relay_observations(&mut guard, &[], &message, Some(&task), &superior, X53_NOW)
            .expect("an empty set records nothing");
    }
    let still = EventRepository::list_for_task(&*store.lock().expect("locked"), &task)
        .expect("events")
        .iter()
        .filter(|event| event.event_type == "EventAppended")
        .count();
    assert_eq!(still, 1, "an empty observation set is an honest nothing");
}

/// **A subordinate's `complete` collects the Result and moves the Task to EVALUATING** —
/// the Result face: collect, copy, gate, then row + carry in one transaction
/// (`ATH-PERS-003`, `ATH-CORE-041`, `ATH-EVAL-005`).
#[tokio::test]
async fn a_subordinates_complete_collects_the_result_and_moves_the_task_to_evaluating() {
    let store = std::sync::Mutex::new(new_store());
    let task = {
        let mut guard = store.lock().expect("locked");
        task_in(&mut guard, TaskState::Running)
    };
    let runtime = StubRuntime::scripted(StubScript::completes_any_instruction("done"));
    let superior = InstanceId::new();
    // The subordinate boots with the Task bound — collect_result attributes its result.
    let mut sub_context = context();
    sub_context.task_id = Some(task.clone());
    let handle = Runtime::start(&runtime, definition(), sub_context)
        .await
        .expect("the subordinate boots");
    x53_seed_subordinate(
        &mut store.lock().expect("locked"),
        &handle.instance_id,
        &task,
    );
    Runtime::send(&runtime, &handle, x53_trigger(&task))
        .await
        .expect("the stub takes the instruction");
    runtime.clock().advance(50);

    let received = x53_receive(&runtime, &store, &handle, &superior, &[], &task)
        .await
        .expect("the poll walks the boundary");
    let mut reports = 0usize;
    let mut complete = None;
    for outcome in received {
        match outcome {
            Received2::Report { .. } => reports += 1,
            Received2::Complete { .. } => complete = Some(outcome),
            Received2::Error { .. } => {}
        }
    }
    assert_eq!(
        reports, 1,
        "the worker's report rode in alongside the completion"
    );
    let Some(Received2::Complete {
        task_id,
        result_id,
        artifacts_copied,
    }) = complete
    else {
        panic!("the completion is the face");
    };
    assert_eq!(task_id, task);
    assert_eq!(
        artifacts_copied, 0,
        "the stub attaches no artifacts, so the copy walks nothing (ATH-RESULT-004)"
    );
    let results =
        ResultRepository::list_for_task(&*store.lock().expect("locked"), &task).expect("results");
    assert_eq!(results.len(), 1, "exactly one Result row");
    assert_eq!(
        results[0].id, result_id,
        "the id is re-minted Core-side (ATH-CORE-041)"
    );
    assert_eq!(
        results[0].instance_id, handle.instance_id,
        "the reporting Instance is the subordinate's"
    );
    assert_eq!(
        results[0].metadata.get("self_declared"),
        Some(&serde_json::json!(true)),
        "the self-declaration rides the Result (ATH-EVAL-005)"
    );
    let row = TaskRepository::get(&*store.lock().expect("locked"), &task)
        .expect("read")
        .expect("row");
    assert_eq!(
        row.state,
        TaskState::Evaluating,
        "the arrival carried its edge"
    );
    let events =
        EventRepository::list_for_task(&*store.lock().expect("locked"), &task).expect("events");
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "TaskResultReceived"),
        "the carry rides the transition (ATH-PERS-003): {events:?}"
    );
}

/// **A `complete` arriving at a closed window refuses whole, with its own persist/accept
/// faces already landed** — the gate stands before any Result persists; nothing is
/// half-recorded.
#[tokio::test]
async fn a_complete_at_a_non_running_task_refuses_without_a_result_row() {
    let store = std::sync::Mutex::new(new_store());
    let task = {
        let mut guard = store.lock().expect("locked");
        task_in(&mut guard, TaskState::Queued)
    };
    let runtime = StubRuntime::scripted(StubScript::completes_any_instruction("refused"));
    let superior = InstanceId::new();
    let mut sub_context = context();
    sub_context.task_id = Some(task.clone());
    let handle = Runtime::start(&runtime, definition(), sub_context)
        .await
        .expect("the subordinate boots");
    x53_seed_subordinate(
        &mut store.lock().expect("locked"),
        &handle.instance_id,
        &task,
    );
    Runtime::send(&runtime, &handle, x53_trigger(&task))
        .await
        .expect("the stub takes the instruction");
    runtime.clock().advance(50);

    let error = x53_receive(&runtime, &store, &handle, &superior, &[], &task)
        .await
        .expect_err("the closed window refuses the arrival");
    let ReceptionFault::NotRunning {
        task: named,
        actual,
    } = &error
    else {
        panic!("the state gate is the refusal: {error:?}");
    };
    assert_eq!(named, task.as_str(), "the gate names the Task");
    assert_eq!(actual, "QUEUED", "the row's own state names the gate");
    assert!(
        ResultRepository::list_for_task(&*store.lock().expect("locked"), &task)
            .expect("results")
            .is_empty(),
        "nothing persisted to the closed window"
    );
    let row = TaskRepository::get(&*store.lock().expect("locked"), &task)
        .expect("read")
        .expect("row");
    assert_eq!(
        row.state,
        TaskState::Queued,
        "no edge rode a refused arrival"
    );
    let events =
        EventRepository::list_for_task(&*store.lock().expect("locked"), &task).expect("events");
    assert!(
        !events
            .iter()
            .any(|event| event.event_type == "TaskResultReceived"),
        "no carry was written"
    );
}

/// **A subordinate's `error` lands its row and stands as an observation** — the
/// never-dropped kind is received and recorded; the consequence is the deciding agent's.
#[tokio::test]
async fn a_subordinates_error_lands_its_row_and_stands_as_an_observation() {
    let store = std::sync::Mutex::new(new_store());
    let task = {
        let mut guard = store.lock().expect("locked");
        task_in(&mut guard, TaskState::Running)
    };
    let runtime = StubRuntime::scripted(x53_script_that_replies(
        MessageType::Error,
        "the step failed",
    ));
    let handle = boot(&runtime).await;
    let superior = InstanceId::new();
    x53_seed_subordinate(
        &mut store.lock().expect("locked"),
        &handle.instance_id,
        &task,
    );
    Runtime::send(&runtime, &handle, x53_trigger(&task))
        .await
        .expect("the stub takes the instruction");
    runtime.clock().advance(10);

    let received = x53_receive(&runtime, &store, &handle, &superior, &[], &task)
        .await
        .expect("the error lands");
    let Received2::Error { message } = received.into_iter().next().expect("the error rode in")
    else {
        panic!("an error is the reception's product");
    };
    let rows =
        MessageRepository::list_for_thread(&*store.lock().expect("locked"), &message.thread_id)
            .expect("rows");
    assert_eq!(rows.len(), 1, "exactly one Broker row");
    assert_eq!(rows[0].message_type, MessageType::Error);
    assert_eq!(rows[0].from_instance_id, handle.instance_id);
    assert!(
        rows[0].deadline.is_none(),
        "inbound kinds carry no deadline (ATH-MSG-005)"
    );
    assert_eq!(
        EventRepository::max_sequence(&*store.lock().expect("locked")).expect("seq"),
        2,
        "the Task's creation + the acceptance"
    );
    let row = TaskRepository::get(&*store.lock().expect("locked"), &task)
        .expect("read")
        .expect("row");
    assert_eq!(
        row.state,
        TaskState::Running,
        "an error's reception never settles the Task itself"
    );
}

/// **A redelivered subordinate message is answered by the recorded delivery** — every poll
/// replays the boundary's messages on a `Runtime` at-least-once, and the Broker's dedup
/// (`ATH-MSG-014`) is the answer: one row, one acceptance, no Event said twice.
#[tokio::test]
async fn a_redelivered_subordinate_message_is_answered_by_the_recorded_delivery() {
    let store = std::sync::Mutex::new(new_store());
    let task = {
        let mut guard = store.lock().expect("locked");
        task_in(&mut guard, TaskState::Running)
    };
    let superior = InstanceId::new();
    let subordinate = RuntimeInstanceRef {
        runtime_type: RuntimeType::AgentCli,
        runtime_instance_id: athena_types::RuntimeInstanceId::new(),
        instance_id: InstanceId::new(),
    };
    x53_seed_subordinate(
        &mut store.lock().expect("locked"),
        &subordinate.instance_id,
        &task,
    );
    let message = athena_runtime::Message {
        message_id: MessageId::new(),
        message_type: MessageType::Report,
        from_agent: Some(AgentId::new("x53-worker")),
        from_instance_id: Some(subordinate.instance_id.clone()),
        thread_id: core_thread(&task),
        reply_to: None,
        priority: Priority::Normal,
        body: "the report is written".into(),
        payload_json: None,
        deadline: None,
    };
    let runtime = ReplayRuntime::replaying(vec![message]);

    let first = x53_receive(&runtime, &store, &subordinate, &superior, &[], &task)
        .await
        .expect("the first copy lands");
    assert!(
        matches!(first.as_slice(), [Received2::Report { .. }]),
        "the report landed: {first:?}"
    );
    assert_eq!(
        EventRepository::max_sequence(&*store.lock().expect("locked")).expect("seq"),
        2,
        "the Task's creation + the acceptance"
    );

    let second = x53_receive(&runtime, &store, &subordinate, &superior, &[], &task)
        .await
        .expect("the redelivery polls clean");
    assert!(
        second.is_empty(),
        "the dedup answers the redelivery: nothing lands twice"
    );
    assert_eq!(
        EventRepository::max_sequence(&*store.lock().expect("locked")).expect("seq"),
        2,
        "no Event was said twice (ATH-MSG-014)"
    );
    assert_eq!(
        MessageRepository::list_for_thread(&*store.lock().expect("locked"), &core_thread(&task))
            .expect("rows")
            .len(),
        1,
        "one row, written once"
    );
}

/// **A message from an unrecorded Instance is refused, not landed** — the anchor is a fact
/// the rows hold (`ATH-DATA-011`); without it the walk refuses whole and records nothing.
#[tokio::test]
async fn a_message_from_an_unrecorded_instance_is_refused_unanchored() {
    let store = std::sync::Mutex::new(new_store());
    let subordinate = RuntimeInstanceRef {
        runtime_type: RuntimeType::AgentCli,
        runtime_instance_id: athena_types::RuntimeInstanceId::new(),
        instance_id: InstanceId::new(),
    };
    let message = athena_runtime::Message {
        message_id: MessageId::new(),
        message_type: MessageType::Report,
        from_agent: Some(AgentId::new("x53-worker")),
        from_instance_id: Some(subordinate.instance_id.clone()),
        thread_id: "thr-unanchored".into(),
        reply_to: None,
        priority: Priority::Normal,
        body: "the report".into(),
        payload_json: None,
        deadline: None,
    };
    let runtime = ReplayRuntime::replaying(vec![message]);
    let error = x53_receive(
        &runtime,
        &store,
        &subordinate,
        &InstanceId::new(),
        &[],
        &TaskId::new(),
    )
    .await
    .expect_err("the anchor is a fact, never a guess");
    let ReceptionFault::Unanchored(named) = &error else {
        panic!("the anchor fault is the refusal: {error:?}");
    };
    assert_eq!(
        named,
        subordinate.instance_id.as_str(),
        "the refusal names the unanchored Instance"
    );
    assert_eq!(
        EventRepository::max_sequence(&*store.lock().expect("locked")).expect("seq"),
        0,
        "nothing was recorded"
    );
}

/// **The deadline sweep fires its timeouts and keeps the rows in peace** — the wait
/// records the resident holds are answered with `AgentMessageTimeout` Events (the
/// machinery's first production walker), an observation that never branches a state
/// (`ATH-STATE-023`); the dropped entry stops watching.
#[tokio::test]
async fn the_deadline_sweep_fires_its_timeouts_and_keeps_the_rows_in_peace() {
    let mut store = new_store();
    let task = task_in(&mut store, TaskState::Running);
    let deadline_passed = "2026-11-12T00:00:00Z";
    let deadline_live = "2026-11-30T00:00:00Z";
    let key_expired = MessageId::new().as_str().to_string();
    let key_live = MessageId::new().as_str().to_string();
    let mut outstanding = HashMap::new();
    outstanding.insert(
        key_expired.clone(),
        OutstandingReply {
            message_id: key_expired.clone(),
            thread_id: core_thread(&task),
            deadline: deadline_passed.into(),
            task_id: Some(task.clone()),
            receiver: AgentId::new("x53-worker"),
        },
    );
    outstanding.insert(
        key_live.clone(),
        OutstandingReply {
            message_id: key_live.clone(),
            thread_id: core_thread(&task),
            deadline: deadline_live.into(),
            task_id: Some(task.clone()),
            receiver: AgentId::new("x53-worker"),
        },
    );

    let expired = expire_outstanding_replies(&mut store, &outstanding, "2026-11-13T00:00:00Z")
        .expect("the sweep observes");
    assert_eq!(
        expired,
        vec![key_expired.clone()],
        "only the deadline that passed fires"
    );

    let events = EventRepository::list_for_task(&store, &task).expect("events");
    let timeout = events
        .iter()
        .find(|event| event.event_type == "AgentMessageTimeout")
        .expect("the timeout is on the record");
    for needle in [
        key_expired.as_str(),
        core_thread(&task).as_str(),
        deadline_passed,
    ] {
        assert!(
            timeout.payload_json.contains(needle),
            "the payload names {needle}"
        );
    }
    assert_eq!(
        events.len(),
        2,
        "creation + one timeout — the live wait fired nothing"
    );
    assert_eq!(EventRepository::max_sequence(&store).expect("seq"), 2);

    let row = TaskRepository::get(&store, &task)
        .expect("read")
        .expect("row");
    assert_eq!(
        row.state,
        TaskState::Running,
        "the sweep observes, it never fails the row (ATH-STATE-023)"
    );

    let mut only_live = HashMap::new();
    only_live.insert(
        key_live.clone(),
        outstanding.get(&key_live).cloned().expect("the live entry"),
    );
    let again = expire_outstanding_replies(&mut store, &only_live, "2026-11-13T00:00:00Z")
        .expect("the sweep observes again");
    assert!(again.is_empty(), "the live deadline holds");
    assert_eq!(
        EventRepository::max_sequence(&store).expect("seq"),
        2,
        "no Event fired twice"
    );
}

/// **The settlement's terminal edge carries the catalog's completion face** (X-54): on the
/// Success foot the §19.2 step-10/13 `AgentCompleted` event is appended beside the
/// settlement's own `TaskCompleted` (payload `empty()` per §23.2, actor Core Agent,
/// caused by the settlement edge), so the replay tells the settlement from the completion.
#[tokio::test]
async fn a_succeeded_settlement_completes_the_catalogs_agent_completed() {
    let mut store = new_store();
    let task = task_in(&mut store, TaskState::Queued);
    let runtime = StubRuntime::scripted(StubScript::completes_any_instruction("done"));
    let handle = boot(&runtime).await;
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
    runtime.clock().advance(50);
    let answer = matched(
        next_answer(&runtime, &handle, &pending.thread_id)
            .await
            .expect("the stub answers"),
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
    let outcome = settle_task(
        &mut store,
        &task,
        &received.result_id,
        // X-70: these rows walk the *self-declared* settlement, which stays available when
        // the operator does not require a Verification. The required case is its own test.
        false,
        "2026-11-08T00:00:02Z",
        "cor-4",
    )
    .expect("settled");
    assert_eq!(outcome, Settled::Succeeded);

    // The terminal leg replays: TaskCompleted (the settlement's verdict) and — X-54 —
    // AgentCompleted (the §19.2 completion vocabulary).
    let events = EventRepository::list_for_task(&store, &task).expect("events");
    let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
    assert!(
        types.contains(&"TaskCompleted"),
        "the settlement's verdict rides: {types:?}"
    );
    assert!(
        types.contains(&"AgentCompleted"),
        "X-54's completion face is on the Success foot: {types:?}"
    );
    let completed = events
        .iter()
        .find(|e| e.event_type == "AgentCompleted")
        .expect("the completion row");
    assert_eq!(
        completed.actor, "core-agent",
        "the Core Agent completed the delegated unit"
    );
    assert_eq!(
        completed.payload_json, "{}",
        "the catalog's empty() payload rides verbatim"
    );
    assert_eq!(
        completed.correlation_id, "cor-4",
        "the settlement's own correlation carries the completion"
    );
}

/// **The failure foot stays `TaskFailed`-only** (X-54): a budget-exhausted settlement
/// never fabricates an `AgentCompleted` — the catalog's completion is the *successful*
/// completion; the failure's terminal fact is its own `TaskFailed` row.
#[tokio::test]
async fn a_failed_settlement_does_not_fabricate_a_completion() {
    let mut store = new_store();
    let task = task_in(&mut store, TaskState::Evaluating);
    fail_task(
        &mut store,
        &task,
        "BudgetExhausted",
        "2026-11-08T00:00:03Z",
        "cor-fail",
    )
    .expect("failed");
    let row = TaskRepository::get(&store, &task)
        .expect("read")
        .expect("row");
    assert_eq!(row.state, TaskState::Failed);
    let events = EventRepository::list_for_task(&store, &task).expect("events");
    let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
    assert!(
        types.contains(&"TaskFailed"),
        "the failure's terminal fact rides: {types:?}"
    );
    assert!(
        !types.contains(&"AgentCompleted"),
        "no fabricated completion on the failure foot: {types:?}"
    );
}

/// **The lease is released when the Task settles** (X-54, `ATH-DATA-009`): the terminal
/// edge ends the Task whose lifetime the lease rode, so the lease row is removed and the
/// catalog's `WorkspaceReleased` event rides the settlement's own correlation — and a
/// later acquire on the same workspace succeeds again.
#[tokio::test]
async fn a_settled_task_releases_its_workspace_lease() {
    let mut store = new_store();
    let task = task_in(&mut store, TaskState::Queued);
    let runtime = StubRuntime::scripted(StubScript::completes_any_instruction("done"));
    let handle = boot(&runtime).await;
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
    runtime.clock().advance(50);
    let answer = matched(
        next_answer(&runtime, &handle, &pending.thread_id)
            .await
            .expect("the stub answers"),
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
    settle_task(
        &mut store,
        &task,
        &received.result_id,
        // X-70: these rows walk the *self-declared* settlement, which stays available when
        // the operator does not require a Verification. The required case is its own test.
        false,
        "2026-11-08T00:00:02Z",
        "cor-4",
    )
    .expect("settled");

    // The lease row gone at the terminal edge: read the Task's own workspace off its row.
    let row = TaskRepository::get(&store, &task)
        .expect("read")
        .expect("row");
    let holder = athena_persistence::WorkspaceLeaseRepository::holder(&store, &row.workspace)
        .expect("holder read");
    assert!(
        holder.is_none(),
        "the release face removed the lease row at the terminal edge"
    );
}

// --- X-55: the evaluation faces ------------------------------------------------------------

/// A settled answer intake opens its evaluation with the catalog's `EvaluationStarted`
/// (X-55, `ATH-TEST-008`): the `RUNNING -> EVALUATING` edge the intake walks records the
/// walk, the payload the catalog's `empty()`, the cause the intake's own carry row.
#[tokio::test]
async fn an_answer_intake_opens_the_evaluation_face() {
    let mut store = new_store();
    let task = task_in(&mut store, TaskState::Queued);
    let runtime = StubRuntime::scripted(StubScript::completes_any_instruction("done"));
    let handle = boot(&runtime).await;

    let pending = prepare_delivery(
        &mut store,
        &task,
        &handle.instance_id,
        "2026-11-13T15:00:00Z",
        "cor-2",
    )
    .expect("reserved");
    crate::core_loop::deliver(&runtime, &handle, &pending)
        .await
        .expect("delivered");
    runtime.clock().advance(50);
    let answer = matched(
        next_answer(&runtime, &handle, &pending.thread_id)
            .await
            .expect("the stub answers"),
    );
    ingest_answer(
        &mut store,
        &task,
        &handle,
        &answer,
        "2026-11-13T15:00:00Z",
        "cor-3",
    )
    .expect("answer ingested");

    let events = EventRepository::list_for_task(&store, &task).expect("events");
    let started = events
        .iter()
        .filter(|event| event.event_type == "EvaluationStarted")
        .count();
    assert_eq!(
        started, 1,
        "the intake's evaluation face is recorded exactly once: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "TaskResultReceived"),
        "the carry still rides the transition (X-55 adds, never replaces)"
    );
}

/// A subordinate's `complete` lands with the same evaluation face (X-55): the arrival's
/// edge is recorded, the actor is the System that walked the broker boundary, and the
/// payload stays the catalog's `empty()`.
#[tokio::test]
async fn a_subordinates_complete_opens_the_evaluation_face() {
    let store = std::sync::Mutex::new(new_store());
    let task = {
        let mut guard = store.lock().expect("locked");
        task_in(&mut guard, TaskState::Running)
    };
    let runtime = StubRuntime::scripted(StubScript::completes_any_instruction("done"));
    let superior = InstanceId::new();
    let mut sub_context = context();
    sub_context.task_id = Some(task.clone());
    let handle = Runtime::start(&runtime, definition(), sub_context)
        .await
        .expect("the subordinate boots");
    x53_seed_subordinate(
        &mut store.lock().expect("locked"),
        &handle.instance_id,
        &task,
    );
    Runtime::send(&runtime, &handle, x53_trigger(&task))
        .await
        .expect("the stub takes the instruction");
    runtime.clock().advance(50);

    let received = x53_receive(&runtime, &store, &handle, &superior, &[], &task)
        .await
        .expect("the poll walks the boundary");
    assert!(
        received
            .iter()
            .any(|outcome| matches!(outcome, Received2::Complete { .. })),
        "the completion arrived: {received:?}"
    );

    let events =
        EventRepository::list_for_task(&*store.lock().expect("locked"), &task).expect("events");
    let started = events
        .iter()
        .filter(|event| event.event_type == "EvaluationStarted")
        .count();
    assert_eq!(
        started, 1,
        "the arrival's evaluation face is recorded exactly once: {events:?}"
    );
}

// --- X-66 / D-62: the answer's identity is the delivery's identity ----------------------

/// One instruction on a named thread, so the stub answers on that thread.
fn instruction_on(thread: &str, body: &str) -> athena_runtime::Message {
    athena_runtime::Message {
        message_id: MessageId::new(),
        message_type: MessageType::Instruction,
        from_agent: Some(AgentId::core_agent()),
        from_instance_id: None,
        thread_id: thread.into(),
        reply_to: None,
        priority: athena_types::Priority::Normal,
        body: body.into(),
        payload_json: None,
        deadline: None,
    }
}

#[tokio::test]
async fn a_reply_on_another_delegations_thread_is_never_this_tasks_answer() {
    // X-66: the reception face is a shared outbox, so "the first `complete` on the stream"
    // is not "this delegation's answer". Session 076 measured the consequence — a Task
    // settled by a different Task's reply, recording a success it never earned. The Intake
    // binds on the thread `prepare_delivery` minted (`ATH-MSG-013`) before anything
    // interprets the body.
    let runtime = StubRuntime::scripted(StubScript::completes_any_instruction("done"));
    let handle = boot(&runtime).await;

    // Only a foreign delegation has answered.
    Runtime::send(
        &runtime,
        &handle,
        instruction_on("core:task-SOMEONE-ELSE", "someone else's work"),
    )
    .await
    .expect("delivered");
    runtime.clock().advance(50);

    let intake = next_answer(&runtime, &handle, "core:task-MINE")
        .await
        .expect("a complete is present, so this is not NoAnswerYet");

    match intake {
        crate::core_loop::AnswerIntake::Foreign { thread_id, .. } => assert_eq!(
            thread_id, "core:task-SOMEONE-ELSE",
            "the foreign reading names the thread it refused to bind"
        ),
        crate::core_loop::AnswerIntake::Matched(message) => panic!(
            "a reply on another thread must never be returned as this delegation's \
             answer: {message:?}"
        ),
    }
}

/// A Runtime whose reply stream is exactly the messages the test lists, in order.
///
/// The scripted stub ends an instance's liveness when it completes, so it cannot hold two
/// delegations' replies at once — and "a foreign reply does not shadow the real one" is a
/// property of the scan order, which this double states directly.
struct StreamOf(Vec<athena_runtime::Message>);

#[async_trait::async_trait]
impl Runtime for StreamOf {
    fn runtime_type(&self) -> athena_types::RuntimeType {
        athena_types::RuntimeType::AgentCli
    }

    async fn start(
        &self,
        _definition: athena_types::AgentDefinition,
        _ctx: StartContext,
    ) -> Result<RuntimeInstanceRef, athena_runtime::RuntimeError> {
        Err(athena_runtime::RuntimeError::Unsupported(
            "read-only double".into(),
        ))
    }

    async fn send(
        &self,
        _instance: &RuntimeInstanceRef,
        _message: athena_runtime::Message,
    ) -> Result<(), athena_runtime::RuntimeError> {
        Ok(())
    }

    fn receive(
        &self,
        _instance: &RuntimeInstanceRef,
    ) -> futures::stream::BoxStream<'static, athena_runtime::Message> {
        Box::pin(futures::stream::iter(self.0.clone()))
    }

    async fn status(
        &self,
        _instance: &RuntimeInstanceRef,
    ) -> Result<athena_runtime::status::RuntimeInstanceStatus, athena_runtime::RuntimeError> {
        Ok(athena_runtime::status::RuntimeInstanceStatus::Running)
    }

    async fn stop(
        &self,
        _instance: &RuntimeInstanceRef,
        _reason: athena_types::StopReason,
    ) -> Result<(), athena_runtime::RuntimeError> {
        Ok(())
    }

    async fn collect_result(
        &self,
        _instance: &RuntimeInstanceRef,
    ) -> Result<athena_types::ExecutionResult, athena_runtime::RuntimeError> {
        Err(athena_runtime::RuntimeError::UnknownInstance(
            "not collected".into(),
        ))
    }

    fn capabilities(&self) -> athena_runtime::status::RuntimeCapabilities {
        athena_runtime::status::RuntimeCapabilities {
            runtime_type: self.runtime_type(),
            isolation: athena_runtime::status::IsolationBoundary::Process,
            supported_tools: vec![],
            max_concurrent_instances: 1,
        }
    }
}

fn complete_on(thread: &str, body: &str) -> athena_runtime::Message {
    let mut message = instruction_on(thread, body);
    message.message_type = MessageType::Complete;
    message
}

#[tokio::test]
async fn this_delegations_own_reply_is_bound_even_when_a_foreign_one_precedes_it() {
    // The foreign reading must not shadow the real answer: one shared outbox holds both,
    // and the scan continues past what it cannot bind.
    let runtime = StreamOf(vec![
        complete_on("core:task-SOMEONE-ELSE", "someone else's product"),
        complete_on("core:task-MINE", "my product"),
    ]);
    let handle = RuntimeInstanceRef {
        runtime_type: athena_types::RuntimeType::AgentCli,
        runtime_instance_id: athena_types::RuntimeInstanceId::new(),
        instance_id: InstanceId::new(),
    };

    let message = matched(
        next_answer(&runtime, &handle, "core:task-MINE")
            .await
            .expect("the stream holds this delegation's answer"),
    );
    assert_eq!(
        message.thread_id, "core:task-MINE",
        "the bound answer is this delegation's own"
    );
    assert_eq!(message.body, "my product");
}

#[tokio::test]
async fn a_stream_with_no_matching_thread_is_reported_foreign_not_empty() {
    // The distinction matters to the caller: `NoAnswerYet` means "still in flight", while a
    // foreign reading means "something answered, but not this delegation" — a fact worth a
    // log line rather than a silent skip.
    let runtime = StreamOf(vec![complete_on("core:task-OTHER", "{}")]);
    let handle = RuntimeInstanceRef {
        runtime_type: athena_types::RuntimeType::AgentCli,
        runtime_instance_id: athena_types::RuntimeInstanceId::new(),
        instance_id: InstanceId::new(),
    };

    assert!(matches!(
        next_answer(&runtime, &handle, "core:task-MINE").await,
        Ok(crate::core_loop::AnswerIntake::Foreign { .. })
    ));
    // And an empty stream is still the in-flight state.
    let empty = StreamOf(vec![]);
    assert!(matches!(
        next_answer(&empty, &handle, "core:task-MINE").await,
        Err(crate::core_loop::LoopFault::NoAnswerYet)
    ));
}
