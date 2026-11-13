//! The Runtime Manager's start face (X-44(b)): an admitted `CREATED` Instance walks the
//! §17.2 table into a process, under the resident's supervision — the cadence/semantics
//! split (D-35) keeps the *rule* here (testable without a resident server) and the clock
//! and process orchestration in `athena-api`'s execution loop.
//!
//! What one call owns, in the §17.2's own words:
//!
//! * the `STARTING` leg — `instance_transition(CREATED, StartRequested)` names
//!   `AgentStarting` with the Runtime Manager as requester; the row write and the Event
//!   append are the stop route's two-writes shape (`agents.rs`), never an invented atomic
//!   primitive (no atomic instance transition+Event store method exists, and this unit does
//!   not add one);
//! * the completion leg — the `Started`/`StartFailed` trigger the caller reports from the
//!   Runtime's own outcome; the happy path backfills `runtime_instance_id` (the row's
//!   backing-process fact), seeds the §26.6 `RuntimeInstance` row, and fills
//!   `set_aiprj_area` with the path the Runtime actually wrote (the row's `workspace`'s
//!   `.aiprj/AI_LOG` — the layout `init_workspace` creates), never a guess;
//! * the failure leg — `AgentFailed` with `error_class` / `attempt` /
//!   `side_effects = Unknown` per `ATH-CORE-048` / `ATH-STATE-029` (a refused start leaves
//!   the side effects undetermined).
//!
//! The single-flight invariant (`ATH-DATA-010`) is re-verified here against the row's own
//! *current* state, so a step already holding a non-terminal Instance in `CREATED`/later
//! cannot re-enter.

use athena_events::envelope::Actor;
use athena_persistence::repository::{AgentRepository, EventRepository, RuntimeInstanceRepository};
use athena_persistence::{AthenaStore, NewEvent};
use athena_runtime::{FilesystemScope, Runtime, StartContext};
use athena_state::{InstanceTrigger, instance_transition};
use athena_types::{
    AgentInstanceState, EventId, InstanceId, PlanStepId, RuntimeInstance, RuntimeInstanceId,
    RuntimeType, SideEffects, TaskId,
};

/// Why a start face refused (the caller logs and answers with the §20.1 envelope).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StartFault {
    /// The instance row the request names is absent.
    #[error("instance {0} is absent")]
    InstanceAbsent(String),
    /// Single-flight re-verified against the row's current state (`ATH-DATA-010`): the
    /// step already holds a non-terminal Instance.
    #[error("plan step {step} already holds {count} non-terminal instance(s) (ATH-DATA-010)")]
    StepAlreadyRunning {
        /// The step.
        step: String,
        /// How many non-terminal Instances it holds.
        count: u64,
    },
    /// The state machine refused one of the walk's edges: no rule names the state.
    #[error("the state table refuses {0:?} on this trigger")]
    TransitionRefused(AgentInstanceState),
    /// The backing definition is gone between admission and start (`ATH-AGENT-002`).
    #[error("agent definition {0} is absent (ATH-AGENT-002)")]
    DefinitionAbsent(String),
    /// The store refused a read or a write; surfaced whole.
    #[error("store fault: {0}")]
    Store(String),
    /// An Event construction refused against the catalog.
    #[error("event construction: {0}")]
    Construction(String),
}

impl StartFault {
    /// The §20.1 class of this fault.
    #[must_use]
    pub fn class(&self) -> crate::protocol::ErrorClass {
        match self {
            Self::InstanceAbsent(_) | Self::DefinitionAbsent(_) => {
                crate::protocol::ErrorClass::ValidationError
            }
            Self::StepAlreadyRunning { .. } | Self::TransitionRefused(_) => {
                crate::protocol::ErrorClass::PolicyError
            }
            Self::Store(_) | Self::Construction(_) => crate::protocol::ErrorClass::InternalError,
        }
    }

