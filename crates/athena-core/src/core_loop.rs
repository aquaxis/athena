//! The Core Agent execution loop's driving entry point (X-26; spec §7.5, design §4.1).
//!
//! The parts of the loop existed across this crate — [`crate::scheduler::tick`] decides
//! admissions, [`crate::state_manager`] owns the transitions, [`crate::protocol`] types the
//! Core Agent's answers, [`crate::issuance`] mints identifiers — but nothing drove them at
//! runtime: a Task created through the API reached `QUEUED` and stopped, because no code
//! ever addressed instance #0, and no code interpreted what it answered. This module is
//! that driver.
//!
//! **What the loop is.** Per `ATH-DATA-004`, the human's instruction *is* the delegation
//! body: in v1 a Task is executed by handing the Core Agent the instruction and reading
//! back its answer, which becomes the Task's Result. That is the execution model
//! `ATH-AIPRJ-011` maps onto `agent-cli`, and `athena-runtime-agent-cli` realizes it as one
//! `ask` round trip per `send`.
//!
//! **What the loop decides.** Nothing (`ATH-CORE-022`, `ATH-CORE-049`). Every function
//! here is a *procedure*: it delivers the instruction it is given, records the answer it
//! receives, and states what it observed. The planning, the delegation decisions and the
//! evaluation verdicts are the Core Agent's; where this module must produce a value on the
//! agent's behalf — the Result's `status` — it takes the one the agent reported and marks
//! it self-declared (`ATH-EVAL-005`).
//!
//! - **Delivery** ([`prepare_delivery`]): transition `QUEUED → RUNNING` with `TaskStarted`
//!   and build the `instruction` the Runtime must carry. The transition is the store's
//!   atomic transition+Event pair (`ATH-PERS-003`), so a Task never sits `RUNNING` without
//!   its Event.
//! - **Intake** ([`ingest_answer`]): persist the agent's answer as the Task's immutable
//!   Result (`ATH-RESULT-003`) and transition `RUNNING → EVALUATING` with
//!   `TaskResultReceived`. The Result carries the agent's self-declared status; acceptance
//!   is the Evaluation gate's, never this reader's.
//! - **Interpretation** ([`interpret_answer`]): read what the Core Agent *said* — the
//!   twelve `CoreRequest` kinds (`ATH-CORE-033`) through the JSON Schema gate
//!   (`ATH-CORE-039`), where `complete_task` names the settling product, `ask_human`
//!   hands the wait to the clarification path (`ATH-HITL-017..020`), and the other ten
//!   route to the caller as observed requests (`Interpreted::Requested`); bare prose is
//!   the ordinary v1 product, never a protocol fault.
//! - **Completion** ([`settle_task`]): transition `EVALUATING → SUCCESS` / `FAILED`.
//! - **Interpretation** ([`interpret_answer`]): route the answer against the Core Control
//!   Protocol (`ATH-CORE-033`'s twelve kinds, through the schema gate `ATH-CORE-039`).
//!   A `complete_task` request — or the v1 bare-prose product (`ATH-DATA-004`) — is the
//!   settlement's product; an `ask_human` request routes to the clarification path
//!   ([`open_clarification`], `ATH-HITL-017..020`, whose deadline-expiry fails the Task and
//!   whose human answer resumes it); any other of the twelve is an observed request the
//!   caller may answer through a later delivery, never a side path this driver runs on its
//!   own (`ATH-CORE-034`'s response face stays with the caller).
//!
//! **[design decision] Why the store half is synchronous and the send is not.** The
//! composition root holds the store behind a `std::sync::Mutex`, whose guard is not
//! `Send` and therefore cannot span an `await`. Splitting each step into a synchronous
//! store half and a lock-free network half is what lets the driver live in a
//! `tokio::spawn` task without changing the store's ownership model — and it has the
//! useful side effect of never holding the store across a slow agent round trip.
//!
//! **[design decision] Why the driver lives in `athena-core`, not the binary.** The
//! composition root (`athena-api`) already owns process supervision; adding the loop's
//! semantics there would put state-machine knowledge in the binary and make the loop
//! untestable without a resident server. This module holds the semantics over the
//! repositories and the Runtime Interface, and the binary supplies the cadence.

