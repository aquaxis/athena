//! Core Agent Session behaviour (ATH-CORE-021…046).

use crate::core_agent_session::{
    ContextDocument, ControlSurfaceChecksum, Recovery, RestartWindow, StartFailureKind,
    heartbeat_event, recovery_decision, restarted_event, started_event, verify_surface,
};
use athena_events::EventContext;
use athena_events::envelope::Actor;
use athena_persistence::db::now_rfc3339;
use athena_persistence::repository::{
    AgentRepository, ApprovalRepository, EventRepository, MemoryRepository, PlanRepository,
    TaskRepository,
};
use athena_persistence::{AthenaStore, NewEvent};
use athena_runtime::context::{FilesystemScope, StartContext};
use athena_runtime::interface::{Runtime, boxed_stream};
use athena_runtime::{RuntimeError, RuntimeInstanceRef};
use athena_types::{
    AgentDefinition, AgentId, AgentInstance, AgentInstanceState, Approval, ApprovalId,
    ApprovalScope, InstanceId, MemoryId, MemoryOrigin, MemoryRecord, MemoryTier, ModelRef,
    PlanStepId, PlanStepState, Priority, RiskLevel, RuntimeInstanceId, RuntimeType, Task, TaskId,
    ToolName,
};

// --- P1-31: the control-surface checksum -------------------------------------------------

fn surface_entries() -> Vec<(&'static str, &'static [u8])> {
    vec![
        (
            ".agent-cli/config.toml",
            b"[runtime]\ncommands_dir = \"\\.agent-cli/commands\"",
        ),
        (".agent-cli/persona.md", b"# producer"),
        (".agent-cli/commands/next.md", b"read task, act, log"),
        ("skills/workflow/SKILL.md", b"ordering"),
    ]
}

#[test]
fn the_checksum_is_order_independent_and_binds_content() {
    // ATH-CORE-044: placement records one digest; the same set agrees however the tree is
    // walked; any content change disagrees.
    let forward = ControlSurfaceChecksum::of(&surface_entries());
    let mut reversed = surface_entries();
    reversed.reverse();
    let backward = ControlSurfaceChecksum::of(&reversed);
    assert_eq!(
        forward.as_str(),
        backward.as_str(),
        "walk order is irrelevant"
    );
    assert_eq!(forward.as_str().len(), 64, "SHA-256 hex length");

    let altered = ControlSurfaceChecksum::of(&[
        (
            ".agent-cli/config.toml",
            b"[runtime]\ncommands_dir = \"\\.agent-cli/commands\"",
        ),
        (".agent-cli/persona.md", b"# producer - rewritten"),
        (".agent-cli/commands/next.md", b"read task, act, log"),
        ("skills/workflow/SKILL.md", b"ordering"),
    ]);
    assert_ne!(forward.as_str(), altered.as_str());
}

#[test]
fn a_mismatched_surface_escalates_without_touching_the_window() {
    // ATH-CORE-044: the §7.5.3 order is the ban — a restart would run a tampered agent.
    let placed = ControlSurfaceChecksum::of(&surface_entries());
    let present = ControlSurfaceChecksum::of(&[(
        ".agent-cli/config.toml",
        b"[runtime]\ncommands_dir = \"\\.agent-cli/commands\"",
    )]);
    let mismatch = verify_surface(&placed, &present).expect_err("the surface changed");

    let mut window = RestartWindow::default_window();
    let decision = recovery_decision(Err(mismatch), StartFailureKind::Counted, &mut window, 0);
    match decision {
        Recovery::Escalate(reason) => assert!(reason.contains("control surface"), "{reason}"),
        other => panic!("a tampered surface never restarts: {other:?}"),
    }
    // The budget spent nothing: the refusal is the checksum, not a counted failure.
    assert_eq!(window.counted(10_000), 0);
}