    /// The stable machine code within the class.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::InstanceAbsent(_) => "INSTANCE_NOT_FOUND",
            Self::StepAlreadyRunning { .. } => "PLAN_STEP_ALREADY_RUNNING",
            Self::TransitionRefused(_) => "INSTANCE_TRANSITION_REFUSED",
            Self::DefinitionAbsent(_) => "AGENT_DEFINITION_NOT_FOUND",
            Self::Store(_) | Self::Construction(_) => "INSTANCE_START_FAILED",
        }
    }
}

/// What one start face recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartOutcome {
    /// The instance started.
    pub instance_id: InstanceId,
    /// The row's state after the face (`IDLE` on the happy path, `FAILED` otherwise).
    pub state: AgentInstanceState,
    /// The Runtime Instance's id on the happy path (the row's backfilled fact).
    pub runtime_instance_id: Option<RuntimeInstanceId>,
    /// The Runtime's pid on the happy path.
    pub pid: Option<u32>,
    /// Whether the Runtime *initialized* the `aiprj` write areas on this start (the
    /// record's `workspace_initialized` fact; a `Ready` workspace records `false`).
    pub workspace_initialized: bool,
    /// The typed `AgentStarted` identity, or the typed `AgentFailed` one.
    pub event_id: EventId,
}

impl StartOutcome {
    /// Whether the walk ended `IDLE`.
    #[must_use]
    pub fn is_started(&self) -> bool {
        self.state == AgentInstanceState::Idle
    }
}

// The `AgentStarting` / `AgentStarted` / `AgentFailed` envelopes' facts. The typed
// builders live in the Event Manager's catalog (the §23.2 payload shapes), but the walk's
// caller-side fold is the same `TypedEvent` → `NewEvent` shape the delegation intake's
// `AgentDelegated` leg keeps, so the payloads are built against the catalog structs here
// and folded once.

/// The §17.2 `STARTING` leg: re-verify single flight, resolve the edge, write the row and
/// append the `AgentStarting` Event on the Task's correlation chain. Two writes, no new
/// store primitive (the stop route's shape).
fn record_starting(
    store: &mut AthenaStore,
    row: &athena_types::AgentInstance,
    now: &str,
    correlation_id: String,
) -> Result<(), StartFault> {
    let resolved = instance_transition(row.state, InstanceTrigger::StartRequested)
        .ok_or(StartFault::TransitionRefused(row.state))?;
    AgentRepository::update_instance_state(store, &row.id, resolved.to).map_err(store_fault)?;

    let typed = athena_events::TypedEvent::new(
        athena_events::events::AgentStarting {},
        athena_events::EventContext {
            event_id: EventId::new(),
            occurred_at: now.to_string(),
            actor: Actor::Runtime,
            task_id: row.task_id.clone(),
            plan_step_id: row.plan_step_id.clone(),
            agent_id: Some(row.agent_id.clone()),
            instance_id: Some(row.id.clone()),
            skill_id: None,
            correlation_id,
            causation_id: None,
        },
    )
    .map_err(|fault| StartFault::Construction(fault.to_string()))?;
    EventRepository::append(store, &fold(&typed)).map_err(store_fault)?;
    Ok(())
}

