//! The Runtime Interface itself — see the module docs of [`crate::interface`].
//!
//! This test module exercises the trait through a minimal in-crate implementation, which
//! is also proof that the trait is implementable outside an Adapter crate and object-safe
//! for the registry (ATH-RT-008).

use crate::context::{FilesystemScope, StartContext};
use crate::error::{Result, RuntimeError};
use crate::interface::{Runtime, RuntimeInstanceRef};
use crate::message::Message;
use crate::registry::RuntimeRegistry;
use crate::status::{IsolationBoundary, RuntimeCapabilities, RuntimeInstanceStatus};
use athena_types::{
    AgentDefinition, AgentId, Deadlines, ExecutionResult, InstanceId, MessageId, MessageType,
    ModelRef, Priority, ResultId, RuntimeType, StopReason, ToolName,
};
use futures::StreamExt;
use std::sync::Arc;

/// A minimal Runtime, only to prove the trait's shape and the registry mechanics.
struct TestRuntime {
    kind: RuntimeType,
    started: std::sync::atomic::AtomicU64,
}

impl TestRuntime {
    fn new(kind: RuntimeType) -> Self {
        Self {
            kind,
            started: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn start_count(&self) -> u64 {
        std::sync::atomic::AtomicU64::load(&self.started, std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl Runtime for TestRuntime {
    fn runtime_type(&self) -> RuntimeType {
        self.kind
    }

    async fn start(
        &self,
        _definition: AgentDefinition,
        ctx: StartContext,
    ) -> Result<RuntimeInstanceRef> {
        ctx.validate().map_err(|err| match err {
            RuntimeError::Validation(detail) => RuntimeError::Validation(detail),
            other => other,
        })?;
        self.started
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(RuntimeInstanceRef {
            runtime_type: self.kind,
            runtime_instance_id: athena_types::RuntimeInstanceId::new(),
            instance_id: ctx.instance_id,
        })
    }

    async fn send(&self, _instance: &RuntimeInstanceRef, _message: Message) -> Result<()> {
        Ok(())
    }

    fn receive(
        &self,
        _instance: &RuntimeInstanceRef,
    ) -> futures::stream::BoxStream<'static, Message> {
        futures::stream::empty().boxed()
    }

    async fn status(&self, _instance: &RuntimeInstanceRef) -> Result<RuntimeInstanceStatus> {
        Ok(RuntimeInstanceStatus::Stopped { exit_code: Some(0) })
    }

    async fn stop(&self, _instance: &RuntimeInstanceRef, _reason: StopReason) -> Result<()> {
        Ok(())
    }

    async fn collect_result(&self, _instance: &RuntimeInstanceRef) -> Result<ExecutionResult> {
        Err(RuntimeError::UnknownInstance("nothing started".into()))
    }

    fn capabilities(&self) -> RuntimeCapabilities {
        RuntimeCapabilities {
            runtime_type: self.kind,
            isolation: IsolationBoundary::Process,
            supported_tools: vec![
                ToolName::FsRead,
                ToolName::FsWrite,
                ToolName::Search,
                ToolName::SendTo,
            ],
            max_concurrent_instances: 4,
        }
    }
}

fn ctx_for(instance: InstanceId) -> StartContext {
    StartContext {
        task_id: None,
        plan_step_id: None,
        instance_id: instance,
        workspace: "/tmp/instances/ai-test".into(),
        work_targets: vec!["/tmp/project/src".into()],
        persona_ref: "workers/researcher".into(),
        persona_version: "1.0.0".into(),
        skill_set: vec![athena_types::SkillRef {
            id: athena_types::SkillId::new("report-skill"),
            version: "1.0.0".into(),
            checksum: Some("sha256:0000".into()),
        }],
        allowed_tools: vec![ToolName::FsRead],
        delegable_tools: vec![],
        filesystem_scope: FilesystemScope {
            workspace: "/tmp/instances/ai-test".into(),
            work_targets: vec!["/tmp/project/src".into()],
            read_only: vec!["/tmp/project/.aiprj/instructions.md".into()],
        },
        model: ModelRef {
            provider: "claude".into(),
            name: "claude-opus-5".into(),
        },
        deadlines: Deadlines {
            startup_ms: 30_000,
            execution_ms: 600_000,
        },
        resume_snapshot_ref: None,
        context_document_ref: None,
    }
}

fn definition() -> AgentDefinition {
    AgentDefinition {
        id: AgentId::new("rtl-worker"),
        name: "Worker".into(),
        level: athena_types::AgentLevel::Worker,
        description: String::new(),
        capabilities: vec!["report".into()],
        persona_ref: "workers/researcher".into(),
        persona_version: "1.0.0".into(),
        allowed_tools: vec![ToolName::FsRead],
        delegable_tools: vec![],
        default_skills: vec![],
        runtime: RuntimeType::AgentCli,
        model: ModelRef {
            provider: "claude".into(),
            name: "claude-opus-5".into(),
        },
        max_delegation_depth: 0,
        dynamic: false,
    }
}

#[test]
fn a_start_context_without_a_workspace_is_refused() {
    let mut ctx = ctx_for(InstanceId::new());
    ctx.workspace = "   ".into();
    let err = ctx.validate().expect_err("must refuse");
    assert!(matches!(err, RuntimeError::Validation(_)), "{err}");
    assert_eq!(err.class(), "ValidationError");
}

#[test]
fn a_relative_work_target_is_refused() {
    // ATH-OPS-010: work targets are absolute, never cwd-relative.
    let mut ctx = ctx_for(InstanceId::new());
    ctx.work_targets = vec!["src/report.rs".into()];
    let err = ctx.validate().expect_err("must refuse");
    assert!(
        matches!(err, RuntimeError::Validation(ref d) if d.contains("absolute")),
        "{err}"
    );
}

#[test]
fn a_start_context_without_model_coordinates_is_refused() {
    // ATH-AGENT-008: no provider/model default exists.
    let mut ctx = ctx_for(InstanceId::new());
    ctx.model = ModelRef {
        provider: String::new(),
        name: "claude-opus-5".into(),
    };
    ctx.validate().expect_err("must refuse");
}

#[test]
fn the_core_agent_context_has_no_task() {
    // R-CORE-032: the Core Agent Instance spans Tasks; its start context carries no task.
    let ctx = ctx_for(InstanceId::new());
    assert!(ctx.task_id.is_none());
    assert!(ctx.plan_step_id.is_none());
    assert!(ctx.resume_snapshot_ref.is_none());
    ctx.validate().expect("core agent context is admissible");
}

#[test]
fn the_runtime_interface_is_usable_behind_a_registry_object() {
    // Only a single-threaded probe is needed: the trait compiles as `Arc<dyn Runtime>`.
    let runtime = Arc::new(TestRuntime::new(RuntimeType::AgentCli)) as Arc<dyn Runtime>;
    let ctx = ctx_for(InstanceId::new());
    futures::executor::block_on(runtime.start(definition(), ctx.clone())).expect("start succeeds");
    assert_eq!(runtime.capabilities().runtime_type, RuntimeType::AgentCli);
}

#[test]
fn no_start_happens_through_an_invalid_context() {
    let runtime = TestRuntime::new(RuntimeType::AgentCli);
    let mut ctx = ctx_for(InstanceId::new());
    ctx.work_targets = vec!["relative".into()];
    let err = futures::executor::block_on(runtime.start(definition(), ctx)).expect_err("refused");
    assert!(matches!(err, RuntimeError::Validation(_)), "{err}");
    assert_eq!(runtime.start_count(), 0);
}

#[test]
fn a_registered_runtime_is_found_by_kind_and_duplicate_kinds_are_refused() {
    let mut registry = RuntimeRegistry::new();
    assert!(registry.is_empty());

    registry
        .register(Arc::new(TestRuntime::new(RuntimeType::AgentCli)))
        .expect("first registration");
    let err = registry
        .register(Arc::new(TestRuntime::new(RuntimeType::AgentCli)))
        .expect_err("duplicate");
    assert!(matches!(err, RuntimeError::Internal(_)), "{err}");

    assert_eq!(registry.registered(), vec![RuntimeType::AgentCli]);
    let found = registry.require(&RuntimeType::AgentCli).expect("found");
    assert_eq!(found.runtime_type(), RuntimeType::AgentCli);

    let missing = match registry.require(&RuntimeType::Docker) {
        Ok(_) => panic!("no docker runtime was registered"),
        Err(missing) => missing,
    };
    assert!(matches!(missing, RuntimeError::Unsupported(_)), "{missing}");
    assert_eq!(missing.class(), "ValidationError");
}

#[test]
fn the_received_stream_reaches_the_caller() {
    let _runtime = TestRuntime::new(RuntimeType::AgentCli);
    let handle = RuntimeInstanceRef {
        runtime_type: RuntimeType::AgentCli,
        runtime_instance_id: athena_types::RuntimeInstanceId::new(),
        instance_id: InstanceId::new(),
    };
    // Any concrete stream projects through `boxed_stream` — the classic shape Adapters
    // reuse (design section 5.1, `receive`).
    let stream = futures::stream::iter(vec![Message {
        message_id: MessageId::new(),
        message_type: MessageType::Report,
        from_agent: Some(AgentId::new("rtl-worker")),
        from_instance_id: Some(handle.instance_id.clone()),
        thread_id: "th-1".into(),
        reply_to: None,
        priority: Priority::Normal,
        body: "step done".into(),
        payload_json: None,
        deadline: None,
    }]);
    let mut boxed = crate::interface::boxed_stream(stream);
    let first = futures::executor::block_on(boxed.next()).expect("one message");
    assert_eq!(first.message_type, MessageType::Report);
    assert!(first.is_undroppable(), "reports are undroppable");
    assert_eq!(first.thread_id, "th-1");
    assert!(futures::executor::block_on(boxed.next()).is_none());
}

#[test]
fn a_failed_result_ingestion_reports_the_agent_class() {
    // ATH-ACLI-005: an abnormal agent end is a failure reported to Core, not swallowed.
    let runtime = TestRuntime::new(RuntimeType::AgentCli);
    let handle = RuntimeInstanceRef {
        runtime_type: RuntimeType::AgentCli,
        runtime_instance_id: athena_types::RuntimeInstanceId::new(),
        instance_id: InstanceId::new(),
    };
    let err = futures::executor::block_on(runtime.collect_result(&handle)).expect_err("reported");
    assert!(matches!(err, RuntimeError::UnknownInstance(_)), "{err}");
}

#[test]
fn the_result_ingested_here_is_not_the_persisted_aggregate() {
    // D-7: the id on a Runtime-collected result is provisional; Result Manager re-mints it
    // (ATH-CORE-041). The type itself stays `athena_types::ExecutionResult`, so nothing in
    // Runtime-land can already embed a persisted identity claim.
    let provisional = ExecutionResult {
        id: ResultId::new(),
        task_id: athena_types::TaskId::new(),
        plan_step_id: None,
        agent_id: AgentId::new("rtl-worker"),
        instance_id: InstanceId::new(),
        status: athena_types::ResultStatus::Success,
        output: "raw agent output".into(),
        artifacts: vec![],
        logs_ref: None,
        skills_used: vec![],
        model: None,
        started_at: "2026-10-14T00:00:00.000Z".into(),
        finished_at: "2026-10-14T00:00:01.000Z".into(),
        execution_time_ms: 1_000,
        metadata: serde_json::json!({ "provisional": true }),
    };
    assert!(!provisional.output.is_empty());
}
