//! Adapter + generation + workspace behaviour (Phase 3's first slice).

use crate::adapter::{AgentCliAdapter, ProcessDriver, SpawnedProcess, command_for};
use crate::workspace::{init_workspace, workspace_layout};
use athena_runtime::{Runtime, RuntimeError};
use athena_types::StopReason;
use athena_types::{
    AgentDefinition, AgentId, AgentLevel, Deadlines, InstanceId, MessageId, MessageType, ModelRef,
    Priority, ResultStatus, RuntimeType, TaskId, ToolName,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

fn definition() -> AgentDefinition {
    AgentDefinition {
        id: AgentId::new("rtl-worker"),
        name: "Worker".into(),
        level: AgentLevel::Worker,
        description: "x".into(),
        capabilities: vec![],
        persona_ref: "speaker".into(),
        persona_version: "1.0.0".into(),
        allowed_tools: vec![ToolName::FsRead],
        delegable_tools: vec![],
        default_skills: vec![],
        runtime: RuntimeType::AgentCli,
        model: ModelRef {
            provider: "claude".into(),
            name: "claude-opus-5".into(),
        },
        max_delegation_depth: 2,
        dynamic: false,
    }
}

fn ctx_with(workspace: PathBuf) -> athena_runtime::StartContext {
    athena_runtime::StartContext {
        task_id: Some(TaskId::new()),
        plan_step_id: None,
        instance_id: InstanceId::new(),
        workspace: workspace.display().to_string(),
        work_targets: vec!["/repo/src".into()],
        persona_ref: "workers/test".into(),
        persona_version: "1.0.0".into(),
        skill_set: vec![],
        allowed_tools: vec![ToolName::FsRead],
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

/// Shared-state driver double: clonable so one instance can be handed to both the
/// manager and the assertions.
#[derive(Default, Clone)]
struct FakeDriver {
    counter: std::sync::Arc<AtomicU64>,
    /// Pids reaped as terminal (graceful or forced exit).
    dead: DeadSet,
    /// Pids this driver actually spawned (an unknown pid is not alive).
    known: DeadSet,
    /// Every `(workspace, command)` the driver was asked to spawn (the argv line's
    /// assertion surface).
    spawned: SpawnedSet,
}

#[derive(Clone, Default)]
struct DeadSet(std::sync::Arc<std::sync::Mutex<Vec<u32>>>);

type SpawnTrace = (PathBuf, String);

#[derive(Clone, Default)]
struct SpawnedSet(std::sync::Arc<std::sync::Mutex<Vec<SpawnTrace>>>);

impl DeadSet {
    fn contains(&self, pid: u32) -> bool {
        self.0.lock().expect("dead").contains(&pid)
    }

    fn insert(&self, pid: u32) {
        self.0.lock().expect("dead").push(pid);
    }
}

impl SpawnedSet {
    fn push(&self, workspace: PathBuf, command: String) {
        self.0.lock().expect("spawned").push((workspace, command));
    }

    fn last(&self) -> SpawnTrace {
        self.0
            .lock()
            .expect("spawned")
            .last()
            .cloned()
            .expect("a spawn happened")
    }
}

/// The driver double that ignores the graceful signal (the SIGKILL escalation case).
#[derive(Default, Clone)]
struct StubbornDriver(FakeDriver);

impl ProcessDriver for FakeDriver {
    fn spawn(
        &self,
        workspace: &std::path::Path,
        command: &str,
    ) -> Result<SpawnedProcess, RuntimeError> {
        std::fs::create_dir_all(workspace).ok();
        self.spawned
            .push(workspace.to_path_buf(), command.to_string());
        let pid =
            u32::try_from(self.counter.fetch_add(1, Ordering::SeqCst) + 1000).unwrap_or(u32::MAX);
        self.known.insert(pid);
        Ok(SpawnedProcess {
            pid,
            runtime_instance_id: athena_types::RuntimeInstanceId::new(),
            peer_id: None,
        })
    }

    fn is_alive(&self, pid: u32) -> bool {
        self.known.contains(pid) && !self.dead.contains(pid)
    }

    fn exit_code(&self, _pid: u32) -> Option<i32> {
        None
    }

    fn terminate(&self, pid: u32) -> Result<(), RuntimeError> {
        self.dead.insert(pid);
        Ok(())
    }

    fn force_kill(&self, pid: u32) -> Result<(), RuntimeError> {
        self.dead.insert(pid);
        Ok(())
    }
}

impl ProcessDriver for StubbornDriver {
    fn spawn(
        &self,
        workspace: &std::path::Path,
        command: &str,
    ) -> Result<SpawnedProcess, RuntimeError> {
        self.0.spawn(workspace, command)
    }

    fn is_alive(&self, pid: u32) -> bool {
        self.0.is_alive(pid)
    }

    fn exit_code(&self, pid: u32) -> Option<i32> {
        self.0.exit_code(pid)
    }

    /// The graceful signal is ignored: the process survives it, which is what makes the
    /// `timeout.stop` escalation the only end (ATH-FAIL-015).
    fn terminate(&self, _pid: u32) -> Result<(), RuntimeError> {
        Ok(())
    }

    fn force_kill(&self, pid: u32) -> Result<(), RuntimeError> {
        self.0.dead.insert(pid);
        Ok(())
    }
}

fn adapter() -> (AgentCliAdapter<FakeDriver>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp");
    let adapter = AgentCliAdapter::new(
        FakeDriver::default(),
        dir.path().join("template"),
        4,
        16,
        vec![],
    );
    (adapter, dir)
}

#[tokio::test]
async fn a_start_builds_the_workspace_writes_areas_and_the_generated_agent_cli() {
    // P3-05/06/09/11 in one admission: read side absent, write side present.
    let (adapter, base) = adapter();
    let workspace = base.path().join("instances").join("ai-1");
    let handle = Runtime::start(&adapter, definition(), ctx_with(workspace.clone()))
        .await
        .expect("started");
    assert!(handle.runtime_instance_id.as_str().starts_with("ri-"));

    let layout = workspace_layout(&workspace);
    assert!(layout.aiprj.is_dir());
    assert!(layout.tasks_file.exists());
    assert!(layout.logs_dir.is_dir());
    // The read side (instructions.md / rules/) is NEVER copied in (ATH-AIPRJ-025).
    assert!(!layout.aiprj.join("instructions.md").exists());
    assert!(!layout.aiprj.join("AI_PRJ_REQUIREMENTS.md").exists());

    // The generated `.agent-cli/`: only what agent-cli reads (ATH-ACLI-015).
    let config = std::fs::read_to_string(layout.agent_cli.join("config.toml")).expect("config");
    assert!(config.contains("[spawn]"), "{config}");
    assert!(
        config.contains("max_depth = 4"),
        "the ceiling floors agent-cli's 2: {config}"
    );
    assert!(config.contains("[history]\nenabled = true"));
    assert!(config.contains("kind = \"claude\""));
    assert!(
        layout.agent_cli.join("commands").is_dir() || !layout.agent_cli.join("commands").exists(),
        "commands copied into the generated overlay"
    );
    assert!(layout.agent_cli.join("persona.md").exists());
}

#[tokio::test]
async fn the_adapter_tracks_liveness_and_stop() {
    let (adapter, base) = adapter();
    let workspace = base.path().join("instances").join("ai-1");
    let handle = Runtime::start(&adapter, definition(), ctx_with(workspace))
        .await
        .expect("started");
    let status = adapter.status(&handle).await.expect("status");
    // Within the startup window the instance is `Starting` (ATH-RT-006), never
    // pre-declared Running.
    assert!(matches!(
        status,
        athena_runtime::RuntimeInstanceStatus::Starting
    ));
    Runtime::stop(&adapter, &handle, StopReason::Shutdown)
        .await
        .expect("stopped");
}

#[tokio::test]
async fn sending_persists_the_delegation_into_the_workspace_inbox() {
    // The IPC handoff lands with the process rows; this skeleton's contract records the
    // delivery in the Workspace inbox (the fact both sides record from). The template is
    // seeded with the aiprj operating-model procedures so the delegation envelope can
    // direct them (P3-17).
    let dir = tempfile::tempdir().expect("temp");
    let driver = FakeDriver::default();
    let template = dir.path().join("template");
    let commands = template.join("commands");
    std::fs::create_dir_all(&commands).expect("template commands");
    for name in ["ai.md", "next_ai.md", "close_ai.md"] {
        std::fs::write(commands.join(name), format!("# {name} (origin copy)\n"))
            .expect("procedure file");
    }
    let adapter = AgentCliAdapter::new(driver.clone(), template, 4, 16, vec![]);
    let workspace = dir.path().join("instances").join("ai-1");
    let handle = Runtime::start(&adapter, definition(), ctx_with(workspace.clone()))
        .await
        .expect("started");
    let instruction = "produce the report";
    Runtime::send(
        &adapter,
        &handle,
        athena_runtime::message::Message {
            message_id: MessageId::new(),
            message_type: MessageType::Instruction,
            from_agent: Some(AgentId::core_agent()),
            from_instance_id: None,
            thread_id: "th-1".into(),
            reply_to: None,
            priority: Priority::Normal,
            body: instruction.into(),
            payload_json: None,
            deadline: None,
        },
    )
    .await
    .expect("delivered to the inbox");
    let entries: Vec<_> = std::fs::read_dir(workspace.join(".athena-delegations"))
        .expect("inbox")
        .flatten()
        .collect();
    assert_eq!(entries.len(), 1);
    // P3-17: the delivered body carries the operating-model direction with the absolute
    // procedure paths, and never a bare slash command (ATH-ACLI-016).
    let text = std::fs::read_to_string(entries[0].path()).expect("inbox entry");
    assert!(
        text.contains("produce the report"),
        "the instruction reaches the agent: {text}"
    );
    assert!(
        text.contains(
            &workspace
                .join(".agent-cli/commands/ai.md")
                .display()
                .to_string()
        ),
        "the /ai procedure is directed by absolute path: {text}"
    );
    assert!(
        text.contains(
            &workspace
                .join(".agent-cli/commands/close_ai.md")
                .display()
                .to_string()
        ),
        "the /close_ai procedure is directed: {text}"
    );
    assert!(
        !text.contains("\"/ai\"") && !text.contains("invoke \"/ai\""),
        "the envelope directs, it never sends a slash command"
    );
}

#[test]
fn the_command_names_the_definition_and_the_files() {
    // Design §5.3's spawn line: cwd = workspace, the overlay supplying the definition.
    let command = command_for(&definition());
    assert!(command.contains("persona.md"));
    assert!(command.contains("rtl-worker"));
}

#[test]
fn the_workspace_init_refuses_an_overwrite_without_permission() {
    // ATH-AIPRJ-006: re-running never discards a live write area.
    let dir = tempfile::tempdir().expect("temp");
    let root = dir.path().join("ai-1");
    init_workspace(&root, false).expect("first");
    std::fs::write(
        workspace_layout(&root).tasks_file,
        "the agent's own task records",
    )
    .expect("the agent wrote");
    assert!(matches!(
        init_workspace(&root, false),
        Err(crate::workspace::WorkspaceFault::Exists(_))
    ));
}

// --- Process Manager (P3-02…P3-04; design §5.4) ------------------------------------------

use crate::manager::{Budgets, Clock, ProcessManager, Record};
use std::time::Duration;

/// One controllable instant (the deterministic-clock surface).
#[derive(Clone)]
struct FixedClock(std::sync::Arc<std::sync::Mutex<std::time::SystemTime>>);

impl FixedClock {
    fn at_past(seconds: u64) -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(
            std::time::SystemTime::now() - Duration::from_secs(seconds),
        )))
    }

    fn advance(&self, by: Duration) {
        *self.0.lock().expect("clock") += by;
    }
}

