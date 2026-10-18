//! Stub Runtime behaviour (ATH-TEST-021).
//!
//! These tests cover the stub-specific mechanics the shared contract cannot: scripting,
//! delay, injected failures, heartbeat cadence, lease expiry, and the poll/snapshot
//! delivery model.

use crate::context::{FilesystemScope, StartContext};
use crate::error::RuntimeError;
use crate::interface::Runtime;
use crate::message::Message;
use crate::status::RuntimeInstanceStatus;
use crate::stub::{StubConfig, StubReaction, StubReply, StubRuntime, StubScript};
use athena_types::{
    AgentDefinition, AgentLevel, Deadlines, InstanceId, MessageId, MessageType, ModelRef, Priority,
    ResultStatus, RuntimeType, StopReason, TaskId, ToolName,
};
use futures::StreamExt;

type Reference = crate::RuntimeInstanceRef;

fn definition() -> AgentDefinition {
    AgentDefinition {
        id: athena_types::AgentId::new("rtl-worker"),
        name: "Worker".into(),
        level: AgentLevel::Worker,
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

fn ctx(task: Option<TaskId>) -> StartContext {
    StartContext {
        task_id: task,
        plan_step_id: None,
        instance_id: InstanceId::new(),
        workspace: "/athena/var/instances/ai-test".into(),
        work_targets: vec!["/project/src".into()],
        persona_ref: "workers/researcher".into(),
        persona_version: "1.0.0".into(),
        skill_set: vec![],
        allowed_tools: vec![ToolName::FsRead],
        delegable_tools: vec![],
        filesystem_scope: FilesystemScope {
            workspace: "/athena/var/instances/ai-test".into(),
            work_targets: vec!["/project/src".into()],
            read_only: vec![],
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

fn instruction(thread: &str, body: &str) -> Message {
    Message {
        message_id: MessageId::new(),
        message_type: MessageType::Instruction,
        from_agent: None,
        from_instance_id: None,
        thread_id: thread.into(),
        reply_to: None,
        priority: Priority::Normal,
        body: body.into(),
        payload_json: None,
        deadline: None,
    }
}

async fn start(runtime: &StubRuntime, task: Option<TaskId>) -> Reference {
    Runtime::start(runtime, definition(), ctx(task))
        .await
        .expect("stub start succeeds")
}

async fn next(runtime: &StubRuntime, reference: &Reference) -> Option<Message> {
    runtime.receive(reference).next().await
}

#[test]
fn a_scripted_instruction_produces_the_scheduled_replies() {
    let runtime = StubRuntime::scripted(StubScript::completes_any_instruction("the report"));
    futures::executor::block_on(async {
        let reference = start(&runtime, Some(TaskId::new())).await;
        Runtime::send(
            &runtime,
            &reference,
            instruction("th-1", "produce the report"),
        )
        .await
        .expect("accepted");

        // The delay-0 reply is due immediately; the later ones on the virtual schedule.
        let accepted = next(&runtime, &reference).await.expect("delay-0 reply");
        assert_eq!(accepted.message_type, MessageType::Status);

        runtime.clock().advance(10);
        let report = next(&runtime, &reference).await.expect("reply at t=10");
        assert_eq!(report.message_type, MessageType::Report);

        runtime.clock().advance(10);
        let complete = next(&runtime, &reference).await.expect("reply at t=20");
        assert_eq!(complete.message_type, MessageType::Complete);
        assert!(complete.body.contains("the report"), "{}", complete.body);
        assert_eq!(complete.thread_id, "th-1");

        // Delivered messages are not re-delivered: poll replay is not a re-send.
        runtime.clock().advance(500);
        assert!(next(&runtime, &reference).await.is_none());
    });
}

#[test]
fn failure_injection_chooses_the_error_class() {
    // ATH-TEST-021: the stub is the fixture for every injected error class.
    let runtime = StubRuntime::scripted(StubScript::default());
    runtime.arm_failures(
        vec![RuntimeError::Transient("startup window closed".into())],
        vec![],
    );
    futures::executor::block_on(async {
        let err = Runtime::start(&runtime, definition(), ctx(Some(TaskId::new())))
            .await
            .expect_err("injected");
        assert!(matches!(err, RuntimeError::Transient(_)), "{err}");
        assert_eq!(err.class(), "TransientError");

        // The failure is consumed: the next start succeeds.
        let _ = Runtime::start(&runtime, definition(), ctx(Some(TaskId::new())))
            .await
            .expect("the queue is one-shot");
    });
}

#[test]
fn an_unknown_instance_is_reported_not_fabricated() {
    let runtime = StubRuntime::scripted(StubScript::default());
    futures::executor::block_on(async {
        let stale = Reference {
            runtime_type: RuntimeType::LocalProcess,
            runtime_instance_id: athena_types::RuntimeInstanceId::new(),
            instance_id: InstanceId::new(),
        };
        let err = Runtime::send(&runtime, &stale, instruction("th-1", "hi"))
            .await
            .expect_err("unknown");
        assert!(matches!(err, RuntimeError::UnknownInstance(_)), "{err}");
        let status = runtime.status(&stale).await.expect_err("unknown");
        assert!(
            matches!(status, RuntimeError::UnknownInstance(_)),
            "{status}"
        );
    });
}

#[test]
fn a_control_message_stops_the_instance_immediately() {
    // ATH-MSG-017: cancel / interrupt jump the queue and are honoured at once.
    let runtime = StubRuntime::scripted(StubScript::completes_any_instruction("out"));
    futures::executor::block_on(async {
        let reference = start(&runtime, Some(TaskId::new())).await;
        runtime.clock().advance(5);
        let mut cancel = instruction("th-1", "produce the report");
        cancel.message_type = MessageType::Cancel;
        Runtime::send(&runtime, &reference, cancel)
            .await
            .expect("cancel delivered");
        let status = runtime.status(&reference).await.expect("status");
        assert!(!status.is_alive());
        assert!(
            matches!(status, RuntimeInstanceStatus::Stopped { .. }),
            "{status:?}"
        );
    });
}

#[test]
fn the_virtual_clock_drives_the_execution_deadline() {
    // ATH-RT-006 on the stub: observed by advancing virtual time only. The instance is
    // already Running, so what fires is the execution budget.

    let runtime = StubRuntime::scripted(StubScript::default());
    futures::executor::block_on(async {
        let reference = start(&runtime, None).await;
        runtime.clock().advance(600_001);
        let err = runtime.status(&reference).await.expect_err("deadline");
        assert!(matches!(err, RuntimeError::Deadline(_)), "{err}");
        assert_eq!(err.class(), "TimeoutError");
    });
}

#[test]
fn a_lapsed_heartbeat_lease_self_terminates() {
    // ATH-CORE-047: the lease belongs to ATHENA; self-termination to the process.
    let runtime = StubRuntime::new(
        StubConfig {
            heartbeat_interval_ms: None,
            lease_duration_ms: Some(60_000),
            ..StubConfig::default()
        },
        StubScript::default(),
    );
    futures::executor::block_on(async {
        let reference = start(&runtime, Some(TaskId::new())).await;

        // Manual renewals keep the lease alive indefinitely.
        runtime.clock().advance(50_000);
        runtime.heartbeat(&reference).expect("renewed");
        runtime.clock().advance(50_000);
        runtime.heartbeat(&reference).expect("renewed");
        assert_eq!(
            runtime.heartbeat_observations(&reference),
            vec![50_000, 100_000]
        );

        // 61 s after the last renewal the lease has lapsed.
        runtime.clock().advance(61_000);
        let status = runtime.status(&reference).await.expect("observed");
        let failed = matches!(&status, RuntimeInstanceStatus::Failed { detail }
            if detail.contains("ATH-CORE-047"));
        assert!(failed, "{status:?}");

        let err = Runtime::send(&runtime, &reference, instruction("th-1", "x"))
            .await
            .expect_err("dead");
        assert!(matches!(err, RuntimeError::Failure(_)), "{err}");
    });
}

#[test]
fn the_automatic_heartbeat_renews_on_the_configured_cadence() {
    let runtime = StubRuntime::new(
        StubConfig {
            heartbeat_interval_ms: Some(20_000),
            lease_duration_ms: Some(60_000),
            ..StubConfig::default()
        },
        StubScript::default(),
    );
    futures::executor::block_on(async {
        let reference = start(&runtime, None).await;
        for _ in 0..3 {
            runtime.clock().advance(20_000);
            let _ = next(&runtime, &reference).await;
        }
        assert_eq!(
            runtime.heartbeat_observations(&reference),
            vec![20_000, 40_000, 60_000],
            "a healthy agent renews every 20 s"
        );
        // Past three renewals the lease has never lapsed.
        runtime.clock().advance(1);
        let status = runtime.status(&reference).await.expect("alive");
        assert!(status.is_alive());
    });
}

#[test]
fn the_scripted_result_is_provisional_on_ingestion() {
    // D-7: the Runtime hands over an ExecutionResult whose id Core re-mints.
    let runtime = StubRuntime::scripted(StubScript::completes_any_instruction("artifact list"));
    futures::executor::block_on(async {
        let reference = start(&runtime, Some(TaskId::new())).await;
        Runtime::send(&runtime, &reference, instruction("th-1", "artifact list"))
            .await
            .expect("sent");
        runtime.clock().advance(100);

        let result = runtime.collect_result(&reference).await.expect("result");
        assert_eq!(result.status, ResultStatus::Success);
        assert!(result.output.contains("artifact list"));
        assert!(result.id.as_str().starts_with("res-"));
        assert_eq!(
            result.metadata.get("provisional"),
            Some(&serde_json::json!(true))
        );

        // One-shot: the scripted product is consumed by the first ingestion.
        let again = runtime
            .collect_result(&reference)
            .await
            .expect_err("exhausted");
        assert!(matches!(again, RuntimeError::Failure(_)), "{again}");
    });
}

#[test]
fn an_unattributable_result_is_refused() {
    // A result without a Task binding cannot be attributed (ATH-RESULT-001).
    let runtime = StubRuntime::scripted(StubScript::completes_any_instruction("x"));
    futures::executor::block_on(async {
        let reference = start(&runtime, None).await;
        Runtime::send(&runtime, &reference, instruction("th-1", "x"))
            .await
            .expect("sent");
        let err = runtime
            .collect_result(&reference)
            .await
            .expect_err("unattributable");
        assert!(matches!(err, RuntimeError::Validation(_)), "{err}");
    });
}

#[test]
fn stopping_a_completed_instance_is_idempotent() {
    let runtime = StubRuntime::scripted(StubScript::default());
    futures::executor::block_on(async {
        let reference = start(&runtime, Some(TaskId::new())).await;
        Runtime::stop(&runtime, &reference, StopReason::Completed)
            .await
            .expect("stop");
        Runtime::stop(&runtime, &reference, StopReason::Completed)
            .await
            .expect("stop again");
        let status = runtime.status(&reference).await.expect("status");
        assert!(
            matches!(
                status,
                RuntimeInstanceStatus::Stopped { exit_code: Some(0) }
            ),
            "{status:?}"
        );
    });
}

#[test]
fn a_reaction_can_retarget_the_trigger_body() {
    // First substring match wins; the default reaction is the fallback.
    let script = StubScript {
        reactions: vec![(
            "deploy".into(),
            StubReaction {
                replies: vec![StubReply {
                    message_type: MessageType::Report,
                    body: "release built".into(),
                    delay_ms: 0,
                }],
                result: None,
                status_after: None,
            },
        )],
        default: StubReaction::default(),
    };
    let runtime = StubRuntime::scripted(script);
    futures::executor::block_on(async {
        let reference = start(&runtime, None).await;
        Runtime::send(
            &runtime,
            &reference,
            instruction("th-1", "please deploy to production"),
        )
        .await
        .expect("sent");
        let reply = next(&runtime, &reference).await.expect("matched");
        assert_eq!(reply.body, "release built");

        // Unmatched bodies fall to the default reaction, which scripts nothing here.
        Runtime::send(
            &runtime,
            &reference,
            instruction("th-1", "write the summary"),
        )
        .await
        .expect("sent");
        assert!(next(&runtime, &reference).await.is_none());
    });
}