/// The happy completion leg (`STARTING → IDLE`, `AgentStarted`): the row's runtime id,
/// the §26.6 Runtime Instance seed, and the X-41 `set_aiprj_area` face — the `AI_LOG`
/// directory `init_workspace` actually wrote under the row's workspace.
fn record_started(
    store: &mut AthenaStore,
    row: &athena_types::AgentInstance,
    runtime_ref: &athena_runtime::RuntimeInstanceRef,
    pid: Option<u32>,
    now: &str,
    correlation_id: String,
) -> Result<EventId, StartFault> {
    let resolved = instance_transition(AgentInstanceState::Starting, InstanceTrigger::Started)
        .ok_or(StartFault::TransitionRefused(AgentInstanceState::Starting))?;
    AgentRepository::update_instance_state(store, &row.id, resolved.to).map_err(store_fault)?;
    AgentRepository::set_runtime_instance(store, &row.id, Some(&runtime_ref.runtime_instance_id))
        .map_err(store_fault)?;
    // The X-41 face: the reference is what the Runtime wrote — `init_workspace` gates the
    // readiness and the layout's shape is the workspace's own `.aiprj/AI_LOG` path.
    let area = format!("{}/.aiprj/AI_LOG", row.workspace.trim_end_matches('/'));
    AgentRepository::set_aiprj_area(store, &row.id, Some(&area)).map_err(store_fault)?;

    let runtime_row = RuntimeInstance {
        id: runtime_ref.runtime_instance_id.clone(),
        runtime_type: runtime_ref.runtime_type,
        instance_id: row.id.clone(),
        state: resolved.to,
        pid,
        last_heartbeat_at: Some(now.to_string()),
        started_at: now.to_string(),
        stopped_at: None,
        stop_reason: None,
        exit_code: None,
        side_effects: SideEffects::None,
    };
    RuntimeInstanceRepository::insert(store, &runtime_row).map_err(store_fault)?;

    // The catalog's created face (X-54): the §26.6 Runtime Instance row exists, so the
    // catalog's `RuntimeInstanceCreated` is recorded beside it — the Runtime's own fact
    // (`Actor::Runtime`, the X-44 walk's actor), folded on the walk's correlation chain
    // (`ATH-EVENT-001`); the payload is the catalog's `empty()` (`ATH-EVENT-009`'s closed
    // catalog, no shape invented).
    let created = athena_events::TypedEvent::new(
        athena_events::events::RuntimeInstanceCreated {},
        athena_events::EventContext {
            event_id: EventId::new(),
            occurred_at: now.to_string(),
            actor: Actor::Runtime,
            task_id: row.task_id.clone(),
            plan_step_id: row.plan_step_id.clone(),
            agent_id: Some(row.agent_id.clone()),
            instance_id: Some(row.id.clone()),
            skill_id: None,
            correlation_id: correlation_id.clone(),
            causation_id: None,
        },
    )
    .map_err(|fault| StartFault::Construction(fault.to_string()))?;
    EventRepository::append(store, &fold(&created)).map_err(store_fault)?;

    let typed = athena_events::TypedEvent::new(
        athena_events::events::AgentStarted {
            instance_id: row.id.clone(),
            agent_id: row.agent_id.clone(),
            runtime_type: runtime_ref.runtime_type,
        },
        athena_events::EventContext {
            event_id: EventId::new(),
            occurred_at: now.to_string(),
            actor: Actor::Runtime,
            task_id: row.task_id.clone(),
            plan_step_id: row.plan_step_id.clone(),
            agent_id: Some(row.agent_id.clone()),
            instance_id: Some(row.id.clone()),
            skill_id: None,
            correlation_id,
            causation_id: None,
        },
    )
    .map_err(|fault| StartFault::Construction(fault.to_string()))?;
    let stored = EventRepository::append(store, &fold(&typed)).map_err(store_fault)?;
    Ok(stored.event_id)
}

/// The failure completion leg (`STARTING → FAILED`, `AgentFailed`): `error_class` /
/// `attempt` / `side_effects = Unknown` per `ATH-CORE-048` / `ATH-STATE-029`.
fn record_failed(
    store: &mut AthenaStore,
    row: &athena_types::AgentInstance,
    fault: &str,
    now: &str,
    correlation_id: String,
) -> Result<EventId, StartFault> {
    let resolved = instance_transition(AgentInstanceState::Starting, InstanceTrigger::StartFailed)
        .ok_or(StartFault::TransitionRefused(AgentInstanceState::Starting))?;
    AgentRepository::update_instance_state(store, &row.id, resolved.to).map_err(store_fault)?;

    let typed = athena_events::TypedEvent::new(
        athena_events::events::AgentFailed {
            error_class: crate::protocol::ErrorClass::RuntimeError.wire().to_string(),
            attempt: row.attempt,
            side_effects: SideEffects::Unknown,
        },
        athena_events::EventContext {
            event_id: EventId::new(),
            occurred_at: now.to_string(),
            actor: Actor::Runtime,
            task_id: row.task_id.clone(),
            plan_step_id: row.plan_step_id.clone(),
            agent_id: Some(row.agent_id.clone()),
            instance_id: Some(row.id.clone()),
            skill_id: None,
            correlation_id,
            causation_id: None,
        },
    )
    .map_err(|fault| StartFault::Construction(fault.to_string()))?;
    // The fault text rides the log the caller writes; the payload stays the §23.2 row.
    let _ = fault;
    let stored = EventRepository::append(store, &fold(&typed)).map_err(store_fault)?;
    Ok(stored.event_id)
}