impl Clock for FixedClock {
    fn now(&self) -> std::time::SystemTime {
        *self.0.lock().expect("clock")
    }
}

fn budgets(second: u64) -> Budgets {
    Budgets::new(
        Duration::from_secs(second),
        Duration::from_secs(120),
        Duration::from_millis(150),
        Duration::from_secs(second * 2),
    )
}

fn manager_with(second: u64) -> (ProcessManager<FakeDriver>, FakeDriver, FixedClock) {
    let driver = FakeDriver::default();
    let clock = FixedClock::at_past(0);
    let manager = ProcessManager::new(
        driver.clone(),
        budgets(second),
        std::sync::Arc::new(clock.clone()),
    );
    (manager, driver, clock)
}

#[tokio::test]
async fn the_startup_window_reports_starting_then_running() {
    let (manager, _driver, clock) = manager_with(10);
    let dir = tempfile::tempdir().expect("temp");
    let handle = manager
        .spawn(&definition(), &ctx_with(dir.path().join("ai-1")))
        .expect("spawned");
    let state = manager
        .status(handle.runtime_instance_id.as_str())
        .await
        .expect("status");
    assert!(matches!(
        state,
        athena_runtime::RuntimeInstanceStatus::Starting
    ));
    clock.advance(Duration::from_secs(11));
    let state = manager
        .status(handle.runtime_instance_id.as_str())
        .await
        .expect("status");
    assert!(matches!(
        state,
        athena_runtime::RuntimeInstanceStatus::Running
    ));
}