#[test]
fn an_intact_surface_verifies_and_the_restart_proceeds() {
    let placed = ControlSurfaceChecksum::of(&surface_entries());
    let live = ControlSurfaceChecksum::of(&surface_entries());
    verify_surface(&placed, &live).expect("intact");

    let mut window = RestartWindow::default_window();
    let decision = recovery_decision(Ok(()), StartFailureKind::Counted, &mut window, 1_000);
    assert!(
        matches!(decision, Recovery::Restart { attempt: 1 }),
        "{decision:?}"
    );
}

// --- P1-32: the restart window -----------------------------------------------------------

#[test]
fn three_counted_failures_exhaust_the_window() {
    // ATH-CORE-045 / ATH-CORE-030: the 4th counted failure inside the hour escalates
    // instead of allowing a fourth restart.
    let mut window = RestartWindow::default_window();
    assert_eq!(
        recovery_decision(Ok(()), StartFailureKind::Counted, &mut window, 1_000),
        Recovery::Restart { attempt: 1 }
    );
    assert_eq!(
        recovery_decision(Ok(()), StartFailureKind::Counted, &mut window, 2_000),
        Recovery::Restart { attempt: 2 }
    );
    assert_eq!(
        recovery_decision(Ok(()), StartFailureKind::Counted, &mut window, 3_000),
        Recovery::Restart { attempt: 3 }
    );
    let fourth = recovery_decision(Ok(()), StartFailureKind::Counted, &mut window, 4_000);
    assert!(
        matches!(&fourth, Recovery::Escalate(reason) if reason.contains("exhausted")),
        "{fourth:?}"
    );
}

#[test]
fn transient_failures_consume_no_budget() {
    // ATH-CORE-045: the two excluded kinds never count, so they cannot manufacture an
    // escalation by themselves.
    let mut window = RestartWindow::default_window();
    for at in 0..100 {
        let decision = recovery_decision(Ok(()), StartFailureKind::Transient, &mut window, at);
        assert!(
            matches!(decision, Recovery::RetryNotCounted),
            "{decision:?}"
        );
    }
    assert_eq!(window.counted(100), 0);
    let decision = recovery_decision(Ok(()), StartFailureKind::ResourceShortage, &mut window, 101);
    assert!(matches!(decision, Recovery::RetryNotCounted));
}

#[test]
fn the_window_slides_so_old_counts_drop_off() {
    let mut window = RestartWindow::default_window();
    for at in 0..3 {
        recovery_decision(Ok(()), StartFailureKind::Counted, &mut window, at);
    }
    assert!(window.exhausted(4_000), "three counts inside the hour");
    // The budget is per sliding hour, not one sequence: an hour later it restarts at 1.
    assert!(
        matches!(
            recovery_decision(Ok(()), StartFailureKind::Counted, &mut window, 3_601_000),
            Recovery::Restart { attempt: 1 }
        ),
        "the old attempts slid out of the window"
    );
}

#[test]
fn runtime_failure_kinds_map_to_the_count_rule() {
    // ATH-CORE-045: only the prescribed exclusions pass uncounted.
    assert_eq!(
        StartFailureKind::from_runtime(&RuntimeError::Transient("socket reset".into())),
        StartFailureKind::Transient
    );
    for counted in [
        RuntimeError::Failure("exit 139".into()),
        RuntimeError::Deadline("startup window".into()),
        RuntimeError::Internal("inconsistent".into()),
    ] {
        assert_eq!(
            StartFailureKind::from_runtime(&counted),
            StartFailureKind::Counted
        );
    }
}

// --- P1-30: the boot refuses a Task-bound instance #0 ------------------------------------

/// A Runtime the boot path must never call with a Task-bound context.
#[derive(Default)]
struct ProbeRuntime {
    called: std::sync::atomic::AtomicBool,
}