fn fold<P: athena_events::events::Payload>(typed: &athena_events::TypedEvent<P>) -> NewEvent {
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

fn store_fault(fault: athena_persistence::error::PersistenceError) -> StartFault {
    StartFault::Store(fault.to_string())
}

/// The correlation chain the start face joins: the Task's own chain when it carries one,
/// else a per-start mint (`record_delegated` / `beat_once`'s minted-per-face precedent).
///
/// # Errors
/// [`StartFault::Store`] when a read faulted.
pub fn correlation_for(
    store: &AthenaStore,
    task_id: &TaskId,
    instance_id: &InstanceId,
    now: &str,
) -> Result<String, StartFault> {
    let correlation = crate::EventRepository::list_for_task(store, task_id)
        .map_err(store_fault)?
        .into_iter()
        .rev()
        .map(|event| event.correlation_id)
        .find(|chain| !chain.is_empty())
        .unwrap_or_else(|| athena_events::redact::digest(&format!("start-{instance_id:?}-{now}")));
    Ok(correlation)
}

/// The start face, called by the resident's composition: re-verify single flight against
/// the row's current state and walk `CREATED → STARTING`; the [`BeginStart`] answer
/// carries the facts the Runtime's `start` call needs, with the Runtime kind taken from
/// the definition's own `runtime` fact (`ATH-AGENT-001`) — the caller never states it.
///
/// # Errors
/// [`StartFault`], whole; the caller logs and answers with the response envelope.
pub fn begin_start(
    store: &mut AthenaStore,
    instance_id: &InstanceId,
    now: &str,
) -> Result<BeginStart, StartFault> {
    let row = AgentRepository::get_instance(store, instance_id)
        .map_err(store_fault)?
        .ok_or_else(|| StartFault::InstanceAbsent(instance_id.as_str().to_string()))?;
    // The single-flight invariant re-verified here, not only at admission: single-flight
    // holds across admission→start, not inside admission alone.
    if let Some(step_id) = row.plan_step_id.as_ref() {
        let count =
            AgentRepository::count_non_terminal_for_step(store, step_id).map_err(store_fault)?;
        if count != 1 {
            return Err(StartFault::StepAlreadyRunning {
                step: step_id.as_str().to_string(),
                count,
            });
        }
    }
    if row.state != AgentInstanceState::Created {
        return Err(StartFault::TransitionRefused(row.state));
    }
    let definition = AgentRepository::get_definition(store, &row.agent_id)
        .map_err(store_fault)?
        .ok_or_else(|| StartFault::DefinitionAbsent(row.agent_id.as_str().to_string()))?;
    let correlation = correlation_for(store, row.task_id.as_ref().expect("task"), &row.id, now)?;
    record_starting(store, &row, now, correlation)?;
    let step = row.plan_step_id.clone().expect("the row's own step");
    let task = row.task_id.clone().expect("the row's own task");
    // The Runtime kind is the definition's own fact (`ATH-AGENT-001`); the caller never
    // states it, and the closed vocabulary was resolved when the definition was supplied.
    let runtime_type = definition.runtime;
    Ok(BeginStart {
        definition,
        step_id: step,
        task_id: task,
        instance_id: row.id.clone(),
        workspace: row.workspace.clone(),
        skill_set: row.skill_set.clone(),
        runtime_type,
        attempt: row.attempt,
    })
}

/// What the caller drives between the `STARTING` edge and the completion leg.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeginStart {
    /// The definition, from the store (never recomputed).
    pub definition: athena_types::AgentDefinition,
    /// The step the row executes.
    pub step_id: PlanStepId,
    /// The Task the row belongs to.
    pub task_id: TaskId,
    /// The instance.
    pub instance_id: InstanceId,
    /// The row's own workspace, the cwd the Runtime enters.
    pub workspace: String,
    /// The frozen Skill Set (`ATH-DATA-006`).
    pub skill_set: Vec<athena_types::SkillRef>,
    /// The Runtime kind enforcing the isolation boundary.
    pub runtime_type: RuntimeType,
    /// The attempt the row is.
    pub attempt: u64,
}

