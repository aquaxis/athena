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

use std::path::Path;

use crate::broker::BrokerDelivery as Delivery;
use athena_events::envelope::Actor;
use athena_persistence::repository::{
    AgentRepository, EventRepository, MessageRepository, NewEvent, ResultRepository, TaskRepository,
};
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

/// Settle a Task once its Result is in hand — against what is **recorded** about that
/// Result, not against the agent's word alone (X-48, §19.2).
///
/// The decision is `result_manager::record::settlement`'s: an objective accepting
/// Evaluation drives `SUCCESS`; any other recorded verdict drives `FAILED` with the
/// verdict preserved; a Result carrying verifiable deliverables that no objective
/// Verification accepted may not be `SUCCESS` at all (`ATH-EVAL-002`); and only when
/// nothing is recorded and nothing verifiable was produced does the self-declared status
/// decide, which is the case `ATH-EVAL-002` does not reach. Core Host still takes no view
/// of its own (`ATH-CORE-022`) — it reads rows.
///
/// # Errors
/// [`LoopFault::NotInState`] when the Task is not `EVALUATING` or the Result is absent,
/// [`LoopFault::Store`] on a store fault.
pub fn settle_task(
    store: &mut AthenaStore,
    task: &TaskId,
    result_id: &ResultId,
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

    let result = ResultRepository::get(store, result_id)?.ok_or_else(|| LoopFault::NotInState {
        task: task.to_string(),
        expected: "a recorded Result to settle against".into(),
        actual: format!("result {} is absent", result_id.as_str()),
    })?;
    let evaluations = ResultRepository::list_evaluations_for_result(store, result_id)?;
    let settled = crate::result_manager::record::settlement(&result, &evaluations);
    let (to, event_type) = (settled.state, settled.event_type);
    let ground = match &settled.ground {
        crate::result_manager::record::Ground::Verified { method, verifier } => {
            serde_json::json!({
                "kind": "objective_verification",
                "method": method,
                "verifier": verifier,
            })
        }
        crate::result_manager::record::Ground::Verdict(verdict) => serde_json::json!({
            "kind": "recorded_verdict",
            "verdict": format!("{verdict:?}").to_lowercase(),
        }),
        crate::result_manager::record::Ground::UnverifiedDeliverables => serde_json::json!({
            "kind": "unverified_deliverables",
            "detail": "the Result carries verifiable deliverables and no objective \
                       Verification accepted them (ATH-EVAL-002)",
        }),
        crate::result_manager::record::Ground::SelfDeclared(status) => serde_json::json!({
            "kind": "self_declared",
            "status": status.as_str(),
            "detail": "no Evaluation bears on this Result and it carries no verifiable \
                       deliverable (ATH-EVAL-005 is not stood on: nothing is accepted as \
                       verified here)",
        }),
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
            "result_id": result_id.as_str(),
            "self_declared_status": result.status.as_str(),
            "evaluations": evaluations.len(),
            "ground": ground,
        })
        .to_string(),
    };
    let stored = store.transition_task(task, TaskState::Evaluating, to, &event)?;

    // The lease rode the Task's lifetime (`ATH-DATA-009`): the terminal edge ends it, so
    // the first production `release` runs here and the catalog's `WorkspaceReleased` is
    // appended on the settlement's own correlation. A release fault refuses the
    // settlement — a Terminal Task holding its lease would pin the Workspace against
    // every later Task.
    workspace_lease_released(store, task, correlation_id, now)?;

    // The catalog's completion face (X-54): a settlement is an instance's work done, so
    // the catalog's `AgentCompleted` walks on the Success foot — the same foot the §19.2
    // walk's step 13 rides (a failed settlement is `TaskFailed`'s row, already carried
    // above). It folds the row the settlement itself produced as its cause, so the two
    // Events are one chain (`ATH-EVENT-001`); the payload is the catalog's `empty()`.
    if to == TaskState::Success {
        let typed = athena_events::TypedEvent::new(
            athena_events::events::AgentCompleted {},
            athena_events::EventContext {
                event_id: EventId::new(),
                occurred_at: now.to_string(),
                actor: athena_types::Actor::CoreAgent,
                task_id: Some(task.clone()),
                plan_step_id: None,
                agent_id: Some(AgentId::core_agent()),
                instance_id: None,
                skill_id: None,
                correlation_id: correlation_id.to_string(),
                causation_id: Some(stored.event_id),
            },
        )
        .map_err(|fault| {
            LoopFault::Store(PersistenceError::Plainly(format!(
                "the completion Event refused its own construction: {fault}"
            )))
        })?;
        EventRepository::append(store, &fold_typed(&typed))?;
    }
    Ok(match to {
        TaskState::Success => Settled::Succeeded,
        _ => Settled::Failed,
    })
}