use athena_persistence::repository::{NewEvent, ResultRepository, TaskRepository};
use athena_persistence::{AthenaStore, PersistenceError};
use athena_runtime::interface::Runtime;
use athena_runtime::{RuntimeError, RuntimeInstanceRef};
use athena_types::{
    AgentId, ClarificationId, EventId, ExecutionResult, InstanceId, MessageId, MessageType,
    ResultId, ResultStatus, Task, TaskId, TaskState,
};

/// The thread every Core-Agent exchange rides. A Task's whole life is one thread, so the
/// Core Agent can hold the Task's context across turns (`ATH-MSG-004`).
#[must_use]
pub fn core_thread(task: &TaskId) -> String {
    format!("core:{}", task.as_str())
}

/// Why a loop step did not complete.
#[derive(Debug, thiserror::Error)]
pub enum LoopFault {
    /// The Task was not in the state the step requires — a racing consumer got there
    /// first, or the caller mis-sequenced. `ATH-STATE-014`'s optimistic check, surfaced.
    #[error("task {task} is in {actual}, not {expected}")]
    NotInState {
        /// The Task.
        task: String,
        /// What the step required.
        expected: String,
        /// What the store holds.
        actual: String,
    },
    /// The Core Agent has not answered yet. Not an error: the ordinary intermediate state
    /// of a delegation in flight.
    #[error("the Core Agent has not answered yet")]
    NoAnswerYet,
    /// The Runtime refused the delivery.
    #[error("runtime refused the delivery: {0}")]
    Runtime(#[from] RuntimeError),
    /// The store refused a write.
    #[error("store fault: {0}")]
    Store(#[from] PersistenceError),
    /// The agent's body was not the protocol JSON the persona directs it to emit
    /// (`ATH-CORE-033`): unparseable, or a `kind` outside the twelve. Not fatal — the
    /// body is still an answer and is recorded as one; the caller decides whether to
    /// re-ask (the JSON Schema gate's 3-attempt rule is `ATH-CORE-040`, driven by the
    /// composition root, which is why the fault carries the detail).
    #[error("the answer is not a CoreRequest: {0}")]
    NotProtocol(String),
    /// The clarification path refused an `ask_human` route (`ATH-HITL-018`'s own rules:
    /// empty question, past deadline). Carries the path's own text.
    #[error("the clarification refused: {0}")]
    Clarify(String),
}

/// What one delivery produced, and the message the Runtime must carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDelivery {
    /// The Task being run.
    pub task_id: TaskId,
    /// The message to send to instance #0.
    pub message: athena_runtime::Message,
    /// The thread the exchange rides.
    pub thread_id: String,
}

/// What one answer intake produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Received {
    /// The Task the result belongs to.
    pub task_id: TaskId,
    /// The persisted Result's Core-minted identifier (`ATH-CORE-041`).
    pub result_id: ResultId,
    /// The self-declared status the agent reported (`ATH-EVAL-005`).
    pub status: ResultStatus,
}

/// How a Task settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settled {
    /// `EVALUATING → SUCCESS` (`CriteriaMet`).
    Succeeded,
    /// `EVALUATING → FAILED` (`BudgetExhausted`) — the completion criteria were not met.
    Failed,
}

/// The queue the loop consumes: every Task in `QUEUED`, oldest first.
///
/// Ordered by `created_at` so the loop drains in submission order — the FIFO within one
/// priority that [`crate::scheduler`] applies to PlanSteps, lifted to the Task level.
///
/// # Errors
/// Store faults surface whole.
pub fn queued_tasks(store: &AthenaStore) -> Result<Vec<Task>, PersistenceError> {
    let mut rows = TaskRepository::list_by_state(store, TaskState::Queued, u64::MAX)?;
    rows.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.as_str().cmp(b.id.as_str()))
    });
    Ok(rows)
}

