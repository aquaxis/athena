//! The Phase 3 bundle (P3-18): the Adapter-level Agent Path. Deterministic CI — no
//! real `agent-cli` process and no LLM (`ATH-TEST-022`; the driver double stands at the
//! process boundary and the file protocol carries the agent side):
//!
//! - **a Task completes over the Adapter boundary**: the enveloped delegation lands in
//!   the Workspace inbox (P3-17), the agent's `complete` comes back through
//!   `receive` (the `Result` persistence itself is Phase 4's intake, P4-10);
//! - **`.agent-cli/` generation idempotence**: a re-start regenerates byte-identical
//!   files and never clobbers a live write area (`ATH-AIPRJ-006`);
//! - **the control surface is unwritable by the agent** (`ATH-CORE-043`, `ATH-SEC-010`):
//!   the scope excludes the Instance's own `.agent-cli/` and `skills/` and the project's
//!   `.athena/`;
//! - **isolation and crash non-propagation in both directions** live in
//!   `core_agent_boot.rs` (instance #0 vs the subordinate record sets; this file's
//!   cross-references record the fact).

use athena_runtime::FilesystemScope;
use athena_runtime::Runtime;
use athena_runtime::message::Message;
use athena_runtime_agent_cli::adapter::{AgentCliAdapter, ProcessDriver, SpawnedProcess};
use athena_types::{
    AgentDefinition, AgentId, AgentLevel, Deadlines, InstanceId, MessageId, MessageType, ModelRef,
    Priority, RuntimeType, TaskId, ToolName,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The driver double: virtual pids, all spawned processes born alive, killed on request.
#[derive(Clone, Default)]
struct Recorder {
    counter: Arc<AtomicU64>,
    dead: Arc<std::sync::Mutex<Vec<u32>>>,
}

impl ProcessDriver for Recorder {
    fn spawn(
        &self,
        workspace: &Path,
        _command: &str,
    ) -> Result<SpawnedProcess, athena_runtime::RuntimeError> {
        std::fs::create_dir_all(workspace).expect("workspace dir");
        let pid =
            u32::try_from(self.counter.fetch_add(1, Ordering::SeqCst) + 7000).unwrap_or(u32::MAX);
        Ok(SpawnedProcess {
            pid,
            runtime_instance_id: athena_types::RuntimeInstanceId::new(),
            peer_id: None,
        })
    }

    fn is_alive(&self, pid: u32) -> bool {
        !self.dead.lock().expect("dead").contains(&pid)
    }

    fn exit_code(&self, _pid: u32) -> Option<i32> {
        None
    }

    fn terminate(&self, pid: u32) -> Result<(), athena_runtime::RuntimeError> {
        self.dead.lock().expect("dead").push(pid);
        Ok(())
    }

    fn force_kill(&self, pid: u32) -> Result<(), athena_runtime::RuntimeError> {
        self.dead.lock().expect("dead").push(pid);
        Ok(())
    }
}

fn definition() -> AgentDefinition {
    AgentDefinition {
        id: AgentId::new("p3-worker"),
        name: "Worker".into(),
        level: AgentLevel::Worker,
        description: "phase 3 fixture".into(),
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

fn context(workspace: PathBuf) -> athena_runtime::StartContext {
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
        filesystem_scope: FilesystemScope::default(),
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

/// The seeded template origin (the `athena init` shape): the aiprj operating-model
/// procedures with absolute reads (`@<PROJECT_ROOT>/.aiprj/…`), cwd-relative writes.
fn seeded_template(base: &Path) -> PathBuf {
    let template = base.join("template");
    let commands = template.join("commands");
    std::fs::create_dir_all(&commands).expect("template commands");
    let project_root = base.join("project");
    for (name, reference) in [
        ("ai.md", "exec_job"),
        ("next_ai.md", "next_job"),
        ("close_ai.md", "close_ai"),
    ] {
        std::fs::write(
            commands.join(name),
            format!(
                "read @{}/.aiprj/rules/{reference}.md (absolute)\nwrite .aiprj/AI_LOG/\n",
                project_root.display()
            ),
        )
        .expect("procedure file");
    }
    template
}

fn adapter(base: &Path) -> AgentCliAdapter<Recorder> {
    adapter_with(base).0
}

/// The Adapter plus its own driver double — the double the tests crash through.
fn adapter_with(base: &Path) -> (AgentCliAdapter<Recorder>, Recorder) {
    let recorder = Recorder::default();
    (
        AgentCliAdapter::new(recorder.clone(), seeded_template(base), 4, 16, vec![]),
        recorder,
    )
}

/// P3-18 row 1: a Task completes across the Adapter boundary — the enveloped delegation
/// arrives in a Workspace inbox the *agent* reads, and the agent's `complete` message
/// comes back through `receive`. (The `Result` intake and its persistence land with
/// Phase 4's Result Manager, P4-10..P4-12.)
#[test]
fn a_task_completes_over_the_adapter_boundary() {
    let dir = tempfile::tempdir().expect("temp");
    let adapter = adapter(dir.path());
    let workspace = dir.path().join("instances").join("ai-1");
    let handle = futures::executor::block_on(Runtime::start(
        &adapter,
        definition(),
        context(workspace.clone()),
    ))
    .expect("started");

    // Core → Agent: the enveloped delegation.
    let instruction = "implement the ordered module";
    futures::executor::block_on(Runtime::send(
        &adapter,
        &handle,
        Message {
            message_id: MessageId::new(),
            message_type: MessageType::Instruction,
            from_agent: Some(AgentId::core_agent()),
            from_instance_id: None,
            thread_id: "th-p3-18".into(),
            reply_to: None,
            priority: Priority::Normal,
            body: instruction.into(),
            payload_json: None,
            deadline: None,
        },
    ))
    .expect("delegated");
    let inbox: Vec<_> = std::fs::read_dir(workspace.join(".athena-delegations"))
        .expect("inbox")
        .flatten()
        .collect();
    assert_eq!(
        inbox.len(),
        1,
        "exactly one enveloped delegation the agent consumes"
    );
    let delegation = std::fs::read_to_string(inbox[0].path()).expect("delegation body");
    assert!(
        delegation.contains(instruction),
        "the instruction rides inside the envelope"
    );
    assert!(
        delegation.contains("ATH-AIPRJ-011") && delegation.contains(".agent-cli/commands/"),
        "the operating-model direction rides the envelope (P3-17)"
    );

    // Agent → Core: the agent's own boundary write (what `agent-cli` normally reports by
    // IPC; the double writes the same file) — a `complete` saying the work is done.
    let outbox = workspace.join(".athena-outbox");
    std::fs::create_dir_all(&outbox).expect("outbox");
    std::fs::write(
        outbox.join("agent-complete.json"),
        r#"{"message_id":"msg-p3-18-complete","type":"complete","thread":"th-p3-18","body":"the module is implemented; log saved to .aiprj/AI_LOG/"}"#,
    )
    .expect("agent side completed");
    let received: Vec<Message> = futures::executor::block_on(async {
        use futures::StreamExt as _;
        adapter.receive(&handle).collect().await
    });
    assert_eq!(received.len(), 1);
    assert_eq!(
        received[0].message_type,
        MessageType::Complete,
        "the agent's completion crossed the boundary"
    );
    assert!(
        received[0].body.contains("AI_LOG"),
        "the completion cites the saved run log (ATH-AIPRJ-008's close step)"
    );
}

/// P3-18 row 2: `.agent-cli/` generation idempotence — a re-start regenerates the same
/// bytes and never clobbers a live write area (`ATH-AIPRJ-006`, `ATH-ACLI-013`).
#[test]
fn a_restart_regenerates_without_clobbering_the_live_areas() {
    let dir = tempfile::tempdir().expect("temp");
    let adapter = adapter(dir.path());
    let workspace = dir.path().join("instances").join("ai-2");

    let first = futures::executor::block_on(Runtime::start(
        &adapter,
        definition(),
        context(workspace.clone()),
    ))
    .expect("first start");
    let persona_first =
        std::fs::read(workspace.join(".agent-cli").join("persona.md")).expect("persona");
    let config_first =
        std::fs::read(workspace.join(".agent-cli").join("config.toml")).expect("config");

    // The agent worked between the starts (its tab file and log are live facts).
    std::fs::write(
        workspace.join(".aiprj").join("AI_PRJ_TASKS.md"),
        "the agent's own task table",
    )
    .expect("agent wrote");
    std::fs::write(
        workspace
            .join(".aiprj")
            .join("AI_LOG")
            .join("2026-10-17_000.md"),
        "log",
    )
    .expect("agent logged");

    futures::executor::block_on(async {
        Runtime::stop(&adapter, &first, athena_types::StopReason::Shutdown)
            .await
            .expect("stopped");
    });
    let second = futures::executor::block_on(Runtime::start(
        &adapter,
        definition(),
        context(workspace.clone()),
    ))
    .expect("second start");
    assert_ne!(
        second.runtime_instance_id, first.runtime_instance_id,
        "a restart is a fresh Runtime Instance"
    );

    // Generated control-surface files are byte-identical (source-derived, idempotent).
    assert_eq!(
        std::fs::read(workspace.join(".agent-cli").join("persona.md")).expect("persona"),
        persona_first,
        "the persona regenerates identically"
    );
    assert_eq!(
        std::fs::read(workspace.join(".agent-cli").join("config.toml")).expect("config"),
        config_first,
        "the config regenerates identically"
    );
    // The live areas were never clobbered.
    assert_eq!(
        std::fs::read_to_string(workspace.join(".aiprj").join("AI_PRJ_TASKS.md")).expect("tasks"),
        "the agent's own task table",
        "the agent's task table survives (`ATH-AIPRJ-006`)"
    );
    assert!(
        workspace
            .join(".aiprj")
            .join("AI_LOG")
            .join("2026-10-17_000.md")
            .exists(),
        "the saved log survives"
    );
}

/// P3-18 row 3: the control surface is unwritable by the agent — the scope the Runtime
/// projects excludes the Instance's own `.agent-cli/` and `skills/`, and the project's
/// `.athena/` (`ATH-CORE-043`, `ATH-SEC-010`).
#[test]
fn the_control_surface_is_excluded_from_the_agents_scope() {
    let dir = tempfile::tempdir().expect("temp");
    let project_root = dir.path().join("project");
    std::fs::create_dir_all(project_root.join(".athena").join("var")).expect("project layout");
    let workspace = dir
        .path()
        .join(".athena")
        .join("var")
        .join("instances")
        .join("ai-3");

    let scope = athena_runtime_agent_cli::generation::build_scope(
        &workspace,
        &["/repo/src".into()],
        &project_root,
    );

    let under = |root: &Path, name: &str| root.join(name);
    let excluded = |path: &Path| scope.excluded.iter().any(|root| path.starts_with(root));
    // The control surface itself and anything under it is out of scope.
    assert!(excluded(&under(&workspace, ".agent-cli")));
    assert!(excluded(&under(
        &workspace.join(".agent-cli"),
        "config.toml"
    )));
    assert!(excluded(&under(&workspace, "skills")));
    // The project's `.athena/` is out of scope for every subordinate.
    assert!(excluded(&project_root.join(".athena")));
    assert!(excluded(&under(&project_root.join(".athena"), "athena.db")));
    // The work target and the Workspace remain writable.
    let writable = |path: &Path| scope.writable.iter().any(|root| path.starts_with(root));
    assert!(writable(&workspace));
    assert!(writable(&Path::new("/repo/src").join("mod.rs")));
    // And the conventions are readable, never rewritable.
    let read_only = |path: &Path| scope.read_only.iter().any(|root| path.starts_with(root));
    assert!(read_only(
        &project_root.join(".aiprj").join("instructions.md")
    ));
    assert!(
        excluded(&project_root.join(".aiprj").join("instructions.md"))
            || !writable(&project_root.join(".aiprj").join("instructions.md"))
    );
}

/// P3-18 row 4 (the complement): a subordinate crash never moves instance #0's record —
/// the ─ direction `core_agent_boot.rs` covers core → subordinate; this is the other way.
#[test]
fn a_subordinate_crash_is_invisible_to_the_core_agent_record() {
    let dir = tempfile::tempdir().expect("temp");
    let (adapter, recorder) = adapter_with(dir.path());
    let home = dir.path().join(".athena");

    // Instance #0 boots through Core Host's own start, in its dedicated Workspace.
    let core_boot = futures::executor::block_on(athena_core::core_agent_session::start_core_agent(
        &adapter,
        {
            let mut producer = definition();
            producer.id = AgentId::core_agent();
            producer.level = AgentLevel::Producer;
            producer.persona_ref = "producer".into();
            producer.allowed_tools = vec![ToolName::FsRead, ToolName::FsWrite, ToolName::SendTo];
            producer.max_delegation_depth = 4;
            producer
        },
        athena_runtime::StartContext {
            task_id: None,
            plan_step_id: None,
            instance_id: InstanceId::new(),
            workspace: athena_runtime_agent_cli::workspace::core_agent_workspace(&home)
                .display()
                .to_string(),
            work_targets: vec![],
            persona_ref: "producer".into(),
            persona_version: "1.0.0".into(),
            skill_set: vec![],
            allowed_tools: vec![ToolName::FsRead, ToolName::FsWrite, ToolName::SendTo],
            delegable_tools: vec![],
            filesystem_scope: FilesystemScope::default(),
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
        },
    ))
    .expect("instance #0 boots");

    // A subordinate boots and then crashes (force-killed: a `kill -9` the reclamation
    // has not yet swept — the record may linger, but that is not instance #0's concern).
    let subordinate = futures::executor::block_on(Runtime::start(
        &adapter,
        definition(),
        context(dir.path().join("instances").join("ai-4")),
    ))
    .expect("subordinate boots");
    let sub_pid = adapter
        .process_record(subordinate.runtime_instance_id.as_str())
        .expect("tracked")
        .pid;

    let core_before = adapter.core_record().expect("instance #0 tracked");
    recorder.force_kill(sub_pid).expect("subordinate crashed");

    let core_after = adapter.core_record().expect("instance #0 still tracked");
    assert_eq!(
        (
            core_before.pid,
            core_before.workspace,
            core_before.instance_id,
            core_before.is_core_agent,
            core_before.workspace_initialized,
            core_before.last_beat,
        ),
        (
            core_after.pid,
            core_after.workspace,
            core_after.instance_id,
            core_after.is_core_agent,
            core_after.workspace_initialized,
            core_after.last_beat,
        ),
        "instance #0's record facts never moved"
    );
    assert!(
        adapter
            .process_record(core_boot.runtime_instance_id.as_str())
            .is_some(),
        "instance #0 stays tracked while the subordinate's process is dead"
    );
}