/// The Workspace lease's release face (X-54, `ATH-DATA-009`): `settle_task`'s terminal
/// edge ends the Task whose lifetime the lease rode, so the lease row is removed here —
/// the first production caller of `WorkspaceLeaseRepository::release` — and the catalog's
/// `WorkspaceReleased` Event is appended on the settlement's own correlation, the payload
/// the catalog's `empty()`. The release rides the settlement's transaction boundary: a
/// release fault refuses the settlement, because a Task terminal state that leaves its
/// lease held would pin the Workspace against every later Task (ATH-DATA-009's exclusivity
/// is the row's own contract).
pub fn workspace_lease_released(
    store: &mut AthenaStore,
    task: &TaskId,
    correlation_id: &str,
    now: &str,
) -> Result<(), PersistenceError> {
    let row = TaskRepository::get(store, task)?.ok_or_else(|| PersistenceError::NotFound {
        entity: "task",
        id: task.as_str().into(),
    })?;
    athena_persistence::WorkspaceLeaseRepository::release(store, &row.workspace)?;

    let typed = athena_events::TypedEvent::new(
        athena_events::events::WorkspaceReleased {},
        athena_events::EventContext {
            event_id: EventId::new(),
            occurred_at: now.to_string(),
            actor: Actor::System,
            task_id: Some(task.clone()),
            plan_step_id: None,
            agent_id: None,
            instance_id: None,
            skill_id: None,
            correlation_id: correlation_id.to_string(),
            causation_id: None,
        },
    )
    .map_err(|fault| {
        PersistenceError::Plainly(format!(
            "the released Event refused its own construction: {fault}"
        ))
    })?;
    EventRepository::append(store, &fold_typed(&typed))?;
    Ok(())
}