/// The Task already in flight, if any (`RUNNING`), oldest first.
///
/// # Errors
/// Store faults surface whole.
pub fn running_tasks(store: &AthenaStore) -> Result<Vec<Task>, PersistenceError> {
    let mut rows = TaskRepository::list_by_state(store, TaskState::Running, u64::MAX)?;
    rows.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.as_str().cmp(b.id.as_str()))
    });
    Ok(rows)
}

/// Transition a `QUEUED` Task into `RUNNING` and build the `instruction` to deliver.
///
/// The transition happens **first**, so a successful send is always recorded as a running
/// Task whose `TaskStarted` Event exists (`ATH-PERS-003`). A delivery the Runtime then
/// refuses leaves the Task `RUNNING` with no work in flight, which the caller's failure
/// path settles ([`fail_task`]) rather than silently re-queueing.
///
/// # Errors
/// [`LoopFault::NotInState`] when the Task is not `QUEUED`; [`LoopFault::Store`] on a
/// store fault.
pub fn prepare_delivery(
    store: &mut AthenaStore,
    task: &TaskId,
    instance_id: &InstanceId,
    now: &str,
    correlation_id: &str,
) -> Result<PendingDelivery, LoopFault> {
    let row = TaskRepository::get(store, task)?.ok_or_else(|| LoopFault::NotInState {
        task: task.to_string(),
        expected: TaskState::Queued.as_str().to_string(),
        actual: "absent".into(),
    })?;
    if row.state != TaskState::Queued {
        return Err(LoopFault::NotInState {
            task: task.to_string(),
            expected: TaskState::Queued.as_str().to_string(),
            actual: row.state.as_str().to_string(),
        });
    }

    // `QUEUED → RUNNING` (`SlotAcquired`), transition + `TaskStarted` in one unit.
    let started = NewEvent {
        event_id: EventId::new(),
        schema_version: 1,
        event_type: "TaskStarted".into(),
        occurred_at: now.into(),
        task_id: Some(task.clone()),
        plan_step_id: None,
        agent_id: Some(AgentId::core_agent()),
        instance_id: Some(instance_id.clone()),
        skill_id: None,
        correlation_id: correlation_id.into(),
        causation_id: None,
        actor: "scheduler".into(),
        payload_json: serde_json::json!({ "instance_id": instance_id.as_str() }).to_string(),
    };
    store.transition_task(task, TaskState::Queued, TaskState::Running, &started)?;

    // The delegation body is the human's instruction, verbatim (`ATH-DATA-004`). The
    // Adapter wraps it in the operating-model direction (`ATH-AIPRJ-011`).
    let thread_id = core_thread(task);
    let message = athena_runtime::Message {
        message_id: MessageId::new(),
        message_type: MessageType::Instruction,
        from_agent: Some(AgentId::core_agent()),
        from_instance_id: Some(instance_id.clone()),
        thread_id: thread_id.clone(),
        reply_to: None,
        priority: row.priority,
        body: row.instruction.clone(),
        payload_json: None,
        deadline: None,
    };

    Ok(PendingDelivery {
        task_id: task.clone(),
        message,
        thread_id,
    })
}

/// Deliver a prepared message to the Core Agent.
///
/// The lock-free half of a delivery: the caller holds no store while this awaits, so a
/// slow agent round trip never blocks the API's request handling.
///
/// # Errors
/// [`LoopFault::Runtime`] when the Runtime refuses the send.
pub async fn deliver(
    runtime: &dyn Runtime,
    instance: &RuntimeInstanceRef,
    pending: &PendingDelivery,
) -> Result<(), LoopFault> {
    runtime.send(instance, pending.message.clone()).await?;
    Ok(())
}

