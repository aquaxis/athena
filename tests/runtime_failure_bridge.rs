//! P3-14 bridge: an abnormal `agent-cli` exit reaches Core as a resolved failure fact (the
//! `RuntimeError` report path ATH-TEST-024 exercises later with real fault injection).
//!
//! Chain under test: the Adapter classifies the exit in `status` (ATH-ACLI-005) →
//! `athena-core::runtime_manager::resolve_runtime_failure` routes it through the §17.2
//! Agent Instance machine the State Manager uses (ATH-CORE-048's routes) →
//! `failure_event` materializes the `AgentFailed` Event the store writes atomically with
//! the transition (ATH-PERS-003).

use athena_runtime::Runtime;
use athena_runtime_agent_cli::{AgentCliAdapter, ProcessDriver, SpawnedProcess};
use athena_types::{
    AgentDefinition, AgentId, AgentInstanceState, AgentLevel, Deadlines, InstanceId, ModelRef,
    RuntimeType, TaskId, ToolName,
};

/// Driver double: spawned pids stay alive until the shared switch flips them dead with the
/// abnormal exit code 1.
#[derive(Default, Clone)]
struct FailingDriver {
    failed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ProcessDriver for FailingDriver {
    fn spawn(
        &self,
        workspace: &std::path::Path,
        _command: &str,
    ) -> Result<SpawnedProcess, athena_runtime::RuntimeError> {
        std::fs::create_dir_all(workspace)
            .map_err(|e| athena_runtime::RuntimeError::Validation(format!("spawn: {e}")))?;
        Ok(SpawnedProcess {
            pid: 2001,
            runtime_instance_id: athena_types::RuntimeInstanceId::new(),
            peer_id: None,
        })
    }

    fn is_alive(&self, _pid: u32) -> bool {
        !self.failed.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn exit_code(&self, _pid: u32) -> Option<i32> {
        if self.failed.load(std::sync::atomic::Ordering::SeqCst) {
            Some(1)
        } else {
            None
        }
    }

    fn terminate(&self, _pid: u32) -> Result<(), athena_runtime::RuntimeError> {
        Ok(())
    }

    fn force_kill(&self, _pid: u32) -> Result<(), athena_runtime::RuntimeError> {
        Ok(())
    }
}

fn definition() -> AgentDefinition {
    AgentDefinition {
        id: AgentId::new("bridge-worker"),
        name: "Worker".into(),
        level: AgentLevel::Worker,
        description: "x".into(),
        capabilities: vec![],
        persona_ref: "scalar-worker".into(),
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

fn context(workspace: std::path::PathBuf) -> athena_runtime::StartContext {
    athena_runtime::StartContext {
        task_id: Some(TaskId::new()),
        plan_step_id: None,
        instance_id: InstanceId::new(),
        workspace: workspace.display().to_string(),
        work_targets: vec![workspace.join("target").display().to_string()],
        persona_ref: "scalar-worker".into(),
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

/// The whole P3-14 chain, without any Core Host orchestration change.
#[test]
fn an_abnormal_agent_cli_exit_resolves_into_core_as_a_failure_report() {
    futures::executor::block_on(async {
        let scratch = tempfile::tempdir().expect("scratch");
        let driver = FailingDriver::default();
        let failed_driver = driver.failed.clone();
        let adapter = AgentCliAdapter::new(driver, scratch.path().join("template"), 4, 16, vec![]);
        let workspace = scratch.path().join("instances").join("ai-1");
        let handle = Runtime::start(&adapter, definition(), context(workspace.clone()))
            .await
            .expect("started");

        // The agent process exits with a nonzero code — the abnormal exit the reportable
        // fact is defined on (ATH-ACLI-005).
        failed_driver.store(true, std::sync::atomic::Ordering::SeqCst);
        let observed = adapter.status(&handle).await.expect("status");

        // The Adapter classifies: the dead pid with exit 1 is a `Failed` observation.
        if let athena_runtime::RuntimeInstanceStatus::Running = observed {
            panic!("the exit must not look alive");
        }

        // Core resolves it through the §17.2 machine, as a supervision step would.
        let resolution = athena_core::runtime_manager::resolve_runtime_failure(
            AgentInstanceState::Busy,
            &observed,
        )
        .expect("the failed observation resolves for a Busy instance");
        let athena_core::runtime_manager::Resolution::Failed(report) = resolution else {
            panic!("expected a failure report");
        };
        assert_eq!(
            report.trigger,
            athena_state::InstanceTrigger::ExecutionFailed
        );
        assert_eq!(report.resolved_to, AgentInstanceState::Failed);
        assert_eq!(report.error_class, "runtime_error");

        // And the fact materializes the `AgentFailed` Event the store writes atomically
        // with the transition (ATH-PERS-003).
        let mut instance = agent_instance_fixture();
        instance.state = AgentInstanceState::Busy;
        let event = athena_core::runtime_manager::failure_event(
            &report,
            &instance,
            "2026-10-17T00:00:00Z".into(),
            "cor-bridge".into(),
        )
        .expect("the typed event builds");
        assert_eq!(event.event_type, "AgentFailed");
        assert!(
            event.payload_json.contains("\"runtime_error\""),
            "{event:?}"
        );
        assert!(event.payload_json.contains("\"unknown\""), "{event:?}");
    });
}

// `AgentInstance` fixture: the fields the Event materialization reads may be fixture
// values; the state is what the resolution consumed.
fn agent_instance_fixture() -> athena_types::AgentInstance {
    athena_types::AgentInstance {
        id: InstanceId::new(),
        agent_id: AgentId::new("bridge-worker"),
        task_id: Some(TaskId::new()),
        plan_step_id: None,
        superior_instance_id: None,
        delegation_depth: 1,
        state: AgentInstanceState::Busy,
        runtime_instance_id: None,
        workspace: "/tmp/bridge".into(),
        skill_set: vec![],
        aiprj_log_ref: None,
        attempt: 1,
        started_at: None,
        finished_at: None,
    }
}