#[tokio::test]
async fn an_execution_deadline_marks_the_instance_failed() {
    let (manager, _driver, clock) = manager_with(1);
    let dir = tempfile::tempdir().expect("temp");
    let handle = manager
        .spawn(&definition(), &ctx_with(dir.path().join("ai-1")))
        .expect("spawned");
    clock.advance(Duration::from_secs(200)); // past startup (1s) and execution (120s)
    let state = manager
        .status(handle.runtime_instance_id.as_str())
        .await
        .expect("status");
    let athena_runtime::RuntimeInstanceStatus::Failed { detail } = state else {
        panic!("expected the execution-deadline failure, got {state:?}");
    };
    assert!(detail.contains("execution deadline"), "{detail}");
}

#[tokio::test]
async fn a_lost_heartbeat_fails_the_supervision_contract() {
    let (manager, _driver, clock) = manager_with(1);
    let dir = tempfile::tempdir().expect("temp");
    let handle = manager
        .spawn(&definition(), &ctx_with(dir.path().join("ai-1")))
        .expect("spawned");
    clock.advance(Duration::from_secs(11)); // past startup, inside execution (10s)
    manager.beat(handle.runtime_instance_id.as_str()); // activity resets the clock
    clock.advance(Duration::from_secs(60)); // silence past the heartbeat budget (2s)
    let state = manager
        .status(handle.runtime_instance_id.as_str())
        .await
        .expect("status");
    let athena_runtime::RuntimeInstanceStatus::Failed { detail } = state else {
        panic!("expected the heartbeat failure, got {state:?}");
    };
    assert!(detail.contains("heartbeat lost"), "{detail}");
}

#[tokio::test]
async fn a_graceful_stop_releases_the_record_and_reports_afterward() {
    let (manager, _driver, _clock) = manager_with(1);
    let dir = tempfile::tempdir().expect("temp");
    let handle = manager
        .spawn(&definition(), &ctx_with(dir.path().join("ai-1")))
        .expect("spawned");
    manager
        .stop(handle.runtime_instance_id.as_str(), StopReason::Shutdown)
        .await
        .expect("stopped");
    assert!(
        manager.is_empty(),
        "the terminal record is released (ATH-RT-010)"
    );
    // Later probes report, never fabricate (the shared contract's rule).
    assert!(matches!(
        manager.status(handle.runtime_instance_id.as_str()).await,
        Err(RuntimeError::UnknownInstance(_))
    ));
}

#[tokio::test]
async fn a_stubborn_process_is_force_killed_after_the_stop_budget() {
    let clock = FixedClock::at_past(0);
    let driver = StubbornDriver(FakeDriver::default());
    let manager = ProcessManager::new(
        driver.clone(),
        budgets(1),
        std::sync::Arc::new(clock.clone()),
    );
    let dir = tempfile::tempdir().expect("temp");
    let handle = manager
        .spawn(&definition(), &ctx_with(dir.path().join("ai-1")))
        .expect("spawned");
    let pid = manager
        .record(handle.runtime_instance_id.as_str())
        .expect("record")
        .pid;
    // The injected clock is the only time source, so the test drives it while the stop
    // loop polls (the escalation budget elapses in simulated time).
    let clock_for_loop = clock.clone();
    tokio::spawn(async move {
        for _ in 0..200 {
            tokio::time::sleep(Duration::from_millis(5)).await;
            clock_for_loop.advance(Duration::from_millis(10));
        }
    });
    manager
        .stop(handle.runtime_instance_id.as_str(), StopReason::Shutdown)
        .await
        .expect("stopped through the escalation");
    assert!(!driver.0.is_alive(pid), "the SIGKILL landed");
    assert!(manager.is_empty());
}

