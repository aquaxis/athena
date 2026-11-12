//! X-44(b)'s tests: the Runtime Manager's start face — the §17.2 walk, the completion
//! legs, and the refusals.
//!
//! The Runtime is a deterministic double (the `tests/core_agent_boot.rs` pattern): a real
//! process is unreachable in this environment, so the double answers the two outcomes the
//! legs are written for and the store's own rows carry the verdict. Nothing here asserts a
//! live process; every claim is a row or an Event the walk wrote.

use crate::delegation::{DelegationAccepted, DelegationRequest, accept_delegation};
use crate::runtime_instance::{StartError, StartFault, begin_start, start_admitted};
use athena_persistence::repository::{AgentRepository, EventRepository, RuntimeInstanceRepository};
use athena_runtime::{Runtime, RuntimeError, RuntimeInstanceRef};
use athena_types::{
    AgentDefinition, AgentId, AgentInstanceState, AgentLevel, InstanceId, ModelRef, PlanStepDraft,
    RuntimeInstanceId, RuntimeType, SideEffects, Task, TaskId, TaskState,
};
use std::sync::Mutex;

const NOW: &str = "2026-11-12T03:00:00.000Z";
const INSTANCES_ROOT: &str = "/repo/.athena/var/instances";
const AGENT: &str = "x44-worker";

/// The Runtime double: it answers one scripted outcome and records the context it saw, so
/// the test can assert what the composition handed it (never a mocked store).
struct ScriptedRuntime {
    outcome: Mutex<Option<Result<RuntimeInstanceRef, RuntimeError>>>,
    seen: Mutex<Option<athena_runtime::StartContext>>,
}

impl ScriptedRuntime {
    fn started(instance: &InstanceId) -> (Self, RuntimeInstanceId) {
        let runtime_instance_id = RuntimeInstanceId::new();
        let handle = RuntimeInstanceRef {
            runtime_type: RuntimeType::AgentCli,
            runtime_instance_id: runtime_instance_id.clone(),
            instance_id: instance.clone(),
        };
        (
            Self {
                outcome: Mutex::new(Some(Ok(handle))),
                seen: Mutex::new(None),
            },
            runtime_instance_id,
        )
    }

    fn refusing(error: RuntimeError) -> Self {
        Self {
            outcome: Mutex::new(Some(Err(error))),
            seen: Mutex::new(None),
        }
    }

    fn context(&self) -> Option<athena_runtime::StartContext> {
        self.seen.lock().expect("seen").clone()
    }
}

#[async_trait::async_trait]
impl Runtime for ScriptedRuntime {
    fn runtime_type(&self) -> RuntimeType {
        RuntimeType::AgentCli
    }

    async fn start(
        &self,
        _definition: AgentDefinition,
        ctx: athena_runtime::StartContext,
    ) -> Result<RuntimeInstanceRef, RuntimeError> {
        *self.seen.lock().expect("seen") = Some(ctx);
        self.outcome
            .lock()
            .expect("outcome")
            .take()
            .unwrap_or_else(|| Err(RuntimeError::UnknownInstance("script exhausted".into())))
    }

    async fn send(
        &self,
        _instance: &RuntimeInstanceRef,
        _message: athena_runtime::message::Message,
    ) -> Result<(), RuntimeError> {
        Ok(())
    }

    fn receive(
        &self,
        _instance: &RuntimeInstanceRef,
    ) -> futures::stream::BoxStream<'static, athena_runtime::message::Message> {
        Box::pin(futures::stream::empty::<athena_runtime::message::Message>())
    }

    async fn status(
        &self,
        _instance: &RuntimeInstanceRef,
    ) -> Result<athena_runtime::status::RuntimeInstanceStatus, RuntimeError> {
        Ok(athena_runtime::status::RuntimeInstanceStatus::Running)
    }

    async fn stop(
        &self,
        _instance: &RuntimeInstanceRef,
        _reason: athena_types::StopReason,
    ) -> Result<(), RuntimeError> {
        Ok(())
    }

    async fn collect_result(
        &self,
        _instance: &RuntimeInstanceRef,
    ) -> Result<athena_types::ExecutionResult, RuntimeError> {
        Err(RuntimeError::UnknownInstance("not collected".into()))
    }

    fn capabilities(&self) -> athena_runtime::status::RuntimeCapabilities {
        athena_runtime::status::RuntimeCapabilities {
            runtime_type: self.runtime_type(),
            isolation: athena_runtime::status::IsolationBoundary::Process,
            supported_tools: vec![],
            max_concurrent_instances: 4,
        }
    }
}

