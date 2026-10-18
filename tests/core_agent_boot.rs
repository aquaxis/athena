//! The Core Agent's migration to `agent-cli` #0 (P3-15):
//!
//! Core Host's boot (`athena_core::start_core_agent`, ATH-CORE-021) has always been
//! Runtime-agnostic; what this file proves is that the boot now goes through the real
//! `agent-cli` Adapter with:
//!
//! - the **`spawn`** execution line (ATH-ACLI-008; Phase 0 verified `spawn` re-execs
//!   `serve` detached and self-registers as an IPC-resolvable peer), not `run`
//!   (`ATH-ACLI-016`);
//! - a **dedicated Workspace** at `<ATHENA_HOME>/var/core-agent/` (ATH-OPS-009), never a
//!   subordinate's instance directory and never `<PROJECT_ROOT>`;
//! - **role isolation**: instance #0's record is tracked with the core role, excluded from
//!   the subordinate record set, and its process crashes never move a subordinate's record
//!   (1 : 1 Agent Instance : Runtime Instance, ATH-RT-012).

use athena_core::core_agent_session::start_core_agent;
use athena_runtime::FilesystemScope;
use athena_runtime::Runtime;
use athena_runtime_agent_cli::adapter::{AgentCliAdapter, ProcessDriver, SpawnedProcess};
use athena_runtime_agent_cli::workspace::core_agent_workspace;
use athena_types::{
    AgentDefinition, AgentId, AgentLevel, Deadlines, InstanceId, ModelRef, RuntimeType, TaskId,
    ToolName,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The driver double recording every `(workspace, command)` and tracking pids.
#[derive(Clone, Default)]
struct BootRecorder {
    counter: Arc<AtomicU64>,
    spawned: Arc<std::sync::Mutex<Vec<(PathBuf, String)>>>,
    dead: Arc<std::sync::Mutex<HashMap<u32, ()>>>,
}

impl ProcessDriver for BootRecorder {
    fn spawn(
        &self,
        workspace: &std::path::Path,
        command: &str,
    ) -> Result<SpawnedProcess, athena_runtime::RuntimeError> {
        std::fs::create_dir_all(workspace).ok();
        self.spawned
            .lock()
            .expect("spawned")
            .push((workspace.to_path_buf(), command.to_string()));
        let pid =
            u32::try_from(self.counter.fetch_add(1, Ordering::SeqCst) + 5000).unwrap_or(u32::MAX);
        Ok(SpawnedProcess {
            pid,
            runtime_instance_id: athena_types::RuntimeInstanceId::new(),
            peer_id: None,
        })
    }

    fn is_alive(&self, pid: u32) -> bool {
        !self.dead.lock().expect("dead").contains_key(&pid)
    }

    fn exit_code(&self, _pid: u32) -> Option<i32> {
        None
    }

    fn terminate(&self, pid: u32) -> Result<(), athena_runtime::RuntimeError> {
        self.dead.lock().expect("dead").insert(pid, ());
        Ok(())
    }

    fn force_kill(&self, pid: u32) -> Result<(), athena_runtime::RuntimeError> {
        self.dead.lock().expect("dead").insert(pid, ());
        Ok(())
    }
}

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

fn core_context(home: &std::path::Path) -> athena_runtime::StartContext {
    athena_runtime::StartContext {
        task_id: None,
        plan_step_id: None,
        instance_id: InstanceId::new(),
        workspace: core_agent_workspace(home).display().to_string(),
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
    }
}

fn boot(base: &std::path::Path) -> (AgentCliAdapter<BootRecorder>, BootRecorder, PathBuf) {
    let driver = BootRecorder::default();
    let adapter = AgentCliAdapter::new(driver.clone(), base.join("template"), 4, 16, vec![]);
    let home = base.join(".athena");
    (adapter, driver, home)
}

/// The recorded spawn lines, innermost last.
fn last_spawn(recorder: &BootRecorder) -> (PathBuf, String) {
    recorder
        .spawned
        .lock()
        .expect("spawned")
        .last()
        .cloned()
        .expect("a spawn was recorded")
}

#[test]
fn the_core_agent_boot_drives_instance_zero_onto_the_adapter() {
    let dir = tempfile::tempdir().expect("temp");
    let (adapter, recorder, home) = boot(dir.path());

    let handle = futures::executor::block_on(start_core_agent(
        &adapter,
        core_definition(),
        core_context(&home),
    ))
    .expect("instance #0 boots");

    // The record: the core role, on its dedicated Workspace.
    let record = adapter.core_record().expect("tracked");
    assert!(record.is_core_agent);
    assert_eq!(record.runtime_instance_id, handle.runtime_instance_id);
    assert_eq!(
        record.workspace,
        core_agent_workspace(&home),
        "cwd is the Core Agent's own Workspace (ATH-OPS-009)"
    );

    // The spawn line, not `run` (ATH-ACLI-008 vs ATH-ACLI-016).
    let (workspace, command) = last_spawn(&recorder);
    assert_eq!(workspace, core_agent_workspace(&home));
    assert!(
        command.split_whitespace().nth(1) == Some("spawn"),
        "the boot argv is the spawn mode: {command}"
    );
}

#[test]
fn the_boot_refuses_a_task_bound_core_start_through_the_real_adapter() {
    let dir = tempfile::tempdir().expect("temp");
    let (adapter, recorder, home) = boot(dir.path());
    let mut bound = core_context(&home);
    bound.task_id = Some(TaskId::new());

    let error = futures::executor::block_on(start_core_agent(&adapter, core_definition(), bound))
        .expect_err("refused (ATH-CORE-020)");
    assert!(
        matches!(error, athena_runtime::RuntimeError::Validation(_)),
        "{error}"
    );
    assert!(
        recorder.spawned.lock().expect("spawned").is_empty(),
        "the Runtime never observed the refused start"
    );
}

#[test]
fn instance_zero_is_isolated_from_the_subordinate_record_set() {
    let dir = tempfile::tempdir().expect("temp");
    let (adapter, recorder, home) = boot(dir.path());

    let _core_boot = futures::executor::block_on(start_core_agent(
        &adapter,
        core_definition(),
        core_context(&home),
    ))
    .expect("boots");

    // A delegated instance lands in its own instance directory, not the Core Agent's.
    let sub_handle = futures::executor::block_on(Runtime::start(
        &adapter,
        {
            let mut worker = core_definition();
            worker.id = AgentId::new("athena-worker-p15");
            worker.level = AgentLevel::Worker;
            worker.persona_ref = "speaker".into();
            worker.allowed_tools = vec![ToolName::FsRead];
            worker
        },
        athena_runtime::StartContext {
            task_id: Some(TaskId::new()),
            plan_step_id: None,
            instance_id: InstanceId::new(),
            workspace: dir
                .path()
                .join("instances")
                .join("ai-1")
                .display()
                .to_string(),
            work_targets: vec!["/repo/src".into()],
            persona_ref: "speaker".into(),
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
        },
    ))
    .expect("the delegation's instance boots");

    // The record sets are disjoint by role; two processes exist (separate pids, ATH-RT-012).
    let core_record = adapter.core_record().expect("core tracked");
    let subs = adapter.subordinate_records();
    assert_eq!(
        subs.len(),
        1,
        "the subordinate flow sees only its own record"
    );
    assert_ne!(
        subs[0].runtime_instance_id, core_record.runtime_instance_id,
        "instance #0 never appears in the subordinate set"
    );
    assert_ne!(core_record.pid, subs[0].pid);

    // Kill the Core Agent's process: the subordinate's record is untouched.
    let core_pid = core_record.pid;
    recorder.dead.lock().expect("dead").insert(core_pid, ());
    let subs_after = adapter.subordinate_records();
    assert_eq!(subs_after.len(), 1);
    assert_eq!(
        subs_after[0].runtime_instance_id, sub_handle.runtime_instance_id,
        "a core crash propagates nothing into the subordinate record (ATH-RT-006's
         reattach-or-stop still reads it)"
    );
    assert_ne!(subs_after[0].pid, core_pid);
}