#[tokio::test]
async fn reconciliation_reattaches_the_living_and_releases_the_dead() {
    let (manager, _driver, clock) = manager_with(1);
    let dir = tempfile::tempdir().expect("temp");
    let alive = manager
        .spawn(&definition(), &ctx_with(dir.path().join("ai-1")))
        .expect("spawned");
    let mut carried = vec![
        manager
            .record(alive.runtime_instance_id.as_str())
            .expect("alive record"),
    ];
    // A second record whose process never lived: the reconcile must release it, not
    // resurrect it (ATH-RT-011).
    carried.push(dead_record(&manager));

    clock.advance(Duration::from_secs(2));
    let report = manager.reconcile(carried).expect("reconciled");
    assert_eq!(report.reattached, vec![alive.runtime_instance_id.clone()]);
    assert_eq!(report.released.len(), 1, "the dead one was released");
    assert!(matches!(
        manager.status(alive.runtime_instance_id.as_str()).await,
        Ok(athena_runtime::RuntimeInstanceStatus::Running
            | athena_runtime::RuntimeInstanceStatus::Starting)
    ));
}

/// A fabricated carried record whose pid the driver knows nothing of (dead).
fn dead_record(_manager: &ProcessManager<FakeDriver>) -> Record {
    Record {
        pid: 4_000_000,
        runtime_instance_id: athena_types::RuntimeInstanceId::new(),
        instance_id: InstanceId::new(),
        workspace: PathBuf::from("/nonexistent"),
        definition: definition(),
        started_at: std::time::SystemTime::now(),
        last_beat: std::time::SystemTime::now(),
        is_core_agent: false,
        workspace_initialized: false,
        task_id: None,
        plan_step_id: None,
        peer_id: None,
    }
}

/// P3-10/P3-13: the placed persona carries the work targets and procedure files by
/// absolute path (ATH-ACLI-017, ATH-OPS-010), and the `commands/` copy is verbatim from an
/// absolute-read origin.
#[tokio::test]
async fn start_places_the_persona_with_absolute_coordinates() {
    let (adapter, base) = adapter();
    let template = base.path().join("template");
    std::fs::create_dir_all(template.join("commands")).expect("template commands");
    // The `athena init` origin shape: absolute reads, absolute work target.
    std::fs::write(
        template.join("commands").join("ai.md"),
        format!("@{}/.aiprj/rules/exec_job.md", base.path().display()),
    )
    .expect("template ai.md");

    let workspace = base.path().join("instances").join("ai-1");
    Runtime::start(&adapter, definition(), ctx_with(workspace.clone()))
        .await
        .expect("started");

    let persona =
        std::fs::read_to_string(workspace.join(".agent-cli").join("persona.md")).expect("persona");
    assert!(
        persona.contains("## Work targets (absolute)\n- /repo/src\n"),
        "work targets are delivered by absolute path: {persona}"
    );
    assert!(
        persona.contains(&format!(
            "## Procedure files (read by absolute path)\n- {}",
            workspace
                .join(".agent-cli")
                .join("commands")
                .join("ai.md")
                .display()
        )),
        "the persona points at the procedure file by absolute path (ATH-ACLI-017): {persona}"
    );
    // The copied command file is verbatim from the absolute-read origin.
    let copied =
        std::fs::read_to_string(workspace.join(".agent-cli").join("commands").join("ai.md"))
            .expect("copied command");
    assert!(
        copied.contains(&format!(
            "@{}/.aiprj/rules/exec_job.md",
            base.path().display()
        )) && !copied.contains("@.aiprj/"),
        "the per-Instance copy carries the absolute read: {copied}"
    );
}

/// P3-10: an origin that still carries the upstream cwd-relative `@.aiprj/` read is
/// refused at generation — it could only resolve against an Instance's cwd, which would
/// resolve the convention against the wrong root (ATH-AIPRJ-028).
#[tokio::test]
async fn start_refuses_a_cwd_relative_read_in_the_template() {
    let (adapter, base) = adapter();
    let template = base.path().join("template");
    std::fs::create_dir_all(template.join("commands")).expect("template commands");
    std::fs::write(
        template.join("commands").join("ai.md"),
        "@.aiprj/rules/exec_job.md",
    )
    .expect("raw upstream wrapper");

    let workspace = base.path().join("instances").join("ai-1");
    let error = Runtime::start(&adapter, definition(), ctx_with(workspace))
        .await
        .expect_err("the raw upstream origin must be refused");
    assert!(
        matches!(error, RuntimeError::Validation(ref text) if text.contains("ATH-AIPRJ-028")),
        "the refusal names the procedure-file requirement: {error}"
    );
}

/// P3-13: the generation refuses a cwd-relative work target outright (belt-and-braces over
/// `StartContext::validate`).
#[test]
fn render_persona_refuses_a_relative_work_target() {
    let error = crate::generation::render_persona("body", &["relative/target".into()], &[])
        .expect_err("a relative target must be refused at generation (P3-13, ATH-OPS-010)");
    assert!(
        error.to_string().contains("not an absolute path"),
        "{error}"
    );
}

// --- P3-15: the Core Agent migrates to `agent-cli` #0 ------------------------------------