/// Persist the agent's answer as the Task's Result and transition `RUNNING → EVALUATING`.
///
/// The answer's body becomes the Result's output; its self-declared status is recorded and
/// marked self-declared (`ATH-EVAL-005`), and the identifier is re-minted Core-side
/// (`ATH-CORE-041`, D-7).
///
/// # Errors
/// [`LoopFault::NotInState`] when the Task is not `RUNNING`, [`LoopFault::Store`] on a
/// store fault.
pub fn ingest_answer(
    store: &mut AthenaStore,
    task: &TaskId,
    instance: &RuntimeInstanceRef,
    answer: &athena_runtime::Message,
    now: &str,
    correlation_id: &str,
) -> Result<Received, LoopFault> {
    let row = TaskRepository::get(store, task)?.ok_or_else(|| LoopFault::NotInState {
        task: task.to_string(),
        expected: TaskState::Running.as_str().to_string(),
        actual: "absent".into(),
    })?;
    if row.state != TaskState::Running {
        return Err(LoopFault::NotInState {
            task: task.to_string(),
            expected: TaskState::Running.as_str().to_string(),
            actual: row.state.as_str().to_string(),
        });
    }

    let status = reported_status(answer);
    let result_id = ResultId::new();
    // The Result row: the agent's product, recorded as received. `metadata` keeps the
    // self-declaration marker so a later reader cannot mistake it for an acceptance
    // (ATH-EVAL-005).
    let metadata = serde_json::json!({
        "self_declared": true,
        "reported_status": status.as_str(),
        "message_id": answer.message_id.as_str(),
    });
    let result = ExecutionResult {
        id: result_id.clone(),
        task_id: task.clone(),
        plan_step_id: None,
        agent_id: AgentId::core_agent(),
        instance_id: instance.instance_id.clone(),
        status,
        output: answer.body.clone(),
        artifacts: Vec::new(),
        logs_ref: None,
        skills_used: Vec::new(),
        model: None,
        started_at: now.into(),
        finished_at: now.into(),
        execution_time_ms: 0,
        metadata,
    };
    ResultRepository::insert(store, &result)?;

    // `RUNNING → EVALUATING` (`ResultReceived`), transition + `TaskResultReceived`.
    let received_event = NewEvent {
        event_id: EventId::new(),
        schema_version: 1,
        event_type: "TaskResultReceived".into(),
        occurred_at: now.into(),
        task_id: Some(task.clone()),
        plan_step_id: None,
        agent_id: Some(AgentId::core_agent()),
        instance_id: Some(instance.instance_id.clone()),
        skill_id: None,
        correlation_id: correlation_id.into(),
        causation_id: None,
        actor: "core-agent".into(),
        payload_json: serde_json::json!({
            "result_id": result_id.as_str(),
            "self_declared_status": status.as_str(),
        })
        .to_string(),
    };
    store.transition_task(
        task,
        TaskState::Running,
        TaskState::Evaluating,
        &received_event,
    )?;

    Ok(Received {
        task_id: task.clone(),
        result_id,
        status,
    })
}

/// Settle a Task once its Result is in hand.
///
/// The verdict is the agent's own: `ResultStatus::Success` means the agent reports the
/// criteria met, anything else means it does not. Core Host records the outcome and takes
/// no view beyond it (`ATH-CORE-022`, `ATH-EVAL-005` — a self-declaration cannot make a
/// deliverable *accepted*, but it is what decides which terminal row applies here).
///
/// # Errors
/// [`LoopFault::NotInState`] when the Task is not `EVALUATING`, [`LoopFault::Store`] on a
/// store fault.
pub fn settle_task(
    store: &mut AthenaStore,
    task: &TaskId,
    status: ResultStatus,
    now: &str,
    correlation_id: &str,
) -> Result<Settled, LoopFault> {
    let row = TaskRepository::get(store, task)?.ok_or_else(|| LoopFault::NotInState {
        task: task.to_string(),
        expected: TaskState::Evaluating.as_str().to_string(),
        actual: "absent".into(),
    })?;
    if row.state != TaskState::Evaluating {
        return Err(LoopFault::NotInState {
            task: task.to_string(),
            expected: TaskState::Evaluating.as_str().to_string(),
            actual: row.state.as_str().to_string(),
        });
    }

    let (to, event_type, trigger_note) = match status {
        ResultStatus::Success => (TaskState::Success, "TaskCompleted", "criteria met"),
        ResultStatus::Failure | ResultStatus::Partial => {
            (TaskState::Failed, "TaskFailed", "criteria unmet")
        }
    };
    let event = NewEvent {
        event_id: EventId::new(),
        schema_version: 1,
        event_type: event_type.into(),
        occurred_at: now.into(),
        task_id: Some(task.clone()),
        plan_step_id: None,
        agent_id: Some(AgentId::core_agent()),
        instance_id: None,
        skill_id: None,
        correlation_id: correlation_id.into(),
        causation_id: None,
        actor: "core-agent".into(),
        payload_json: serde_json::json!({
            "self_declared_status": status.as_str(),
            "basis": trigger_note,
        })
        .to_string(),
    };
    store.transition_task(task, TaskState::Evaluating, to, &event)?;

    Ok(match to {
        TaskState::Success => Settled::Succeeded,
        _ => Settled::Failed,
    })
}