/// The completion leg the caller answers after the Runtime's own outcome: `IDLE` on a
/// successful start (the row's `runtime_instance_id` and the Runtime Instance row), or
/// `FAILED` on a refused start.
///
/// # Errors
/// [`StartFault::InstanceAbsent`] when the row is gone; store or construction faults whole.
pub fn complete_start_started(
    store: &mut AthenaStore,
    instance_id: &InstanceId,
    runtime_ref: &athena_runtime::RuntimeInstanceRef,
    pid: Option<u32>,
    now: &str,
) -> Result<StartOutcome, StartFault> {
    let row = AgentRepository::get_instance(store, instance_id)
        .map_err(store_fault)?
        .ok_or_else(|| StartFault::InstanceAbsent(instance_id.as_str().to_string()))?;
    let correlation = correlation_for(store, &row.task_id.clone().expect("task"), &row.id, now)?;
    let event_id = record_started(store, &row, runtime_ref, pid, now, correlation)?;
    Ok(StartOutcome {
        instance_id: row.id.clone(),
        state: AgentInstanceState::Idle,
        runtime_instance_id: Some(runtime_ref.runtime_instance_id.clone()),
        pid,
        workspace_initialized: false,
        event_id,
    })
}

/// The completion leg on a refused Runtime start.
///
/// # Errors
/// [`StartFault::InstanceAbsent`] when the row is gone; store or construction faults whole.
pub fn complete_start_failed(
    store: &mut AthenaStore,
    instance_id: &InstanceId,
    fault: &str,
    now: &str,
) -> Result<StartOutcome, StartFault> {
    let row = AgentRepository::get_instance(store, instance_id)
        .map_err(store_fault)?
        .ok_or_else(|| StartFault::InstanceAbsent(instance_id.as_str().to_string()))?;
    let correlation = correlation_for(store, &row.task_id.clone().expect("task"), &row.id, now)?;
    let event_id = record_failed(store, &row, fault, now, correlation)?;
    Ok(StartOutcome {
        instance_id: row.id.clone(),
        state: AgentInstanceState::Failed,
        runtime_instance_id: None,
        pid: None,
        workspace_initialized: false,
        event_id,
    })
}
//
// --- X-44(b) composition: the resident's one-call start drive ---------------------------
//
// The two rules above hold the store-side faces; this half is the one the resident's
// execution loop calls once per admitted delegation. It owns the composition only —
// acquire, `CREATED → STARTING`, the Runtime's own `start`, and the completion leg the
// outcome names — and it answers the report the caller logs. The store mutex is taken
// twice (never across an `await`: the future must stay `Send`), the Runtime runs with the
// lock released, and every post-`STARTING` refusal lands the failure leg, because a row
// left `STARTING` would sit outside single-flight's own count (ATH-DATA-010).

/// Why the one-call composition refused, before or inside the Runtime's own start.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StartError {
    /// The store walk refused (`StartFault`, whole); the class is the fault's own — a
    /// poisoned mutex surfaces as `Store` (InternalError, no retry, ATH-FAIL-002).
    #[error("{0}")]
    Fault(crate::runtime_instance::StartFault),
    /// The Runtime (or the composition in front of it) refused; carried whole so the
    /// caller's log names the Runtime's own category: `Validation` → ValidationError
    /// (the start-face gate, ATH-AGENT-008), `Deadline` → TimeoutError (ATH-RT-006),
    /// `Transient` → TransientError. A refusal *after* the row walked
    /// `CREATED → STARTING` is already failed by the composition itself; this is the
    /// rest — the §20.1 class mapping is [`StartError::class`].
    #[error("{0}")]
    Runtime(athena_runtime::RuntimeError),
}