impl ProbeRuntime {
    fn was_called(&self) -> bool {
        self.called.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl Runtime for ProbeRuntime {
    fn runtime_type(&self) -> RuntimeType {
        RuntimeType::LocalProcess
    }

    async fn start(
        &self,
        _definition: AgentDefinition,
        _ctx: athena_runtime::StartContext,
    ) -> Result<RuntimeInstanceRef, RuntimeError> {
        self.called.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(RuntimeInstanceRef {
            runtime_type: self.runtime_type(),
            runtime_instance_id: RuntimeInstanceId::new(),
            instance_id: InstanceId::new(),
        })
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
        boxed_stream(futures::stream::empty::<athena_runtime::message::Message>())
    }

    async fn status(
        &self,
        _instance: &RuntimeInstanceRef,
    ) -> Result<athena_runtime::status::RuntimeInstanceStatus, RuntimeError> {
        Ok(athena_runtime::status::RuntimeInstanceStatus::Stopped { exit_code: Some(0) })
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
        Err(RuntimeError::UnknownInstance("not started".into()))
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

fn session_context() -> StartContext {
    StartContext {
        task_id: None,
        plan_step_id: None,
        instance_id: InstanceId::new(),
        workspace: "/athena/var/core-agent".into(),
        work_targets: vec![],
        persona_ref: "producer".into(),
        persona_version: "1.0.0".into(),
        skill_set: vec![],
        allowed_tools: vec![ToolName::FsRead, ToolName::SendTo],
        delegable_tools: vec![],
        filesystem_scope: FilesystemScope::default(),
        model: ModelRef {
            provider: "claude".into(),
            name: "claude-opus-5".into(),
        },
        deadlines: athena_types::Deadlines {
            startup_ms: 10_000,
            execution_ms: 600_000,
        },
        resume_snapshot_ref: None,
        context_document_ref: None,
    }
}

#[test]
fn the_boot_refuses_a_task_bound_context_and_starts_the_session() {
    let probe = ProbeRuntime::default();
    let mut bound = session_context();
    bound.task_id = Some(TaskId::new());

    // The Task-bound one is refused before the Runtime sees it (ATH-CORE-020).
    let forbidden = futures::executor::block_on(crate::core_agent_session::start_core_agent(
        &probe,
        the_definition(),
        bound,
    ))
    .expect_err("refused");
    assert!(
        matches!(forbidden, RuntimeError::Validation(_)),
        "{forbidden}"
    );
    assert!(
        !probe.was_called(),
        "the Runtime never observed the forbidden start"
    );

    // The session-scoped one reaches the Runtime through the interface.
    let granted = futures::executor::block_on(crate::core_agent_session::start_core_agent(
        &probe,
        the_definition(),
        session_context(),
    ))
    .expect("the Core Agent boots");
    assert!(probe.was_called());
    assert!(granted.runtime_instance_id.as_str().starts_with("ri-"));
}

fn worker_definition() -> AgentDefinition {
    // The worker-level definition the `seed_instance` rows reference (ATH-AGENT-002's
    // parent record): a static, minimal definition with the minimal default tools.
    AgentDefinition {
        id: AgentId::new("rtl-worker"),
        name: "RTL Worker".into(),
        level: athena_types::AgentLevel::Worker,
        description: "the worker the reconciliation seeds reference".into(),
        capabilities: vec![],
        persona_ref: "worker".into(),
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

fn the_definition() -> AgentDefinition {
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

#[test]
fn the_core_agent_events_carry_their_payload_and_type() {
    // ATH-CORE-031: `CoreAgentStarted` / `Restarted` / `Heartbeat` / `Escalated` are typed
    // catalog Events (D-3).
    let context = EventContext {
        event_id: athena_types::EventId::new(),
        occurred_at: now_rfc3339(),
        actor: Actor::CoreAgent,
        task_id: None,
        plan_step_id: None,
        agent_id: Some(AgentId::core_agent()),
        instance_id: None,
        skill_id: None,
        correlation_id: "cor-boot".into(),
        causation_id: None,
    };
    let started = started_event(context.clone()).expect("start");
    assert_eq!(started.event_type, "CoreAgentStarted");
    let restarted =
        restarted_event(1, "2026-10-14T00:00:00Z", "ctx-doc-1", context.clone()).expect("restart");
    assert_eq!(restarted.event_type, "CoreAgentRestarted");
    assert_eq!(restarted.payload.attempt, 1);
    let beat = heartbeat_event(2, Some(TaskId::new()), 3, context.clone()).expect("beat");
    assert_eq!(beat.event_type, "CoreAgentHeartbeat");
    assert_eq!(beat.payload.pending_requests, 3);
    let escalated = crate::core_agent_session::escalated_event(context).expect("escalate");
    assert_eq!(escalated.event_type, "CoreAgentEscalated");
}

// --- P1-32: the context document rebuild -------------------------------------------------

#[test]
fn the_rebuilt_context_document_carries_every_memory_007_row() {
    // ATH-MEM-007 / ATH-CORE-046: the Working Memory minimum must be reconstructible from
    // the store: plan + revision, incomplete steps, unterminated delegations, reports by
    // reference, pending + granted approvals, denials, `SUSPENDED` snapshots.
    let mut store = AthenaStore::in_memory().expect("store");
    let task = seed_task(&mut store);
    let [done_step, running_step] = seed_plan(&mut store, &task);
    seed_instance(
        &mut store,
        &task,
        Some(&running_step),
        AgentInstanceState::Waiting,
    );
    let suspended = seed_instance(
        &mut store,
        &task,
        Some(&done_step),
        AgentInstanceState::Suspended,
    );
    seed_snapshot(&mut store, &suspended);
    let report = seed_report_event(&mut store, &task);
    seed_denial_event(&mut store, &task);
    seed_approvals(&mut store, &task);

    let document = ContextDocument::rebuild(&mut store, &task).expect("rebuild");
    assert!(document.plan.is_some(), "the Plan with its revision");
    assert_eq!(
        document.incomplete_steps.len(),
        1,
        "the incomplete PlanStep"
    );
    assert!(
        document
            .delegations
            .iter()
            .any(|(step, agent, _, state)| step.is_some()
                && agent == "rtl-worker"
                && state == "WAITING"),
        "the unterminated delegation is present: {:?}",
        document.delegations
    );
    assert!(
        document.latest_report_refs.contains(&report),
        "the latest report is referenced: {:?}",
        document.latest_report_refs
    );
    assert!(
        document.recent_denials.iter().any(
            |denial| denial.request_kind == "execute_tool" && denial.policy_id == "agent.grant"
        ),
        "the recent denial is present"
    );
    assert_eq!(document.pending_approvals.len(), 1);
    assert_eq!(document.granted_unconsumed.len(), 1);
    assert_eq!(document.suspended_snapshots.len(), 1, "the snapshot ref");
    // The re-injection payload carries what the restarted agent can act on.
    assert!(document.to_json().contains("rtl-worker"));
}

// --- Fixtures ----------------------------------------------------------------------------

fn seed_task(store: &mut AthenaStore) -> TaskId {
    let task = Task {
        id: TaskId::new(),
        parent_id: None,
        title: "Recovery".into(),
        instruction: "NFR-14".into(),
        workspace: "/work".into(),
        goal: None,
        constraints: vec![],
        completion_criteria: vec![],
        state: athena_types::TaskState::Running,
        priority: Priority::Normal,
        plan_id: None,
        risk_level: RiskLevel::Medium,
        attempt_count: 0,
        max_attempts: 3,
        iteration_count: 0,
        max_iterations: 50,
        deadline: None,
        created_by: "test".into(),
        created_at: now_rfc3339(),
        updated_at: now_rfc3339(),
        finished_at: None,
    };
    TaskRepository::insert(store, &task).expect("task seeds");
    task.id
}

fn step(
    plan_id: &athena_types::PlanId,
    order: u64,
    state: PlanStepState,
) -> athena_types::PlanStep {
    athena_types::PlanStep {
        id: PlanStepId::new(),
        plan_id: plan_id.clone(),
        order,
        intent: "work".into(),
        depends_on: vec![],
        assigned_agent: Some(AgentId::new("rtl-worker")),
        skill_set: vec![],
        state,
        result_id: None,
    }
}

fn seed_plan(store: &mut AthenaStore, task: &TaskId) -> [PlanStepId; 2] {
    let plan_id = athena_types::PlanId::new();
    let plan = athena_types::Plan {
        id: plan_id.clone(),
        task_id: task.clone(),
        revision: 1,
        goal: "resume".into(),
        steps: vec![
            step(&plan_id, 0, PlanStepState::Success),
            step(&plan_id, 1, PlanStepState::Running),
        ],
        created_at: now_rfc3339(),
        superseded_by: None,
    };
    PlanRepository::insert(store, &plan).expect("plan seeds");
    [plan.steps[0].id.clone(), plan.steps[1].id.clone()]
}

fn seed_instance(
    store: &mut AthenaStore,
    task: &TaskId,
    plan_step: Option<&PlanStepId>,
    state: AgentInstanceState,
) -> InstanceId {
    // The definition row precedes the instance (the store FK chain, ATH-AGENT-002) — and
    // the definition seeded is the one the instance's own `agent_id` names.
    AgentRepository::upsert_definition(store, &worker_definition()).expect("definition seeds");
    let instance = AgentInstance {
        id: InstanceId::new(),
        agent_id: AgentId::new("rtl-worker"),
        task_id: Some(task.clone()),
        plan_step_id: plan_step.cloned(),
        superior_instance_id: None,
        delegation_depth: 0,
        state,
        runtime_instance_id: Some(RuntimeInstanceId::new()),
        workspace: "/work/instances/ai-1".into(),
        skill_set: vec![],
        aiprj_log_ref: None,
        attempt: 0,
        started_at: Some(now_rfc3339()),
        finished_at: None,
    };
    AgentRepository::insert_instance(store, &instance).expect("instance seeds");
    instance.id
}

fn seed_snapshot(store: &mut AthenaStore, instance: &InstanceId) {
    let record = MemoryRecord {
        id: MemoryId::new(),
        tier: MemoryTier::Working,
        scope_id: instance.as_str().into(),
        content: "short term memory digest".into(),
        summary: Some("frozen at suspend".into()),
        origin: MemoryOrigin {
            task_id: None,
            event_id: None,
        },
        created_at: now_rfc3339(),
        expires_at: None,
    };
    MemoryRepository::insert(store, &record).expect("snapshot seeds");
}

fn seed_report_event(store: &mut AthenaStore, task: &TaskId) -> (String, String) {
    let reporter = InstanceId::new();
    let reporter_return = reporter.clone();
    let event = NewEvent {
        event_id: athena_types::EventId::new(),
        schema_version: 1,
        event_type: "AgentMessageReceived".into(),
        occurred_at: now_rfc3339(),
        task_id: Some(task.clone()),
        plan_step_id: None,
        agent_id: Some(AgentId::core_agent()),
        instance_id: Some(reporter),
        skill_id: None,
        correlation_id: "cor-recovery".into(),
        causation_id: None,
        actor: "agent".into(),
        payload_json: serde_json::json!({
            "message_id": "msg-report-1",
            "type": "report",
            "thread_id": "th-1"
        })
        .to_string(),
    };
    EventRepository::append(store, &event).expect("append");
    (reporter_return.as_str().into(), "msg-report-1".into())
}

fn seed_denial_event(store: &mut AthenaStore, task: &TaskId) {
    let event = NewEvent {
        event_id: athena_types::EventId::new(),
        schema_version: 1,
        event_type: "PrivilegedRequestDenied".into(),
        occurred_at: now_rfc3339(),
        task_id: Some(task.clone()),
        plan_step_id: None,
        agent_id: Some(AgentId::core_agent()),
        instance_id: None,
        skill_id: None,
        correlation_id: "cor-recovery".into(),
        causation_id: None,
        actor: "system".into(),
        payload_json: serde_json::json!({
            "request_kind": "execute_tool",
            "error_class": "permission_error",
            "policy_id": "agent.grant"
        })
        .to_string(),
    };
    EventRepository::append(store, &event).expect("append");
}

fn seed_approvals(store: &mut AthenaStore, task: &TaskId) {
    let pending = Approval {
        id: ApprovalId::new(),
        task_id: task.clone(),
        requested_by: AgentId::core_agent(),
        operation: "fs_write:/project/src/lib.rs".into(),
        reason: "needed".into(),
        impact: "rewrite".into(),
        risk_level: RiskLevel::High,
        state: athena_types::ApprovalState::Requested,
        operation_descriptor: "fs_write".into(),
        requested_by_instance_id: InstanceId::new(),
        uses_remaining: 1,
        expires_at: now_rfc3339(),
        responded_by: None,
        responded_at: None,
        scope: ApprovalScope::OperationOnly {
            operation_only: true,
        },
    };
    ApprovalRepository::insert(store, &pending).expect("pending approval seeds");
    let granted = Approval {
        id: ApprovalId::new(),
        task_id: task.clone(),
        requested_by: AgentId::core_agent(),
        operation: "shell:run-build".into(),
        reason: "approval exists".into(),
        impact: "build artifacts change".into(),
        risk_level: RiskLevel::High,
        state: athena_types::ApprovalState::Granted,
        operation_descriptor: "shell".into(),
        requested_by_instance_id: InstanceId::new(),
        uses_remaining: 1,
        expires_at: now_rfc3339(),
        responded_by: Some("human".into()),
        responded_at: Some(now_rfc3339()),
        scope: ApprovalScope::OperationOnly {
            operation_only: true,
        },
    };
    ApprovalRepository::insert(store, &granted).expect("granted approval seeds");
}

// --- P1-33: the boot reconciliation ------------------------------------------------------

#[test]
fn the_reconciliation_finalizes_process_less_rows_and_orphans_their_step() {
    // ATH-CORE-048: a non-terminal row without a process is `FAILED` with unknown side
    // effects, and its `RUNNING` PlanStep is `ORPHANED` — every write through the exact
    // §17.2 / §17.5 trigger, in one transaction.
    let mut store = AthenaStore::in_memory().expect("store");
    let task = seed_task(&mut store);
    let [done_step, running_step] = seed_plan(&mut store, &task);
    let process_less = seed_instance(
        &mut store,
        &task,
        Some(&running_step),
        AgentInstanceState::Busy,
    );

    let decisions = crate::core_agent_session::reconcile_boot_states(
        &mut store,
        &task,
        |_instance| false, // the probe reports every row its process lost
        &now_rfc3339(),
        "cor-1",
    )
    .expect("reconciled");
    assert_eq!(decisions.len(), 1);
    assert!(
        matches!(
            &decisions[0],
            crate::core_agent_session::Reconciliation::Finalized { instance, step }
                if *instance == process_less && step.as_ref() == Some(&running_step)
        ),
        "{decisions:?}"
    );

    // The store's row *is* the assertion: state and step agree.
    let row = AgentRepository::get_instance(&store, &process_less)
        .expect("fetch")
        .expect("row");
    assert_eq!(row.state, AgentInstanceState::Failed);
    let step_row = PlanRepository::get_step(&store, &running_step)
        .expect("fetch")
        .expect("step");
    assert_eq!(step_row.state, PlanStepState::Orphaned);

    // The Events the transaction wrote: `AgentFailed` (side effects unknown) and its
    // cause, `PlanStepOrphaned`.
    let events = EventRepository::list_for_task(&store, &task).expect("events");
    let names: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
    assert!(
        names.contains(&"AgentFailed") && names.contains(&"PlanStepOrphaned"),
        "{names:?}"
    );
    let orphan = events
        .iter()
        .find(|event| event.event_type == "PlanStepOrphaned")
        .expect("orphan event");
    assert!(
        orphan.payload_json.contains("unknown"),
        "{}",
        orphan.payload_json
    );
    assert_eq!(
        orphan.plan_step_id.as_ref().map(PlanStepId::as_str),
        Some(running_step.as_str())
    );

    // A live process keeps the row alive — the reconcile never touches it.
    let live = seed_instance(
        &mut store,
        &task,
        Some(&done_step),
        AgentInstanceState::Idle,
    );
    let _ = live;
    let decisions = crate::core_agent_session::reconcile_boot_states(
        &mut store,
        &task,
        |instance| instance.id == live,
        &now_rfc3339(),
        "cor-2",
    )
    .expect("reconciled");
    // The BUSY row was already finalized; only rows the probe refuses get finalized —
    // and no row is left to manufacture a second event (the append is atomic per row).
    assert!(
        decisions.iter().all(|decision| matches!(
            decision,
            crate::core_agent_session::Reconciliation::Kept { .. }
        )) || decisions.is_empty(),
        "{decisions:?}"
    );
}

#[test]
fn waiting_and_suspended_rows_are_kept_not_finalized() {
    // ATH-STATE-019: a `WAITING` / `SUSPENDED` row *legitimately* holds no process —
    // time out the instance, and the reconciliation must not call it a failure.
    let mut store = AthenaStore::in_memory().expect("store");
    let task = seed_task(&mut store);
    let [_, running_step] = seed_plan(&mut store, &task);
    seed_instance(
        &mut store,
        &task,
        Some(&running_step),
        AgentInstanceState::Waiting,
    );
    seed_instance(
        &mut store,
        &task,
        Some(&running_step),
        AgentInstanceState::Suspended,
    );

    let decisions = crate::core_agent_session::reconcile_boot_states(
        &mut store,
        &task,
        |_instance| false,
        &now_rfc3339(),
        "cor-3",
    )
    .expect("reconciled");
    assert!(
        decisions.iter().all(|decision| matches!(
            decision,
            crate::core_agent_session::Reconciliation::Kept { .. }
        ),),
        "{decisions:?}"
    );
}

// --- P1-34: the heartbeat lease ----------------------------------------------------------

#[test]
fn the_lease_marks_a_lapse_past_the_60_second_default() {
    // ATH-CORE-047: a heartbeat lapse of 60 s is the point of self-termination; the lease
    // tracker records when the receipts stop arriving.
    use crate::core_agent_session::HeartbeatLease;
    let mut lease = HeartbeatLease::default_lease();
    let instance = InstanceId::new();
    lease.renew(&instance, 1_000);
    assert!(!lease.has_lapsed(&instance, 60_999));
    assert!(lease.has_lapsed(&instance, 61_000));
    // Renewing resets the horizon.
    lease.renew(&instance, 65_000);
    assert!(!lease.has_lapsed(&instance, 120_000));
    assert!(lease.has_lapsed(&instance, 125_001));
    // The lapsed set lists the oldest lapse first; `later` renews later in time, so it
    // has 55 s of health left at the same instant and stays out.
    let later = InstanceId::new();
    lease.renew(&later, 70_000);
    assert_eq!(lease.lapsed(125_005), vec![instance.clone()]);
    // Thirty more seconds lapse the second one too.
    assert_eq!(lease.lapsed(130_001), vec![instance.clone(), later.clone()]);
    // Forgetting the finalized entries is the only removal — no other store effect.
    lease.forget(&instance);
    lease.forget(&later);
    assert!(lease.lapsed(130_002).is_empty());
}

#[test]
fn the_heartbeat_event_also_feeds_the_absence_detector() {
    // ATH-OBS-004: `CoreAgentHeartbeat` carries the iteration so "no Event for N s" is
    // detectable even while the loop simply waits.
    let context = EventContext {
        event_id: athena_types::EventId::new(),
        occurred_at: now_rfc3339(),
        actor: Actor::CoreAgent,
        task_id: None,
        plan_step_id: None,
        agent_id: Some(AgentId::core_agent()),
        instance_id: None,
        skill_id: None,
        correlation_id: "cor-beat".into(),
        causation_id: None,
    };
    let beat = heartbeat_event(7, Some(TaskId::new()), 1, context).expect("beat");
    assert_eq!(beat.payload.iteration, 7);
    assert!(beat.payload.focus_task_id.is_some());
}