fn core_definition() -> AgentDefinition {
    AgentDefinition {
        id: AgentId::core_agent(),
        name: "Core Agent".into(),
        level: AgentLevel::Producer,
        description: "producer persona".into(),
        capabilities: vec![],
        persona_ref: "producer".into(),
        persona_version: "1.0.0".into(),
        allowed_tools: vec![ToolName::FsRead, ToolName::FsWrite, ToolName::SendTo],
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

/// The Core Agent Session's boot context: session-scoped (no Task bound, ATH-CORE-020)
/// with its cwd at `<home>/var/core-agent/` (the dedicated Workspace, ATH-OPS-009).
fn core_context(home: &std::path::Path) -> athena_runtime::StartContext {
    athena_runtime::StartContext {
        task_id: None,
        plan_step_id: None,
        instance_id: InstanceId::new(),
        workspace: crate::workspace::core_agent_workspace(home)
            .display()
            .to_string(),
        work_targets: vec![],
        persona_ref: "producer".into(),
        persona_version: "1.0.0".into(),
        skill_set: vec![],
        allowed_tools: vec![ToolName::FsRead, ToolName::FsWrite, ToolName::SendTo],
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

#[tokio::test]
async fn the_core_agent_boots_resident_on_spawn_with_a_dedicated_workspace() {
    let dir = tempfile::tempdir().expect("temp");
    let driver = FakeDriver::default();
    let adapter = AgentCliAdapter::new(driver.clone(), dir.path().join("template"), 4, 16, vec![]);
    let home = dir.path().join(".athena");
    let workspace = crate::workspace::core_agent_workspace(&home);

    let handle = Runtime::start(&adapter, core_definition(), core_context(&home))
        .await
        .expect("instance #0 boots");

    // The spawn line: the driver was invoked with cwd = the Core Agent Workspace and the
    // `spawn` subcommand — the resident, IPC-resolvable form (ATH-ACLI-008), not `run`
    // (ATH-ACLI-016: the REPL offers no non-interactive prompt).
    let (spawned_workspace, command) = driver.spawned.last();
    assert_eq!(spawned_workspace, workspace);
    let mut words = command.split_whitespace();
    assert_eq!(words.next(), Some("agent-cli"));
    assert_eq!(
        words.next(),
        Some("spawn"),
        "instance #0 runs the spawn mode: {command}"
    );

    // The dedicated Workspace, never a subordinate's nor `<PROJECT_ROOT>` (ATH-OPS-009).
    assert!(workspace.ends_with(std::path::Path::new("var").join("core-agent").as_path()));

    // The role is tracked, segregated from the subordinate record set.
    let core = adapter.core_record().expect("instance #0 tracked");
    assert!(core.is_core_agent);
    assert_eq!(core.runtime_instance_id, handle.runtime_instance_id);
    assert!(
        adapter.subordinate_records().is_empty(),
        "instance #0's record never appears in the subordinate flow"
    );
}

#[tokio::test]
async fn a_second_core_start_is_refused_while_instance_zero_is_resident() {
    let dir = tempfile::tempdir().expect("temp");
    let driver = FakeDriver::default();
    let adapter = AgentCliAdapter::new(driver.clone(), dir.path().join("template"), 4, 16, vec![]);
    let home = dir.path().join(".athena");

    Runtime::start(&adapter, core_definition(), core_context(&home))
        .await
        .expect("first residency");

    let error = Runtime::start(&adapter, core_definition(), core_context(&home))
        .await
        .expect_err("instance #0 is unique (ATH-RT-012, 1:1)");
    assert!(matches!(error, RuntimeError::Failure(_)), "{error}");
}

#[tokio::test]
async fn a_dead_core_start_is_dropped_on_restart_and_subordinates_are_untouched() {
    let dir = tempfile::tempdir().expect("temp");
    let driver = FakeDriver::default();
    let adapter = AgentCliAdapter::new(driver.clone(), dir.path().join("template"), 4, 16, vec![]);
    let home = dir.path().join(".athena");

    let core = Runtime::start(&adapter, core_definition(), core_context(&home))
        .await
        .expect("boots");
    let sub_workspace = dir.path().join("instances").join("ai-1");
    let subordinate = Runtime::start(&adapter, definition(), ctx_with(sub_workspace.clone()))
        .await
        .expect("subordinate boots");

    // The Core Agent process dies on its own (crash, kill -9): the record is the only
    // fact, and isolation is by construction — the subordinate's record keeps its pid,
    // its workspace, and its heartbeat facts untouched.
    let crashed_pid = adapter.core_record().expect("tracked").pid;
    driver.dead.insert(crashed_pid);

    let sub_record = adapter
        .process_record(subordinate.runtime_instance_id.as_str())
        .expect("subordinate still tracked");
    assert_eq!(
        sub_record.instance_id, subordinate.instance_id,
        "the subordinate's identity never moved"
    );
    assert_eq!(sub_record.workspace, sub_workspace);
    assert_eq!(adapter.subordinate_records().len(), 1);

    // §7.5.3's restart (the Core Host decides through its window): a stale core record
    // cannot stand in the way — the new start drops it and brings up a fresh process.
    let restart = Runtime::start(&adapter, core_definition(), core_context(&home))
        .await
        .expect("the restart brings instance #0 back");
    assert_ne!(
        restart.runtime_instance_id, core.runtime_instance_id,
        "a restarted instance #0 is a fresh Runtime Instance"
    );
    // The records: the new core only, the subordinate unchanged.
    let core_now = adapter.core_record().expect("new residency");
    assert_eq!(core_now.runtime_instance_id, restart.runtime_instance_id);
    assert!(
        adapter
            .process_record(core.runtime_instance_id.as_str())
            .is_none(),
        "the stale record was dropped with the restart"
    );
    assert_eq!(adapter.subordinate_records().len(), 1);
}

// --- P3-16: aiprj readiness before every start -------------------------------------------

#[test]
fn readiness_reports_uninitialized_ready_and_repairable_states() {
    let dir = tempfile::tempdir().expect("temp");
    let root = dir.path().join("ws");

    // Nothing on disk: uninitialized.
    assert_eq!(
        crate::workspace::aiprj_readiness(&root).expect("stat ok"),
        crate::workspace::AiprjReadiness::Uninitialized
    );

    // After prepare: ready — and a re-prepare must not touch the areas.
    let first = crate::workspace::prepare_workspace(&root, true).expect("prepared");
    assert!(first.initialized, "the fresh areas initialize once");
    assert_eq!(
        first.readiness_before,
        crate::workspace::AiprjReadiness::Uninitialized
    );
    let second = crate::workspace::prepare_workspace(&root, true).expect("prepared");
    assert!(!second.initialized, "a Ready area is never re-initialized");
    assert_eq!(
        second.readiness_before,
        crate::workspace::AiprjReadiness::Ready
    );

    // Incomplete: the areas exist, `AI_LOG/` deleted — the init fills only the missing.
    std::fs::remove_dir_all(root.join(".aiprj").join("AI_LOG")).expect("removed");
    let repaired = crate::workspace::prepare_workspace(&root, true).expect("repaired");
    assert!(repaired.initialized);
    assert_eq!(
        repaired.readiness_before,
        crate::workspace::AiprjReadiness::Incomplete {
            missing: vec!["AI_LOG/"],
        },
    );
    assert!(root.join(".aiprj").join("AI_LOG").is_dir());
}

#[tokio::test]
async fn the_start_initializes_only_when_uninitialized_and_records_the_fact() {
    let (adapter, base) = adapter();
    let workspace = base.path().join("instances").join("ai-1");

    // First start on a fresh workspace: the init ran, the record carries the fact.
    let first = Runtime::start(&adapter, definition(), ctx_with(workspace.clone()))
        .await
        .expect("started");
    assert!(
        adapter
            .process_record(first.runtime_instance_id.as_str())
            .expect("tracked")
            .workspace_initialized,
        "a fresh workspace initializes"
    );

    // A start over the now-Ready areas touches nothing: the fact is false.
    let second = Runtime::start(&adapter, definition(), ctx_with(workspace.clone()))
        .await
        .expect("started");
    assert!(
        !adapter
            .process_record(second.runtime_instance_id.as_str())
            .expect("tracked")
            .workspace_initialized,
        "a Ready workspace is not re-initialized (ATH-AIPRJ-006)"
    );
}

#[tokio::test]
async fn a_workspace_carrying_aiprj_read_side_material_refuses_the_start() {
    let (adapter, _base) = adapter();
    let workspace = _base.path().join("instances").join("ai-bad");
    let aiprj = workspace.join(".aiprj");
    std::fs::create_dir_all(&aiprj).expect("aiprj dir");
    std::fs::write(
        aiprj.join("instructions.md"),
        "someone copied the read side",
    )
    .expect("seeded contamination");

    let error = Runtime::start(&adapter, definition(), ctx_with(workspace))
        .await
        .expect_err("the read side must never sit in an Instance area (ATH-AIPRJ-027)");
    assert!(
        matches!(error, RuntimeError::Validation(ref text)
            if text.contains("ATH-AIPRJ-027") && text.contains("instructions")),
        "{error}"
    );
}

#[tokio::test]
async fn the_core_agent_residency_carries_the_workspace_fact_too() {
    let dir = tempfile::tempdir().expect("temp");
    let driver = FakeDriver::default();
    let adapter = AgentCliAdapter::new(driver.clone(), dir.path().join("template"), 4, 16, vec![]);
    let home = dir.path().join(".athena");
    Runtime::start(&adapter, core_definition(), core_context(&home))
        .await
        .expect("instance #0 boots");
    assert!(
        adapter
            .core_record()
            .expect("tracked")
            .workspace_initialized,
        "the Core Agent Workspace initializes on first residency"
    );
    // And disinherits never: a restart over the Ready areas leaves the fact clean.
    let pid = adapter.core_record().expect("tracked").pid;
    driver.dead.insert(pid);
    let restart = Runtime::start(&adapter, core_definition(), core_context(&home))
        .await
        .expect("restart");
    assert!(
        !adapter
            .process_record(restart.runtime_instance_id.as_str())
            .expect("tracked")
            .workspace_initialized,
        "the restart found the areas Ready and initialized nothing"
    );
}

// --- P3-17: the operating-model mapping by procedure-file direction -----------------------

#[test]
fn the_envelope_directs_the_placed_procedures_by_absolute_path() {
    let dir = tempfile::tempdir().expect("temp");
    let workspace = dir.path().join("ws");
    let commands = workspace.join(".agent-cli").join("commands");
    std::fs::create_dir_all(&commands).expect("commands dir");
    for name in ["ai.md", "next_ai.md", "close_ai.md"] {
        std::fs::write(commands.join(name), "# origin copy\n").expect("placed");
    }

    let envelope =
        crate::instruction::delegation_envelope(&workspace, "build the module").expect("enveloped");
    assert!(
        envelope.starts_with("build the module"),
        "the instruction rides first: {envelope}"
    );
    for (marker, needle) in [
        ("run job", commands.join("ai.md").display().to_string()),
        (
            "next job",
            commands.join("next_ai.md").display().to_string(),
        ),
        (
            "close job",
            commands.join("close_ai.md").display().to_string(),
        ),
    ] {
        assert!(
            envelope.contains(needle.as_str()),
            "{marker} directed: {envelope}"
        );
    }
    assert!(
        !envelope.contains("invoke /") && !envelope.contains("/ai\n"),
        "directions never rely on slash expansion: {envelope}"
    );
}

#[test]
fn the_envelope_refuses_a_bare_slash_command_and_missing_procedures() {
    let dir = tempfile::tempdir().expect("temp");
    let bare = crate::instruction::delegation_envelope(dir.path(), "/ai")
        .expect_err("a bare /ai is the dependence ATH-ACLI-016 forbids");
    assert!(
        matches!(bare, crate::instruction::EnvelopeFault::SlashCommandRefused(ref text)
            if text == "/ai"),
        "{bare}"
    );
    assert!(
        matches!(
            crate::instruction::delegation_envelope(dir.path(), "   "),
            Err(crate::instruction::EnvelopeFault::EmptyInstruction)
        ),
        "empty delegation refused"
    );

    // With the workspace present but no placed procedure files, the direction cannot be
    // kept: refused, not silently weakened (ATH-ACLI-017).
    let workspace = dir.path().join("ws");
    std::fs::create_dir_all(workspace.join(".agent-cli").join("commands")).expect("dir");
    let error = crate::instruction::delegation_envelope(&workspace, "work the step")
        .expect_err("no procedure file placed");
    assert!(
        matches!(
            error,
            crate::instruction::EnvelopeFault::ProcedureMissing(_)
        ),
        "{error}"
    );
}

#[test]
fn the_persona_carries_the_operating_model_block_with_absolute_paths() {
    let dir = tempfile::tempdir().expect("temp");
    let workspace = dir.path().join("ws");
    let commands = workspace.join(".agent-cli").join("commands");
    std::fs::create_dir_all(&commands).expect("commands dir");
    for name in ["ai.md", "next_ai.md", "close_ai.md"] {
        std::fs::write(commands.join(name), "# origin copy\n").expect("placed");
    }
    let persona = crate::generation::render_persona("persona body", &[], &[]).expect("render");
    let _ = &workspace;
    // With no procedure files passed, the block is honest about the absence:
    assert!(
        persona.contains("(none placed here)"),
        "no files placed, nothing to direct: {persona}"
    );
    assert!(!persona.contains("## Operating model"), "{persona}");

    // With the files placed, the block names them absolute with their meanings.
    let files: Vec<PathBuf> = ["ai.md", "next_ai.md", "close_ai.md"]
        .iter()
        .map(|name| commands.join(name))
        .collect();
    let persona = crate::generation::render_persona("persona body", &[], &files).expect("render");
    assert!(persona.contains("## Operating model"), "{persona}");
    assert!(
        persona.contains(commands.join("ai.md").display().to_string().as_str()),
        "{persona}"
    );
    assert!(
        persona.contains("NOT commands"),
        "the persona marks the /tags as non-commands: {persona}"
    );
    assert!(
        persona.contains("one PlanStep"),
        "the next-job meaning carries the v1 single-step note: {persona}"
    );
}

// --- P4-07: the tokenizer and the short-term ceiling ride the resolved configuration ------

#[test]
fn the_generated_tools_carry_the_agent_cli_vocabulary_ath_acli_018() {
    // ATH-ACLI-018 (spec §2.2.4, verified against `agent-cli` 0.29.0): the rendered
    // `[tools] enabled` names only `agent-cli`-canonical tools, TOML-quoted — never the
    // ATHENA wire forms, never a peer-manipulating tool.
    let mut def = definition();
    def.allowed_tools = vec![
        ToolName::FsRead,
        ToolName::FsWrite,
        ToolName::Shell,
        ToolName::Git,
        ToolName::Search,
        ToolName::SendTo,
        ToolName::Network,
        ToolName::Process,
        ToolName::Terminal,
    ];
    let generated = crate::generation::render_config(
        &def,
        &Deadlines {
            startup_ms: 10_000,
            execution_ms: 600_000,
        },
        4,
        16,
        &crate::generation::ShortTermRender::default(),
    )
    .expect("renders");
    eprintln!("GENERATED TOML: {}", generated.toml);
    let toml = &generated.toml;
    assert!(
        toml.contains("enabled = [\"bash\", \"edit\", \"glob\", \"grep\", \"read\", \"send_to\", \"webfetch\", \"websearch\", \"write\"]"),
        "every granted tool maps into agent-cli's vocabulary, sorted, deduped: {toml}"
    );
    for absent in [
        "fs_read", "fs_write", "shell", "git", "search", "process", "terminal", "network",
    ] {
        assert!(
            !toml.contains(&format!("\"{absent}\"")),
            "an ATHENA wire form never reaches the wire: {absent} in {toml}"
        );
    }
    for excluded in ["spawn", "stop_agent", "list_agents", "monitor"] {
        assert!(
            !toml.contains(&format!("\"{excluded}\"")),
            "peer-manipulating tools are excluded categorically (ATH-HIER-011): {excluded} in {toml}"
        );
    }
}

#[test]
fn an_empty_grant_renders_an_empty_enabled_list() {
    let mut def = definition();
    def.allowed_tools = vec![];
    let generated = crate::generation::render_config(
        &def,
        &Deadlines {
            startup_ms: 10_000,
            execution_ms: 600_000,
        },
        4,
        16,
        &crate::generation::ShortTermRender::default(),
    )
    .expect("renders");
    assert!(
        generated.toml.contains("enabled = []"),
        "deny-by-default holds at the wire: {}",
        generated.toml
    );
}

#[test]
fn the_generated_history_ceiling_carries_the_resolved_values() {
    // The §28.1 configuration-file defaults, stated explicitly (never an implicit
    // provider default — `ATH-MEM-009`'s wording is about *reliance*, not about the
    // file naming its own default).
    let generated = crate::generation::render_config(
        &definition(),
        &Deadlines {
            startup_ms: 10_000,
            execution_ms: 600_000,
        },
        4,
        16,
        &crate::generation::ShortTermRender {
            max_tokens: 48_000,
            tokenizer: "provider-default-claude".into(),
        },
    )
    .expect("renders");
    assert!(
        generated.toml.contains("max_context_tokens = 48000"),
        "the ceiling is the configured value: {}",
        generated.toml
    );
}

#[test]
fn an_unstated_tokenizer_refuses_generation() {
    let error = crate::generation::render_config(
        &definition(),
        &Deadlines {
            startup_ms: 10_000,
            execution_ms: 600_000,
        },
        4,
        16,
        &crate::generation::ShortTermRender {
            max_tokens: 32_000,
            tokenizer: "   ".into(),
        },
    )
    .expect_err("an empty tokenizer cannot count (ATH-MEM-009)");
    assert!(
        matches!(error, crate::generation::GenerationFault::TokenizerUnstated),
        "{error}"
    );
}

#[tokio::test]
async fn the_adapter_passes_the_boot_wirings_resolved_values_through() {
    let dir = tempfile::tempdir().expect("temp");
    let adapter = AgentCliAdapter::new(
        FakeDriver::default(),
        dir.path().join("template"),
        4,
        16,
        vec![],
    )
    .with_short_term(crate::generation::ShortTermRender {
        max_tokens: 55_000,
        tokenizer: "provider-default-claude".into(),
    });
    let workspace = dir.path().join("instances").join("ai-st");
    Runtime::start(&adapter, definition(), ctx_with(workspace.clone()))
        .await
        .expect("started");
    let config =
        std::fs::read_to_string(workspace.join(".agent-cli").join("config.toml")).expect("config");
    assert!(
        config.contains("max_context_tokens = 55000"),
        "the boot wiring's value reaches the wire: {config}"
    );
}

// --- The IPC row: the real-transport round trip (verified against `agent-cli` 0.29.0) -----

struct ScriptedIpc {
    calls: std::sync::Arc<std::sync::Mutex<Vec<(PathBuf, String, String)>>>,
}

impl crate::adapter::IpcTransport for ScriptedIpc {
    fn ask(
        &self,
        workspace: &std::path::Path,
        peer: &str,
        body: &str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, RuntimeError>> + Send + '_>,
    > {
        self.calls.lock().expect("ipc calls").push((
            workspace.to_path_buf(),
            peer.to_string(),
            body.to_string(),
        ));
        Box::pin(async { Ok("done: the artifact is at /repo/artifacts/report.md".into()) })
    }
}

#[tokio::test]
async fn the_ipc_transport_delivers_over_the_registry_and_the_reply_becomes_the_result() {
    let dir = tempfile::tempdir().expect("temp");
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    // The delegation envelope directs the placed procedure files (P3-17): the template
    // origin must carry them for the start to place and the send to reference.
    let template = dir.path().join("template");
    let commands = template.join("commands");
    std::fs::create_dir_all(&commands).expect("commands dir");
    for name in ["ai.md", "next_ai.md", "close_ai.md"] {
        std::fs::write(commands.join(name), format!("# {name} (origin copy)\n"))
            .expect("procedure file");
    }
    let adapter = AgentCliAdapter::new(FakeDriver::default(), template, 4, 16, vec![])
        .with_ipc_transport(Arc::new(ScriptedIpc {
            calls: calls.clone(),
        }));
    let workspace = dir.path().join("instances").join("ipc-1");
    let context = ctx_with(workspace.clone());
    let task = context.task_id.clone().expect("a Task-bound start");
    let handle = Runtime::start(&adapter, definition(), context)
        .await
        .expect("started");
    Runtime::send(
        &adapter,
        &handle,
        athena_runtime::message::Message {
            message_id: MessageId::new(),
            message_type: MessageType::Instruction,
            from_agent: Some(AgentId::core_agent()),
            from_instance_id: None,
            thread_id: "th-ipc-1".into(),
            reply_to: None,
            priority: Priority::Normal,
            body: "produce the report".into(),
            payload_json: None,
            deadline: None,
        },
    )
    .await
    .expect("delivered over the transport");

    // The transport observed the enveloped body, addressed to the peer the record names.
    let calls = calls.lock().expect("ipc calls").clone();
    assert_eq!(calls.len(), 1, "one ask round trip: {calls:?}");
    assert_eq!(calls[0].1, definition().id.as_str());
    assert!(
        calls[0].2.contains("produce the report"),
        "the body rides the envelope: {}",
        calls[0].2
    );

    // The agent's reply is the result: self-declared, never the acceptance basis.
    let result = Runtime::collect_result(&adapter, &handle)
        .await
        .expect("the reply materializes");
    assert_eq!(result.task_id, task);
    assert_eq!(result.status, ResultStatus::Success);
    assert!(
        result.metadata.get("self_declared") == Some(&serde_json::json!(true)),
        "the result carries the self-declaration marker: {result:?}"
    );
}

#[tokio::test]
async fn collect_result_refuses_before_any_complete_arrived() {
    let dir = tempfile::tempdir().expect("temp");
    let adapter = AgentCliAdapter::new(
        FakeDriver::default(),
        dir.path().join("template"),
        4,
        16,
        vec![],
    );
    let workspace = dir.path().join("instances").join("ipc-2");
    let handle = Runtime::start(&adapter, definition(), ctx_with(workspace.clone()))
        .await
        .expect("started");
    let error = Runtime::collect_result(&adapter, &handle)
        .await
        .expect_err("nothing collected yet");
    assert!(matches!(error, RuntimeError::Failure(_)), "{error}");
}

#[tokio::test]
async fn instance_zero_collects_no_task_result() {
    let dir = tempfile::tempdir().expect("temp");
    let adapter = AgentCliAdapter::new(
        FakeDriver::default(),
        dir.path().join("template"),
        4,
        16,
        vec![],
    );
    let home = dir.path().join(".athena");
    let handle = Runtime::start(&adapter, core_definition(), core_context(&home))
        .await
        .expect("core booted");
    let error = Runtime::collect_result(&adapter, &handle)
        .await
        .expect_err("the Core Agent's output is not a Task result");
    assert!(matches!(error, RuntimeError::Validation(_)), "{error}");
}