impl StartError {
    /// The §20.1 class of this refusal.
    #[must_use]
    pub fn class(&self) -> crate::protocol::ErrorClass {
        match self {
            Self::Fault(fault) => fault.class(),
            Self::Runtime(error) => match error {
                athena_runtime::RuntimeError::UnknownInstance(_)
                | athena_runtime::RuntimeError::Unsupported(_)
                | athena_runtime::RuntimeError::Validation(_) => {
                    crate::protocol::ErrorClass::ValidationError
                }
                athena_runtime::RuntimeError::Deadline(_) => {
                    crate::protocol::ErrorClass::TimeoutError
                }
                athena_runtime::RuntimeError::Transient(_) => {
                    crate::protocol::ErrorClass::TransientError
                }
                athena_runtime::RuntimeError::Failure(_) => {
                    crate::protocol::ErrorClass::RuntimeError
                }
                athena_runtime::RuntimeError::Internal(_) => {
                    crate::protocol::ErrorClass::InternalError
                }
            },
        }
    }

    /// The stable machine code within the class.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Fault(fault) => fault.code(),
            Self::Runtime(_) => "INSTANCE_START_FAILED",
        }
    }
}

/// What one composition answered — the completion leg's outcome, the report the resident
/// logs (`IDLE` on a started instance, `FAILED` on a refused one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartReport {
    /// The completion leg's outcome.
    pub outcome: StartOutcome,
}

/// One lock, poisoned or not (`ATH-PERS-006`'s no-guess face): a poisoned mutex would
/// hand the start face an inconsistent store, so the walk refuses instead of proceeding.
fn lock_the(
    store: &std::sync::Mutex<AthenaStore>,
) -> Result<std::sync::MutexGuard<'_, AthenaStore>, StartFault> {
    store.lock().map_err(|_| {
        StartFault::Store("the store mutex is poisoned; the start face refuses".into())
    })
}

/// Land the failure leg for a row the composition already walked to `STARTING` (the
/// Runtime refused after the walk): `STARTING → FAILED` with `side_effects = Unknown`
/// (`ATH-CORE-048` / `ATH-STATE-029`) — a row left `STARTING` would sit outside
/// single-flight's own count (`ATH-DATA-010`), so the leg lands *before* the refusal
/// surfaces to the caller.
fn fail_started(
    store: &std::sync::Mutex<AthenaStore>,
    begin: &BeginStart,
    detail: &str,
) -> Result<StartOutcome, StartFault> {
    let mut guard = lock_the(store)?;
    let now = athena_persistence::now_rfc3339();
    complete_start_failed(&mut guard, &begin.instance_id, detail, &now)
}