/// Mark a Task `FAILED` from a state whose execution could not proceed.
///
/// The failure path a delivery fault takes: the Task must not be left in a non-terminal
/// state that no consumer will look at again — the observed symptom was exactly that. The
/// transition uses the row the §17.1 table offers for the state the Task is actually in;
/// where the table offers none, the caller sees the refusal, because inventing an edge
/// would be Core Host deciding (`ATH-STATE-002`, `ATH-CORE-049`).
///
/// # Errors
/// [`LoopFault::NotInState`] when the state has no failure row, [`LoopFault::Store`] on a
/// store fault.
pub fn fail_task(
    store: &mut AthenaStore,
    task: &TaskId,
    reason: &str,
    now: &str,
    correlation_id: &str,
) -> Result<(), LoopFault> {
    let row = TaskRepository::get(store, task)?.ok_or_else(|| LoopFault::NotInState {
        task: task.to_string(),
        expected: "any non-terminal".into(),
        actual: "absent".into(),
    })?;
    let event = NewEvent {
        event_id: EventId::new(),
        schema_version: 1,
        event_type: "TaskFailed".into(),
        occurred_at: now.into(),
        task_id: Some(task.clone()),
        plan_step_id: None,
        agent_id: Some(AgentId::core_agent()),
        instance_id: None,
        skill_id: None,
        correlation_id: correlation_id.into(),
        causation_id: None,
        actor: "system".into(),
        payload_json: serde_json::json!({ "reason": reason }).to_string(),
    };
    // The only non-terminal states with a failure row are RUNNING
    // (`UnrecoverableError`) and EVALUATING (`BudgetExhausted`).
    match row.state {
        TaskState::Running | TaskState::Evaluating => {
            store.transition_task(task, row.state, TaskState::Failed, &event)?;
            Ok(())
        }
        other => Err(LoopFault::NotInState {
            task: task.to_string(),
            expected: "RUNNING or EVALUATING (the states with a failure row)".into(),
            actual: other.as_str().to_string(),
        }),
    }
}

/// Read the next answer off the instance's message stream, if there is one.
///
/// **The answer is the completion product** (`ATH-MSG-006`'s taxonomy: `complete` is the
/// report of finished work; `report` is an intermediate observation). An intake that
/// accepted a mid-run `report` would settle the Task off a progress note — the fault the
/// failing suite row exposed — so a `report` is skipped as a heartbeat would be. An empty
/// stream is the ordinary intermediate state of a delegation in flight, surfaced as
/// [`LoopFault::NoAnswerYet`] so the caller can tell it from a refusal.
///
/// # Errors
/// [`LoopFault::NoAnswerYet`] when no `complete` message has arrived yet.
pub async fn next_answer(
    runtime: &dyn Runtime,
    instance: &RuntimeInstanceRef,
) -> Result<athena_runtime::Message, LoopFault> {
    use futures::StreamExt as _;
    let mut stream = runtime.receive(instance);
    while let Some(message) = stream.next().await {
        if message.message_type == MessageType::Complete {
            return Ok(message);
        }
    }
    Err(LoopFault::NoAnswerYet)
}