/// The X-54 fold: a catalog Event's typed row in `NewEvent` shape — the runtime_instance
/// precedent's fold, shared here so the settlement's face and the lease faces ride one
/// construction (`ATH-EVENT-001..006`).
fn fold_typed<P: athena_events::events::Payload>(typed: &athena_events::TypedEvent<P>) -> NewEvent {
    NewEvent {
        event_id: typed.event_id.clone(),
        schema_version: typed.schema_version,
        event_type: typed.event_type.to_string(),
        occurred_at: typed.occurred_at.clone(),
        task_id: typed.task_id.clone(),
        plan_step_id: typed.plan_step_id.clone(),
        agent_id: typed.agent_id.clone(),
        instance_id: typed.instance_id.clone(),
        skill_id: typed.skill_id.clone(),
        correlation_id: typed.correlation_id.clone(),
        causation_id: typed.causation_id.clone(),
        actor: typed.actor.as_str().to_string(),
        payload_json: typed.payload_json.clone(),
    }
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
            workspace_lease_released(store, task, correlation_id, now)?;
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

// --- X-53: the subordinate's inbound leg ----------------------------------------------------
//
// The outbound leg (X-46) delivers the delegation `instruction` through the Broker and
// records its acceptance. What was missing was the **reception half**: a started Instance
// sends its messages back — `report` while working, `complete` when it finishes (and an
// `error` when its work fails) — and no production code read them. `Runtime::collect_result`
// had zero production callers, `Runtime::receive` was never subscribed for a subordinate,
// and `report_relay_observations`' aggregation check therefore had nowhere it could run.
// This section is that path.
//
// The shape is the out-bound one mirrored. The Broker stays the only route (ATH-MSG-001 /
// ATH-MSG-011): the message the Instance emitted is persisted as a `messages` row by
// [`crate::broker::broker_deliver`] (row → delivery record, ATH-MSG-012's ack contract),
// its arrival in the store *is* the receiving side's fact, and the acceptance Event
// (`AgentMessageReceived`, ATH-MSG-007/014) records what the row's own lifecycle already
// says. A `complete` additionally becomes the Task's Result: collected through
// [`Runtime::collect_result`] (the
// Runtime boundary's own materialization, re-minted Core-side, D-7), artifact-rebased
// through [`crate::result_manager::store_copy::rebase_into_store`] before persistence so
// instance-area artifacts survive the X-41 area sweep (`ATH-RESULT-004`'s copy face;
// `ATH-RESULT-003`'s immutability is untouched — the Result row is written once, after the
// copy), and recorded like [`ingest_answer`]'s with the §23.2 `TaskResultReceived` payload
// derived from [`crate::result_manager::store_copy::reception::received_facts`]. A Task the
// delegation already carried to `EVALUATING` never regresses: a second `complete` is still
// recorded whole (row, delivery, acceptance — the never-dropped rule is not conditioned on
// state), while the *result* face refuses with [`ReceptionFault::NotRunning`].
//
// Nothing here decides (`ATH-CORE-022`): no Task settles, no verdict is recorded, no
// re-plan starts. What lands is the record the deciding agent reads — plus the observation
// facts the aggregation face derives ([`crate::broker::report_relay_observations`],
// ATH-HIER-015), which the caller records however the Event vocabulary of §23.2 permits.
//
// **[design decision] Why the stream is pulled per poll instead of held.** The resident's
// execution loop owns the cadence (the same split that keeps instance #0's intake on
// `Step::Intake`); holding one long-lived subscription per Instance would span the store
// mutex's non-`Send` guard across an await and duplicate the Runtime's at-least-once
// contract. Each poll reads what the boundary holds *now*, dedup answers a replay, and an
// empty stream is the honest in-flight state — never a fault.

/// What one reception from the subordinate produced.
///
/// The `Message` returned is the raw reception (a `complete` carries the body the Result
/// materialized from); the caller drives the *decision* faces (`ATH-CORE-022`) — nothing
/// here settles a Task or records a verdict, reception itself is the procedure.
#[derive(Debug, Clone, PartialEq)]
pub enum Received2 {
    /// A `report` — the Broker-side row landed; the relay observation (when the body
    /// rides subordinate outputs verbatim, `ATH-HIER-015`) is the caller's event face.
    Report {
        /// The message as received from the Instance's stream.
        message: athena_runtime::Message,
        /// The observation pairs when the body passed a subordinate's output through
        /// verbatim (empty otherwise — most reports are the worker's plain summary).
        relay_observations: Vec<athena_messaging::VerbatimRelay>,
    },
    /// The `complete` — the Result row has landed (and instance-area artifacts, when the
    /// Runtime boundary produced any, were copied to the Result Store first).
    Complete {
        /// The Task the Result belongs to.
        task_id: TaskId,
        /// The persisted Result (Core-minted id; D-7 re-minted the boundary's provisional
        /// one).
        result_id: ResultId,
        /// The artifact count the copy face rehomed into the Result Store.
        artifacts_copied: usize,
    },
    /// An `error` — the row landed; the deciding agent reads it (`ATH-MSG-012`: delivered,
    /// never dropped).
    Error {
        /// The message as received.
        message: athena_runtime::Message,
    },
}

/// Why a subordinate reception refused before any record landed.
///
/// The store-half faults carry the whole refusal: a message that persisted but failed
/// downstream still landed its row and delivery record — the caller re-polling answers it
/// by dedup, and the honest residue is what the gauge counts.
#[derive(Debug, thiserror::Error)]
pub enum ReceptionFault {
    /// The Broker refused (route, shape, or store) before the row persisted.
    #[error("the Broker refused the inbound message: {0}")]
    Broker(#[from] crate::broker::BrokerFault),
    /// An acceptance-side refusal surfaced through the dispatch fault envelope
    /// (`store` / `construction` are the reachable variants on the inbound face).
    #[error("message bookkeeping refused: {0}")]
    Dispatch(#[from] crate::broker::DispatchFault),
    /// The store refused a write or read outside a repository call.
    #[error("store fault: {0}")]
    Store(#[from] PersistenceError),
    /// The `complete`'s Result materialization refused at the Runtime boundary (an
    /// unstartable collect, or instance #0's line refusing its own collection).
    #[error("collecting the completion refused: {0}")]
    Collect(#[from] RuntimeError),
    /// The artifact copy refused (first offending path) — the Result row does not land
    /// until the copy question is answered (`ATH-RESULT-003`: nothing is written
    /// half-copied; the caller logs and lets the next poll re-answer).
    #[error("the artifact copy refused: {0}")]
    Rebase(#[from] crate::result_manager::store_copy::RebaseFault),
    /// The Task the message anchors is not `RUNNING`, so the result face refuses: the
    /// delegation was settled, failed or re-planned already, and this Result arrives to a
    /// closed window. The row and acceptance still landed whole (never dropped,
    /// `ATH-MSG-012`).
    #[error(
        "task {task} is in {actual}, not RUNNING: the completion arrived to a \
         settled Task"
    )]
    NotRunning {
        /// The Task the message anchored.
        task: String,
        /// The state the row actually holds.
        actual: String,
    },
    /// The Instance row the message anchors carries no Task and the caller supplied none:
    /// the anchor is a fact (`ATH-DATA-011`), never an invention.
    #[error(
        "instance {0} names no Task; the reception anchor is unknown \
         (ATH-DATA-011: the anchor is a fact, never an invention)"
    )]
    Unanchored(String),
    /// A poisoned store mutex or a repository-level read refusal outside the typed
    /// fault set: the walk refuses instead of proceeding on an inconsistent base
    /// (`ATH-PERS-006`'s no-guess face).
    #[error("reception construction refused: {0}")]
    Construction(String),
    /// The reception gate refused a staged artifact (`ATH-RESULT-002/003`): the copy
    /// question precedes the row, so the fault aborts the Result face whole.
    #[error("staging the Result refused: {0}")]
    Stage(#[from] crate::result_manager::reception::Refused),
}

/// The reception kinds this face records (the never-dropped set with a reception
/// consequence, `ATH-MSG-012`); everything else is Runtime chatter (`Ok(None)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReceptionKind {
    /// An intermediate observation (`ATH-MSG-006`'s taxonomy: never settles a Task).
    Report,
    /// The work's completion — the only settlement-capable kind.
    Complete,
    /// The work's failure report (`ATH-MSG-012`: delivered, never dropped).
    Error,
}

/// Read whatever the subordinate Instance has emitted since the last poll and land every
/// message in the Broker's store with its reception records.
///
/// One poll = one [reception](land_subordinate_message) per message the Runtime boundary
/// currently holds. The kinds that ride — `report`, `complete`, `error` — are the
/// never-dropped set (`ATH-MSG-012`); everything else (`status`, control chatter) is the
/// Runtime boundary's own bookkeeping and stays unrecorded here. For each message, in the
/// order the contracts demand:
///
/// 1. **persist** — `broker_deliver` writes the `messages` row and the delivery record
///    (`ATH-MSG-012`'s ack contract; `ATH-MSG-014`'s dedup answers a redelivered copy);
/// 2. **accept** — the message has reached Core-side, so `mark_accepted` +
///    `AgentMessageReceived` land, the mirror of X-46's outbound acceptance;
/// 3. **complete-only extras** — the Result is collected through
///    [`Runtime::collect_result`], artifacts are rebased into the Result Store first
///    (`ATH-RESULT-004`), and the Result row + `TaskResultReceived` land in one
///    transaction (`ATH-PERS-003`) on a Task that still stands `RUNNING`;
/// 4. **report-only extras** — the aggregation check over the body (and the caller's
///    subordinate-output pairs, when supplied) is derived and returned for the caller to
///    record (`ATH-HIER-015`'s observation face — the consequence is the deciding agent's,
///    never Core Host's).
///
/// The caller owns dedup upstream of a replayed redelivery: `broker_deliver` answers
/// [`Delivery::AlreadyDelivered`], which this reception treats as the honest no-op it is —
/// the row exists; nothing is re-accepted or re-recorded. A fault aborts the poll's
/// *remaining* messages (earlier ones landed whole) — the caller logs and continues; the
/// next poll walks the boundary again and dedup answers what already landed.
///
/// # Errors
/// [`ReceptionFault`] — see the fault table above; nothing is ever left half-recorded.
#[allow(clippy::too_many_arguments)]
pub async fn receive_subordinate_messages<R: Runtime + ?Sized>(
    runtime: &R,
    store: &std::sync::Mutex<AthenaStore>,
    subordinate: &RuntimeInstanceRef,
    superior: &InstanceId,
    workspace: &Path,
    athena_home: &Path,
    task_id: Option<&TaskId>,
    subordinate_outputs: &[(String, String)],
    now: &str,
) -> Result<Vec<Received2>, ReceptionFault> {
    use futures::StreamExt as _;

    // The stream is the Runtime's own: this is the first production subscription for a
    // subordinate Instance — the Adapter's reality is a per-call pull over the files the
    // IPC transport wrote into the Workspace `.athena-outbox`, each reply carrying a fresh
    // `message_id`, so the Broker's `(to_instance, message_id)` dedup answers a refetch.
    let received: Vec<athena_runtime::Message> =
        runtime.receive(subordinate).by_ref().collect().await;
    if received.is_empty() {
        // The empty stream is the ordinary in-flight state, not a fault.
        return Ok(Vec::new());
    }
    let mut landed = Vec::with_capacity(received.len());
    for message in received {
        if let Some(outcome) = land_subordinate_message(
            runtime,
            store,
            subordinate,
            superior,
            workspace,
            athena_home,
            task_id,
            subordinate_outputs,
            &message,
            now,
        )
        .await?
        {
            landed.push(outcome);
        }
    }
    Ok(landed)
}

/// One lock, poisoned or not: a poisoned mutex would hand the reception an inconsistent
/// store, so the walk refuses instead of proceeding (the start face's own discipline,
/// restated here; `ATH-PERS-006`'s no-guess face).
fn lock_the_reception(
    store: &std::sync::Mutex<AthenaStore>,
) -> Result<std::sync::MutexGuard<'_, AthenaStore>, ReceptionFault> {
    store
        .lock()
        .map_err(|_| ReceptionFault::Construction("the store lock is broken (poisoned)".into()))
}

/// One message's full reception: persist, accept, then the kind's own extras, or `None`
/// when the kind carries no production record (a `status` is the Runtime's own
/// bookkeeping, never a Broker row).
///
/// # Errors
/// [`ReceptionFault`], whole (nothing half-recorded).
#[allow(clippy::too_many_arguments)]
async fn land_subordinate_message<R: Runtime + ?Sized>(
    runtime: &R,
    store: &std::sync::Mutex<AthenaStore>,
    subordinate: &RuntimeInstanceRef,
    superior: &InstanceId,
    workspace: &Path,
    athena_home: &Path,
    task_id: Option<&TaskId>,
    subordinate_outputs: &[(String, String)],
    message: &athena_runtime::Message,
    now: &str,
) -> Result<Option<Received2>, ReceptionFault> {
    // ATH-MSG-012's never-dropped kinds with a reception face; the rest is Runtime
    // chatter. `report` must never settle anything (`ATH-MSG-006`'s taxonomy) — its face
    // is the row, the acceptance and the observation facts only.
    let kind = match message.message_type {
        MessageType::Report => ReceptionKind::Report,
        MessageType::Complete => ReceptionKind::Complete,
        MessageType::Error => ReceptionKind::Error,
        _ => return Ok(None),
    };

    // The anchors, all read from rows (never guessed): the Task — the caller's fact when
    // it states one, else the Instance row's own (`X-44` wrote it at delegation); the
    // PlanStep; the sender Agent. The **superior** this reply is addressed to is the
    // caller's stated fact (the delegating core-agent Instance identity the resident
    // holds; the delegation row itself leaves `superior_instance_id` unset — the
    // hierarchy's edges belong to the Hierarchy aggregate, per its own comment), and the
    // workspace and home are the caller's facts too (the same ones `receive` and
    // `collect_result` already read).
    let (anchor_task, anchor_plan_step, anchor_agent) = {
        let guard = lock_the_reception(store)?;
        let Some(row) = AgentRepository::get_instance(&*guard, &subordinate.instance_id)
            .map_err(ReceptionFault::Store)?
        else {
            return Err(ReceptionFault::Unanchored(
                subordinate.instance_id.as_str().to_string(),
            ));
        };
        let task = match task_id {
            Some(task) => task.clone(),
            None => row.task_id.clone().ok_or_else(|| {
                ReceptionFault::Unanchored(subordinate.instance_id.as_str().to_string())
            })?,
        };
        (task, row.plan_step_id.clone(), row.agent_id.clone())
    };

    // 1. Persist: the row — the Instance is the sender (`ATH-MSG-011`'s mirror: Core Host
    //    is never a correspondent) — and the delivery record, the ack contract's own pair
    //    (`ATH-MSG-012`); `ATH-MSG-014`'s dedup answers the second copy. The row's address
    //    is the Runtime's envelope: the reply's identity, thread the delegation opened
    //    (`ATH-MSG-013`) and body ride whole, and the deadline is the inbound kind's —
    //    `None`, because `carries_deadline` names `instruction`/`question` only
    //    (`ATH-MSG-005`). The route world the Broker checks is the hierarchy's own edge
    //    restated: the sender is the subordinate Instance, the receiver the delegating
    //    instance the caller named — the pair X-46 opened in reverse. Anything else on this
    //    face is denied (a Core→subordinate reply over *this* channel is refused: the
    //    outbound leg delivers instructions through `dispatch_instruction`, never here).
    let row = athena_persistence::repository::MessageRow {
        message_id: message.message_id.clone(),
        message_type: message.message_type,
        from_agent: anchor_agent.clone(),
        to_agent: AgentId::core_agent(),
        to_instance_id: Some(superior.clone()),
        from_instance_id: subordinate.instance_id.clone(),
        task_id: Some(anchor_task.clone()),
        thread_id: message.thread_id.clone(),
        reply_to: message.reply_to.clone(),
        priority: message.priority,
        body: message.body.clone(),
        payload_json: message.payload_json.clone(),
        // The inbound kinds carry no reply deadline (`ATH-MSG-005`); a row inventing one
        // would forge a duty nobody owes.
        deadline: None,
        created_at: now.to_string(),
    };
    let delivery = {
        let mut guard = lock_the_reception(store)?;
        crate::broker::broker_deliver(
            row,
            |from, to| {
                // The delegation's own edge, read from the rows: the sender is the
                // subordinate Instance this message rode in from, the receiver the
                // delegating instance the caller named — the pair X-46 opened in
                // reverse. Anything else on this face is denied.
                from == &subordinate.instance_id && to == superior
            },
            &mut *guard,
        )?
    };

    // A replayed redelivery is answered by the recorded delivery: the row, its acceptance
    // and the Result face all stand from the first copy — saying them again would write
    // Events for nothing (`ATH-MSG-014`'s dedup is the answer, and the honest no-op).
    if matches!(delivery, Delivery::AlreadyDelivered { .. }) {
        return Ok(None);
    }

    // 2. Accept: the message reached Core-side, so the row's own lifecycle is stated —
    //    `mark_accepted` (the delivery record's `accepted_at`) and `AgentMessageReceived`
    //    in one lock scope, the mirror of X-46's outbound acceptance (`ATH-MSG-007/014`).
    {
        let mut guard = lock_the_reception(store)?;
        let event = athena_events::events::AgentMessageReceived {
            message_id: message.message_id.clone(),
            message_type: message.message_type,
            thread_id: message.thread_id.clone(),
        };
        crate::broker::record_message_event(
            &mut guard,
            event,
            Some(&anchor_task),
            Some(&subordinate.instance_id),
            now,
        )
        .map_err(ReceptionFault::from)?;
        MessageRepository::mark_accepted(&mut *guard, superior, &message.message_id)
            .map_err(ReceptionFault::Store)?;
    }

    match kind {
        ReceptionKind::Report => {
            // ATH-HIER-015: the observation is *derived* here — whether the body is a
            // verbatim pass-through of the supplied subordinate outputs — but never
            // recorded beyond the row: the consequence (re-request a real aggregation,
            // reject, re-plan) is the deciding agent's (`ATH-CORE-022`).
            Ok(Some(Received2::Report {
                message: message.clone(),
                relay_observations: crate::broker::report_relay_observations(
                    &message.body,
                    subordinate_outputs,
                ),
            }))
        }
        ReceptionKind::Error => Ok(Some(Received2::Error {
            message: message.clone(),
        })),
        ReceptionKind::Complete => {
            // 3. The Result face — the first production walk of `Runtime::collect_result`
            //    (the boundary's own record of the step's work, D-7's provisional id) and
            //    the first production walk of the artifact copy (`ATH-RESULT-004`). The
            //    copy walks before the row exists (`ATH-RESULT-003`: nothing is written
            //    half-copied; a copy fault refuses the Result face whole — the caller logs
            //    and the next poll re-answers).
            let mut result = runtime.collect_result(subordinate).await?;
            let artifacts_copied = crate::result_manager::store_copy::rebase_into_store(
                &mut result,
                workspace,
                athena_home,
            )?;

            // The Result row: the boundary's value with the stored facts enforced — the id
            // is re-minted Core-side (`ATH-CORE-041`/D-7), the Task is the anchor's and the
            // state gate refuses an arrival to a closed window (`NotRunning`) before
            // anything persists.
            let mut guard = lock_the_reception(store)?;
            let row_task = TaskRepository::get(&*guard, &anchor_task)
                .map_err(ReceptionFault::Store)?
                .ok_or_else(|| ReceptionFault::NotRunning {
                    task: anchor_task.as_str().to_string(),
                    actual: "absent".to_string(),
                })?;
            if row_task.state != TaskState::Running {
                return Err(ReceptionFault::NotRunning {
                    task: anchor_task.as_str().to_string(),
                    actual: row_task.state.as_str().to_string(),
                });
            }

            // One transaction — the Result row, the `RUNNING → EVALUATING` edge and its
            // `TaskResultReceived` carry (ATH-PERS-003), the payload derived from the
            // stored facts (`ATH-EVAL-005`'s self-declaration marker rides `metadata`).
            let status = result.status;
            let result_id = ResultId::new();
            let stored = ExecutionResult {
                id: result_id.clone(),
                task_id: anchor_task.clone(),
                plan_step_id: anchor_plan_step.clone(),
                agent_id: anchor_agent.clone(),
                instance_id: subordinate.instance_id.clone(),
                status,
                output: result.output,
                artifacts: result.artifacts,
                logs_ref: None,
                skills_used: result.skills_used,
                model: result.model,
                started_at: result.started_at,
                finished_at: result.finished_at,
                execution_time_ms: result.execution_time_ms,
                metadata: serde_json::json!({
                    "self_declared": true,
                    "reported_status": status.as_str(),
                    "message_id": message.message_id.as_str(),
                }),
            };
            crate::result_manager::reception::stage(&stored)?;
            ResultRepository::insert(&mut *guard, &stored).map_err(ReceptionFault::Store)?;

            let correlation = EventRepository::list_for_task(&*guard, &anchor_task)
                .map_err(ReceptionFault::Store)?
                .into_iter()
                .rev()
                .map(|event| event.correlation_id)
                .find(|chain| !chain.is_empty())
                .unwrap_or_else(|| athena_events::redact::digest(&format!("message-{now}")));
            let received_event = NewEvent {
                event_id: EventId::new(),
                schema_version: 1,
                event_type: "TaskResultReceived".into(),
                occurred_at: now.to_string(),
                task_id: Some(anchor_task.clone()),
                plan_step_id: anchor_plan_step.as_ref().cloned(),
                agent_id: Some(anchor_agent),
                instance_id: Some(subordinate.instance_id.clone()),
                skill_id: None,
                correlation_id: correlation,
                causation_id: None,
                actor: "system".into(),
                payload_json: match crate::result_manager::reception::received_facts(&stored) {
                    Some(facts) => serde_json::json!({
                        "result_id": facts.result_id,
                        "plan_step_id": facts.plan_step_id,
                        "status": facts.status,
                    })
                    .to_string(),
                    None => serde_json::json!({
                        "result_id": stored.id.as_str(),
                        "status": stored.status.as_str(),
                    })
                    .to_string(),
                },
            };
            let guard_store = &mut *guard;
            guard_store
                .transition_task(
                    &anchor_task,
                    TaskState::Running,
                    TaskState::Evaluating,
                    &received_event,
                )
                .map_err(ReceptionFault::Store)?;
            drop(guard);

            Ok(Some(Received2::Complete {
                task_id: anchor_task,
                result_id,
                artifacts_copied,
            }))
        }
    }
}

// --- X-53: the resident's wait records ------------------------------------------------------

/// One dispatched `instruction` the resident still awaits a reply to (`ATH-MSG-005`).
///
/// The facts are the outbound leg's own: the Broker row's identity, its thread and the
/// deadline the row was written with — kept here so the deadline sweep never re-reads the
/// row to know what it watches. The map that holds these is the Core-Agent side's own wait
/// book: every entry watches a *subordinate's* reply, which is why the deadline's
/// observation face (the sweep) is the only treatment a wait gets here (`ATH-STATE-023`:
/// nothing on this walk may fail instance #0, and a subordinate's own row is the State
/// Manager's — never the resident's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutstandingReply {
    /// The message the deadline attaches to.
    pub message_id: String,
    /// The thread the exchange rides (`ATH-MSG-013`).
    pub thread_id: String,
    /// The row's own deadline (RFC 3339 UTC), exactly as `dispatch_instruction` recorded it.
    pub deadline: String,
    /// The Task the instruction anchored, when the row carried one (the §23.2 Event rides
    /// the Task's own correlation chain).
    pub task_id: Option<TaskId>,
    /// The Agent the deadline watches — the subordinate who owes the reply (`ATH-MSG-005`).
    /// The sweep copies it into the machinery's own record; the treatment on this walk is
    /// fixed to the observation face (`ATH-STATE-023`), so it never branches a state here.
    pub receiver: AgentId,
}