/// One admitted start, composed — the one call the resident's execution loop makes per
/// admitted delegation (X-44(b)). The sequence:
///
/// 1. the walk under the first lock — `CREATED → STARTING` re-verified against the row's
///    current state (single flight, `ATH-DATA-010`), the definition resolved, the
///    `AgentStarting` Event appended on the Task's own chain (§17.2's `AgentStarting` row,
///    requester Runtime Manager);
/// 2. the Runtime's own `start`, the lock released — the future stays `Send` (the
///    discipline the resident's delivery leg keeps); the `StartContext` carries everything
///    the Runtime needs and nothing it must decide (`ATH-ACLI-006`): the workspace, the
///    frozen Skill Set, the permission sets, the mandatory model coordinates
///    (`ATH-AGENT-008`), the deadlines;
/// 3. the completion leg under the second lock — `IDLE` with the row's backfilled
///    `runtime_instance_id` and the §26.6 Runtime Instance row (`ATH-RT-012`'s 1:1), or
///    `FAILED` whose leg lands before the refusal surfaces.
///
/// The `R` generic is the Runtime trait the composition root links (spec §32.2: the
/// binary declares the adapter; `athena-core` never names one). The Runtime kind itself is
/// never a caller argument — `begin_start` reads the definition's own `runtime` fact
/// (`ATH-AGENT-001`). The pid is not guessed here either: `pid_of` asks the composition
/// root what its process table holds for the handle the Runtime just answered with, and
/// a Runtime that reports none records none.
///
/// # Errors
/// [`StartError`] whole; the caller logs and continues — a refused start is a recorded
/// failure leg, not a resident fault.
pub async fn start_admitted<R: Runtime>(
    runtime: &R,
    store: &std::sync::Mutex<AthenaStore>,
    accepted: &crate::delegation::DelegationAccepted,
    config: &crate::Config,
    pid_of: impl Fn(&athena_runtime::RuntimeInstanceRef) -> Option<u32>,
) -> Result<StartReport, StartError> {
    // 1. The walk, under the first lock; the lock is released when this block closes.
    let begin = {
        let mut guard = lock_the(store).map_err(StartError::Fault)?;
        let now = athena_persistence::now_rfc3339();
        begin_start(&mut guard, &accepted.instance_id, &now).map_err(StartError::Fault)?
    };

    // 2. The context the Runtime's start call receives. The work targets and the
    //    read-only conventions are not invented here: the Instance's cwd is the row's own
    //    workspace (`ATH-OPS-009`), the workspace *is* the writable root this start
    //    grants, and the control surface's exclusions stay the workspace layout's own
    //    shape (`init_workspace`). The model rides the definition's mandatory coordinate
    //    (`ATH-AGENT-008`); the deadlines ride the configuration (`ATH-RT-006`).
    let deadlines = athena_types::Deadlines {
        startup_ms: u64::try_from(config.timeout.startup.as_millis()).unwrap_or(30_000),
        execution_ms: u64::try_from(config.timeout.execution.as_millis()).unwrap_or(600_000),
    };
    let context = StartContext {
        task_id: Some(begin.task_id.clone()),
        plan_step_id: Some(begin.step_id.clone()),
        instance_id: begin.instance_id.clone(),
        workspace: begin.workspace.clone(),
        work_targets: vec![],
        persona_ref: begin.definition.persona_ref.clone(),
        persona_version: begin.definition.persona_version.clone(),
        skill_set: begin.skill_set.clone(),
        allowed_tools: begin.definition.allowed_tools.clone(),
        delegable_tools: begin.definition.delegable_tools.clone(),
        filesystem_scope: FilesystemScope {
            workspace: begin.workspace.clone(),
            work_targets: vec![],
            read_only: vec![],
        },
        model: begin.definition.model.clone(),
        deadlines,
        resume_snapshot_ref: None,
        context_document_ref: None,
    };
    if let Err(error) = context.validate() {
        // The Runtime's own gate refuses before any process runs; the row is `STARTING`,
        // so the failure leg lands first (`ATH-STATE-007`'s shape), then the refusal
        // surfaces with the Runtime's own class.
        let detail = error.to_string();
        fail_started(store, &begin, &detail).map_err(StartError::Fault)?;
        return Err(StartError::Runtime(error));
    }
    match runtime.start(begin.definition.clone(), context).await {
        Ok(handle) => {
            // 3. The happy completion leg, under the second lock: `IDLE`, the row's
            //    runtime id backfilled once (the 1:1, `ATH-RT-012`), the Runtime Instance
            //    row seeded, the `AI_LOG` area reference recorded (X-41).
            let mut guard = lock_the(store).map_err(StartError::Fault)?;
            let now = athena_persistence::now_rfc3339();
            // The pid is the Runtime's own fact, asked of the composition root that owns
            // the process table (D-35's split). It matters beyond observability: the boot
            // reconciliation of a *later* resident has no process table, so the recorded
            // pid is the only evidence that this process ever existed (ATH-CORE-048).
            let pid = pid_of(&handle);
            let outcome =
                complete_start_started(&mut guard, &begin.instance_id, &handle, pid, &now)
                    .map_err(StartError::Fault)?;
            Ok(StartReport { outcome })
        }
        Err(error) => {
            let detail = error.to_string();
            fail_started(store, &begin, &detail).map_err(StartError::Fault)?;
            Err(StartError::Runtime(error))
        }
    }
}