/// What one answer's protocol face resolved to (`ATH-CORE-033`'s twelve kinds, routed).
// No `Eq`: `CoreRequest` carries `serde_json::Value` (ExecuteTool's `args`), which is
// `PartialEq`-only — the loop compares by pattern-match, never whole-value equality.
#[derive(Debug, Clone, PartialEq)]
pub enum Interpreted {
    /// `complete_task`: the Task's closing summary and the Results it closes with — the
    /// settlement's product.
    Completed {
        /// The agent's closing summary (or, for a bare-prose answer, the product text).
        summary: String,
        /// The Results the agent closes with; empty in the one-round-trip v1 model, where
        /// the intake mints the single Result (`ATH-DATA-004`).
        result_ids: Vec<ResultId>,
    },
    /// `ask_human`: the Core Agent asked the human a question; the clarification path owns
    /// the wait (`ATH-HITL-017..020`). Open it before any settle — a question must never
    /// settle its own Task.
    AskHuman {
        /// The question, whose digest alone travels through Events (`ATH-EVENT-007`).
        question: String,
        /// The options, when the question is a multiple choice.
        options: Option<Vec<String>>,
    },
    /// `submit_plan`: the agent submitted a full replacement plan at a revision (X-43).
    /// This is **not** an answer — a plan is the work's shape, not its product — so the
    /// caller must route it through `plan_intake::accept_plan` and leave the Task running.
    /// Settling a Task on its own plan submission is the conflation this variant exists to
    /// end (`ATH-PLAN-001`: the plan is the execution model, and it stays editable).
    PlanSubmitted {
        /// The Task the plan belongs to.
        task_id: athena_types::TaskId,
        /// The revision the agent edited against (`ATH-CORE-042` — Core Host compares it
        /// with the current revision and refuses a mismatch).
        base_revision: u64,
        /// The draft steps, without identifiers (D-5; Core Host mints them).
        steps: Vec<athena_types::PlanStepDraft>,
    },
    /// `delegate`: the agent selected an agent for a PlanStep attempt (X-44). Like a plan
    /// submission this is **not** an answer — the caller routes it through
    /// `delegation::accept_delegation`, which admits or refuses it, and leaves the Task
    /// running either way.
    DelegationRequested(crate::delegation::DelegationRequest),
    /// Any of the other eight: an observation of the agent's stated intention. The v1
    /// execution model (`ATH-DATA-004`) gives the answer itself as the product, so the
    /// request is surfaced for the caller to route — Core Host answers every `CoreRequest`
    /// (`ATH-CORE-034`), and the answer's face is the caller's next delivery, never a side
    /// path this loop would run behind the caller's back (`ATH-CORE-033`). The request
    /// itself rides here, so a caller who *can* answer a kind (`write_memory`'s explicit
    /// judgement, X-34) routes it without re-parsing the raw answer.
    Requested {
        /// The serde tag of the requested kind (see [`crate::protocol::REQUEST_KINDS`]).
        kind: &'static str,
        /// The request as parsed (`ATH-CORE-033`'s gate already validated it).
        request: crate::protocol::CoreRequest,
    },
}

/// Interpret one answer's body against the Core Control Protocol (`ATH-CORE-033`).
///
/// The protocol's own gate runs first: the payload must validate against the generated
/// schema (`ATH-CORE-039`) before anything inspects it, and a refusal carries the gate's
/// error text so the caller can re-ask the agent (`ATH-CORE-040`'s re-emission budget is
/// the agent-side discipline; the host's face here is one candidate per answer, a fresh
/// gate each time).
///
/// A body that is not JSON at all is not a protocol fault: the ordinary v1 answer is the
/// product text (`ATH-DATA-004`), so bare prose reads as the completion summary — the
/// persona directs the agent to emit the JSON, but the product face never depends on it.
///
/// # Errors
/// [`LoopFault::NotProtocol`] when the body parses as JSON but fails the schema — the
/// agent tried the protocol and missed it. The body is still an answer (the caller records
/// it as one); the fault carries the gate's text verbatim for the re-ask.
pub fn interpret_answer(message: &athena_runtime::Message) -> Result<Interpreted, LoopFault> {
    let refused = |detail: String| LoopFault::NotProtocol(detail);
    let candidate: serde_json::Value = match serde_json::from_str(message.body.trim()) {
        Ok(value) => value,
        Err(_) => {
            return Ok(Interpreted::Completed {
                summary: message.body.clone(),
                result_ids: Vec::new(),
            });
        }
    };
    let payload = candidate.to_string();
    let mut gate = crate::schema::ValidationGate::new().map_err(refused)?;
    match gate.attempt(&payload).map_err(refused)? {
        crate::schema::GateOutcome::Accepted(request) => Ok(route(request)),
        crate::schema::GateOutcome::Rejected { error, .. } => Err(refused(error)),
        crate::schema::GateOutcome::Escalated { error } => Err(refused(error)),
    }
}