/// The wait records whose deadlines passed at `now`, each answered with an
/// `AgentMessageTimeout` Event (the catalogue's minimum rows: message, thread, deadline —
/// `ATH-MSG-006`; §23.2). The first production emission of that Event: until X-53 the
/// deadline machinery (`athena-messaging::deadline`) had no production walker at all.
///
/// The treatment stays an *observation*, never a state change (`ATH-STATE-023`): a
/// Core-Agent-side expiry must never fail instance #0, and the resident's own wait — the
/// one this sweep watches, Core's wait for a subordinate's reply — is exactly the case the
/// rule names. The subordinate's own row is Runtime state; its deadline treatment is the
/// State Manager's, not this walk's. The caller drops the returned keys from its map; the
/// next tick stops watching what has already answered.
///
/// # Errors
/// [`crate::broker::DispatchFault`] when an Event write refused — the sweep stops whole
/// (the un-emitted remainder stays in the caller's map and re-fires next tick).
pub fn expire_outstanding_replies(
    store: &mut AthenaStore,
    outstanding: &std::collections::HashMap<String, OutstandingReply>,
    now: &str,
) -> Result<Vec<String>, crate::broker::DispatchFault> {
    let mut expired = Vec::new();
    for (key, reply) in outstanding {
        let wait = athena_messaging::WaitRecord {
            message_id: reply.message_id.clone(),
            thread_id: reply.thread_id.clone(),
            deadline: reply.deadline.clone(),
            receiver: reply.receiver.clone(),
        };
        let Some(fault) = wait.expire_at(now) else {
            continue;
        };
        let payload = fault.payload();
        crate::broker::record_message_event(
            store,
            athena_events::events::AgentMessageTimeout {
                message_id: payload.message_id,
                thread_id: payload.thread_id,
                deadline: payload.deadline,
            },
            reply.task_id.as_ref(),
            None,
            now,
        )?;
        expired.push(key.clone());
    }
    Ok(expired)
}