fn definition(model: ModelRef) -> AgentDefinition {
    AgentDefinition {
        id: AgentId::new(AGENT),
        name: "X44 Worker".into(),
        level: AgentLevel::Worker,
        description: "the definition the start face reads".into(),
        capabilities: vec![],
        persona_ref: "worker.md".into(),
        persona_version: "1.0.0".into(),
        allowed_tools: vec![athena_types::ToolName::FsRead],
        delegable_tools: vec![],
        default_skills: vec![],
        runtime: RuntimeType::AgentCli,
        model,
        max_delegation_depth: 4,
        dynamic: false,
    }
}

fn model() -> ModelRef {
    ModelRef {
        provider: "anthropic".into(),
        name: "claude-opus-5".into(),
    }
}

fn task_row() -> Task {
    Task {
        id: TaskId::new(),
        parent_id: None,
        title: "x44b".into(),
        instruction: "start the admitted instance".into(),
        workspace: "/repo/x44b".into(),
        goal: None,
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

/// One admitted delegation: the Task, the plan's own step, the definition, and the
/// `CREATED` Instance row the intake wrote (the real admission path, never a hand-built
/// row).
fn admitted(model: ModelRef) -> (crate::AthenaStore, Task, DelegationAccepted) {
    let mut store = crate::AthenaStore::in_memory().expect("store");
    let task = task_row();
    crate::TaskRepository::insert(&mut store, &task).expect("task row");
    AgentRepository::upsert_definition(&mut store, &definition(model)).expect("definition");
    let accepted_plan = crate::plan_intake::accept_plan(
        &mut store,
        &task.id,
        0,
        vec![PlanStepDraft {
            order: 1,
            intent: "the step to start".into(),
            depends_on_orders: vec![],
            assigned_agent: Some(AgentId::new(AGENT)),
            skill_set: vec![],
        }],
        NOW,
    )
    .expect("plan lands");
    let plan = crate::PlanRepository::get(&store, &accepted_plan.plan_id)
        .expect("read")
        .expect("plan");
    let step_id = plan.steps.first().expect("one step").id.clone();
    let request = DelegationRequest {
        task_id: task.id.clone(),
        plan_step_id: step_id.clone(),
        agent_id: AgentId::new(AGENT),
        skill_set: vec![athena_types::SkillRef {
            id: athena_types::SkillId::new("pcb-review"),
            version: "1.0.0".into(),
            checksum: None,
        }],
        idempotency_key: crate::protocol::idempotency_key(&step_id, 1),
    };
    let accepted = accept_delegation(
        &mut store,
        &request,
        std::path::Path::new(INSTANCES_ROOT),
        NOW,
    )
    .expect("admitted");
    (store, task, accepted)
}

/// Every Event type the store holds for one Task, in sequence order.
fn event_types(store: &crate::AthenaStore, task: &TaskId) -> Vec<String> {
    EventRepository::list_for_task(store, task)
        .expect("events")
        .into_iter()
        .map(|event| event.event_type)
        .collect()
}

#[tokio::test]
async fn a_started_instance_walks_to_idle_and_records_its_runtime_facts() {
    let (store, task, accepted) = admitted(model());
    let (runtime, runtime_instance_id) = ScriptedRuntime::started(&accepted.instance_id);
    let store = Mutex::new(store);
    let config = crate::Config::default();

    let report = start_admitted(&runtime, &store, &accepted, &config)
        .await
        .expect("started");

    assert_eq!(report.outcome.state, AgentInstanceState::Idle);
    assert!(report.outcome.is_started());
    assert_eq!(
        report.outcome.runtime_instance_id.as_ref(),
        Some(&runtime_instance_id)
    );

    let guard = store.lock().expect("store");
    let row = AgentRepository::get_instance(&*guard, &accepted.instance_id)
        .expect("read")
        .expect("row");
    // The row's own facts: the state the table named, the backing process's id, and the
    // `AI_LOG` area under the workspace the Runtime entered (never a guess).
    assert_eq!(row.state, AgentInstanceState::Idle);
    assert_eq!(row.runtime_instance_id.as_ref(), Some(&runtime_instance_id));
    assert_eq!(
        row.aiprj_log_ref.as_deref(),
        Some(format!("{}/.aiprj/AI_LOG", accepted.workspace).as_str())
    );

    // The §26.6 Runtime Instance row is seeded, 1:1 with the Agent Instance (ATH-RT-012).
    let runtime_row = RuntimeInstanceRepository::get(&*guard, &runtime_instance_id)
        .expect("read")
        .expect("runtime instance row");
    assert_eq!(runtime_row.instance_id, accepted.instance_id);
    assert_eq!(runtime_row.runtime_type, RuntimeType::AgentCli);
    assert_eq!(runtime_row.state, AgentInstanceState::Idle);
    assert_eq!(runtime_row.side_effects, SideEffects::None);

    // Both legs' Events ride the Task's own chain, in the table's order.
    let types = event_types(&guard, &task.id);
    let starting = types
        .iter()
        .position(|name| name == "AgentStarting")
        .expect("AgentStarting appended");
    let started = types
        .iter()
        .position(|name| name == "AgentStarted")
        .expect("AgentStarted appended");
    assert!(
        starting < started,
        "the walk's order is the table's: {types:?}"
    );

    let chains: Vec<String> = EventRepository::list_for_task(&*guard, &task.id)
        .expect("events")
        .into_iter()
        .map(|event| event.correlation_id)
        .collect();
    assert!(
        chains.windows(2).all(|pair| pair[0] == pair[1]),
        "the start joins the Task's existing chain: {chains:?}"
    );
}

#[tokio::test]
async fn the_context_the_runtime_receives_carries_the_rows_own_facts() {
    let (store, task, accepted) = admitted(model());
    let (runtime, _) = ScriptedRuntime::started(&accepted.instance_id);
    let store = Mutex::new(store);

    start_admitted(&runtime, &store, &accepted, &crate::Config::default())
        .await
        .expect("started");

    let context = runtime.context().expect("the Runtime was called");
    // Task-bound (unlike instance #0's own context), rooted in the row's workspace, and
    // carrying the frozen Skill Set — never a recomputed one (ATH-DATA-006).
    assert_eq!(context.task_id.as_ref(), Some(&task.id));
    assert_eq!(context.instance_id, accepted.instance_id);
    assert_eq!(context.workspace, accepted.workspace);
    assert_eq!(context.filesystem_scope.workspace, accepted.workspace);
    assert_eq!(context.skill_set.len(), 1);
    assert_eq!(context.skill_set[0].id.as_str(), "pcb-review");
    assert_eq!(context.model, model());
    assert_eq!(context.persona_ref, "worker.md");
}

#[tokio::test]
async fn a_refused_runtime_start_lands_the_failure_leg_before_it_surfaces() {
    let (store, task, accepted) = admitted(model());
    let runtime = ScriptedRuntime::refusing(RuntimeError::Failure("exit 139".into()));
    let store = Mutex::new(store);

    let error = start_admitted(&runtime, &store, &accepted, &crate::Config::default())
        .await
        .expect_err("the Runtime refused");
    assert!(matches!(error, StartError::Runtime(_)), "{error}");
    assert_eq!(error.class().wire(), "runtime_error");

    let guard = store.lock().expect("store");
    let row = AgentRepository::get_instance(&*guard, &accepted.instance_id)
        .expect("read")
        .expect("row");
    // A row left `STARTING` would sit outside single-flight's count, so the leg lands:
    // `FAILED`, with no runtime id to record.
    assert_eq!(row.state, AgentInstanceState::Failed);
    assert!(row.runtime_instance_id.is_none());

    let types = event_types(&guard, &task.id);
    assert!(types.contains(&"AgentStarting".to_string()), "{types:?}");
    assert!(types.contains(&"AgentFailed".to_string()), "{types:?}");
    assert!(
        !types.contains(&"AgentStarted".to_string()),
        "a refused start never answers started: {types:?}"
    );

    // The payload states the class, the attempt and the undetermined side effects
    // (ATH-CORE-048 / ATH-STATE-029).
    let failed = EventRepository::list_for_task(&*guard, &task.id)
        .expect("events")
        .into_iter()
        .find(|event| event.event_type == "AgentFailed")
        .expect("the failure Event");
    let payload: serde_json::Value =
        serde_json::from_str(&failed.payload_json).expect("payload json");
    assert_eq!(payload["error_class"], "runtime_error");
    assert_eq!(payload["attempt"], 1);
    assert_eq!(payload["side_effects"], "unknown");
}

#[tokio::test]
async fn a_definition_without_model_coordinates_never_reaches_the_runtime() {
    // The start-face gate (ATH-AGENT-008): the context's own validation refuses before a
    // process exists, and the row is left `FAILED` rather than `STARTING`.
    let (store, _task, accepted) = admitted(ModelRef {
        provider: String::new(),
        name: String::new(),
    });
    let (runtime, _) = ScriptedRuntime::started(&accepted.instance_id);
    let store = Mutex::new(store);

    let error = start_admitted(&runtime, &store, &accepted, &crate::Config::default())
        .await
        .expect_err("refused by the gate");
    assert_eq!(error.class().wire(), "validation_error");
    assert!(
        runtime.context().is_none(),
        "the Runtime must never be called with an invalid context"
    );

    let guard = store.lock().expect("store");
    let row = AgentRepository::get_instance(&*guard, &accepted.instance_id)
        .expect("read")
        .expect("row");
    assert_eq!(row.state, AgentInstanceState::Failed);
}

#[tokio::test]
async fn a_second_start_of_one_instance_is_refused_by_the_tables_own_rule() {
    let (store, _task, accepted) = admitted(model());
    let (runtime, _) = ScriptedRuntime::started(&accepted.instance_id);
    let store = Mutex::new(store);
    let config = crate::Config::default();

    start_admitted(&runtime, &store, &accepted, &config)
        .await
        .expect("started");
    // The row is `IDLE` now; `CREATED → STARTING` is the only edge the face walks, so the
    // re-entry refuses on the row's current state (single flight across admission→start).
    let error = start_admitted(&runtime, &store, &accepted, &config)
        .await
        .expect_err("the second start is refused");
    assert_eq!(error.class().wire(), "policy_error");
    assert_eq!(error.code(), "INSTANCE_TRANSITION_REFUSED");
}

#[test]
fn the_start_face_refuses_an_absent_instance() {
    let mut store = crate::AthenaStore::in_memory().expect("store");
    let fault = begin_start(&mut store, &InstanceId::new(), NOW).expect_err("absent");
    assert!(matches!(fault, StartFault::InstanceAbsent(_)), "{fault}");
    assert_eq!(fault.code(), "INSTANCE_NOT_FOUND");
    assert_eq!(fault.class().wire(), "validation_error");
}

#[test]
fn the_starting_leg_walks_the_row_and_appends_the_tables_own_event() {
    let (mut store, task, accepted) = admitted(model());
    let begin = begin_start(&mut store, &accepted.instance_id, NOW).expect("the STARTING leg");
    // What the caller needs for the Runtime call comes from the store, not from itself:
    // the definition's own Runtime kind and the row's frozen Skill Set.
    assert_eq!(begin.runtime_type, RuntimeType::AgentCli);
    assert_eq!(begin.task_id, task.id);
    assert_eq!(begin.workspace, accepted.workspace);
    assert_eq!(begin.attempt, 1);

    let row = AgentRepository::get_instance(&store, &accepted.instance_id)
        .expect("read")
        .expect("row");
    assert_eq!(row.state, AgentInstanceState::Starting);
    assert!(
        event_types(&store, &task.id).contains(&"AgentStarting".to_string()),
        "the edge's own Event is appended with the row write (two writes, one leg)"
    );
}