/// The kind → outcome routing of [`interpret_answer`].
fn route(request: crate::protocol::CoreRequest) -> Interpreted {
    match request {
        crate::protocol::CoreRequest::CompleteTask {
            summary,
            result_ids,
            ..
        } => Interpreted::Completed {
            summary,
            result_ids,
        },
        crate::protocol::CoreRequest::AskHuman {
            question, options, ..
        } => Interpreted::AskHuman { question, options },
        crate::protocol::CoreRequest::SubmitPlan {
            task_id,
            base_revision,
            steps,
        } => Interpreted::PlanSubmitted {
            task_id,
            base_revision,
            steps,
        },
        crate::protocol::CoreRequest::Delegate {
            task_id,
            plan_step_id,
            agent_id,
            skill_set,
            idempotency_key,
            // The delegation instruction is the *agent's* procedure reference, carried to
            // the subordinate when its process starts — the admission decision this routes
            // to does not consult it, so it is not projected into the request type.
            instruction: _,
        } => Interpreted::DelegationRequested(crate::delegation::DelegationRequest {
            task_id,
            plan_step_id,
            agent_id,
            skill_set,
            idempotency_key,
        }),
        other => Interpreted::Requested {
            kind: other.kind_tag(),
            request: other,
        },
    }
}

/// Open the clarification an `ask_human` requested (`ATH-HITL-018`).
///
/// The row goes `requested` and the Task `RUNNING → WAITING_INPUT` through the
/// clarification path's own store face — silence never answers, the deadline's expiry
/// fails the Task, and a later human answer resumes it (`WAITING_INPUT → RUNNING`, or
/// `REPLANNING` on a forced plan change) via the API's answer endpoint, which routes the
/// same call.
///
/// # Errors
/// [`LoopFault::NotInState`] when the Task is not in an asking state (`RUNNING` /
/// `REPLANNING`); [`LoopFault::Clarify`] when the path's own rules refuse; [`LoopFault::Store`] on a
/// store fault.
pub fn open_clarification(
    store: &mut AthenaStore,
    task: &TaskId,
    question: &str,
    expires_at: &str,
    now: &str,
    correlation_id: &str,
) -> Result<ClarificationId, LoopFault> {
    use crate::clarifications::ClarifyFailure;
    match crate::clarifications::ask_task(store, task, question, expires_at, now, correlation_id) {
        Ok(clarification) => Ok(clarification.id),
        Err(ClarifyFailure::Store(fault)) => Err(LoopFault::Store(fault)),
        Err(ClarifyFailure::UnknownTask(detail)) | Err(ClarifyFailure::NotAnswerable(detail)) => {
            Err(LoopFault::Clarify(detail))
        }
        Err(ClarifyFailure::UnknownClarification(detail)) => Err(LoopFault::Clarify(detail)),
        Err(ClarifyFailure::Fault(fault)) => match fault {
            crate::clarifications::ClarifyFault::NotAsking(_) => Err(LoopFault::NotInState {
                task: task.as_str().to_string(),
                expected: "an asking state (RUNNING / REPLANNING)".into(),
                actual: fault.to_string(),
            }),
            other => Err(LoopFault::Clarify(other.to_string())),
        },
    }
}

/// The status an agent's answer self-declares (`ATH-EVAL-005`).
///
/// A `complete` message is a report of success; anything else reads as a non-success. The
/// value is the agent's statement, never an acceptance.
#[must_use]
pub fn reported_status(message: &athena_runtime::Message) -> ResultStatus {
    if message.message_type == MessageType::Complete {
        ResultStatus::Success
    } else {
        ResultStatus::Partial
    }
}