/// Record the aggregation observations one received `report` derived (`ATH-HIER-015`).
///
/// One raw `EventAppended` Event per observation — the catalogue holds no observation kind
/// for the relay (`ATH-EVENT-009`'s set is closed by tests), and the precedent for a raw
/// Event is the maintenance sweep's own record (`maintenance::record_swept`). The payload
/// names the wire code and policy id the pure check minted (`VerbatimRelay::observation`),
/// the relayed subordinate, the derived shape, and the report the observations rode, so
/// the evaluator's consequence can cite the record (`ATH-CORE-022`'s deciding-agent face:
/// Core Host records, never decides).
///
/// # Errors
/// [`PersistenceError`] when the Event write refused, whole (nothing half-recorded).
pub fn record_relay_observations(
    store: &mut AthenaStore,
    observations: &[athena_messaging::VerbatimRelay],
    message: &athena_runtime::Message,
    task_id: Option<&TaskId>,
    instance_id: &InstanceId,
    now: &str,
) -> Result<(), PersistenceError> {
    if observations.is_empty() {
        return Ok(());
    }
    let correlation = match task_id {
        Some(task) => EventRepository::list_for_task(store, task)?
            .into_iter()
            .rev()
            .map(|event| event.correlation_id)
            .find(|chain| !chain.is_empty())
            .unwrap_or_else(|| athena_events::redact::digest(&format!("relay-{now}"))),
        None => athena_events::redact::digest(&format!("relay-{now}")),
    };
    for relay in observations {
        let (code, policy) = relay.observation();
        let event = NewEvent {
            event_id: EventId::new(),
            schema_version: 1,
            event_type: "EventAppended".into(),
            occurred_at: now.to_string(),
            task_id: task_id.cloned(),
            plan_step_id: None,
            agent_id: None,
            instance_id: Some(instance_id.clone()),
            skill_id: None,
            correlation_id: correlation.clone(),
            causation_id: None,
            actor: "system".into(),
            payload_json: serde_json::json!({
                "observation": {
                    "code": code,
                    "policy": policy,
                    "subordinate": relay.subordinate,
                    "detail": relay.detail,
                    "message_id": message.message_id.as_str(),
                    "thread_id": message.thread_id,
                }
            })
            .to_string(),
        };
        EventRepository::append(store, &event)?;
    }
    Ok(())
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
    DelegationRequested {
        /// The admission request: what `delegation::accept_delegation` decides on.
        request: crate::delegation::DelegationRequest,
        /// The delegation instruction — the agent's own procedure reference. The
        /// admission does not consult it; it is the **body of the `instruction` Message**
        /// the Broker delivers to the Instance once it has started (`ATH-MSG-001`,
        /// X-46), so it travels with the request instead of being dropped here.
        instruction: String,
    },
    /// `submit_evaluation`: the Core Agent's judgement of a Result against the completion
    /// criteria (`ATH-EVAL-001`). Unlike the other observations this one is a **record**
    /// the caller must write — the `EVALUATING` state's substance (X-48) — so it is
    /// projected rather than left in the generic arm.
    EvaluationSubmitted {
        /// The Task whose Result is judged.
        task_id: athena_types::TaskId,
        /// The Result under evaluation.
        result_id: ResultId,
        /// One of the five verdicts (`ATH-EVAL-004`).
        verdict: athena_types::Verdict,
        /// The agent's reason (`ATH-CORE-016`).
        rationale: String,
    },
    /// `transition_task`: the Task state change the agent asks for, optimistic on
    /// `expected_state` (`ATH-CORE-036`). Like the plan and the evaluation this is a
    /// **request Core Host executes**, not an observation — the State Manager's
    /// `transition::accept_transition` decides it (X-51).
    TransitionRequested {
        /// The Task.
        task_id: athena_types::TaskId,
        /// The state the agent asks for.
        to: athena_types::TaskState,
        /// The state the agent believes holds (`ATH-STATE-014`).
        expected_state: athena_types::TaskState,
    },
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
            // The delegation instruction is the *agent's* procedure reference. The
            // admission decision does not consult it, so it stays out of the request type
            // — but it is what the started Instance must be told, so it rides beside it
            // and the caller brokers it as the `instruction` Message (X-46).
            instruction,
        } => Interpreted::DelegationRequested {
            request: crate::delegation::DelegationRequest {
                task_id,
                plan_step_id,
                agent_id,
                skill_set,
                idempotency_key,
            },
            instruction,
        },
        crate::protocol::CoreRequest::TransitionTask {
            task_id,
            to,
            expected_state,
        } => Interpreted::TransitionRequested {
            task_id,
            to,
            expected_state,
        },
        crate::protocol::CoreRequest::SubmitEvaluation {
            task_id,
            result_id,
            verdict,
            rationale,
        } => Interpreted::EvaluationSubmitted {
            task_id,
            result_id,
            verdict,
            rationale,
        },
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
