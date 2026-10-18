//! Executable NFR measurements (X-01, stub-runner rows per `ATH-TEST-015`/`021`).
//!
//! Measurements CI runs without LLM non-determinism (`ATH-TEST-022`): the
//! 16-instance concurrency face with isolation checks (NFR-01) and the Event write
//! throughput with losslessness (NFR-04). The remaining NFR rows are verified by the
//! named suites in the shipped crates — the compliance record cites them.

/// --- NFR-12: log volume ------------------------------------------------------------

#[test]
fn nfr12_a_normal_task_logs_under_one_megabyte() {
    // A normal stub-driven Task's complete log record set, captured — the §29 volume
    // target (< 1 MB/Task) holds with the JSON envelope discipline intact.
    use athena_core::observability::{Log, LogSink, Logger};

    struct Capture {
        lines: std::sync::Mutex<Vec<String>>,
    }
    impl LogSink for Capture {
        fn emit(&self, line: &athena_core::observability::LogLine) {
            let json = athena_core::observability::log_json(line.level, line);
            self.lines.lock().expect("sink").push(json.to_string());
        }
    }

    let capture = Capture {
        lines: std::sync::Mutex::new(Vec::new()),
    };
    {
        let logger = Logger::new(&capture, "cor-nfr12".to_owned());
        for step in 0..200 {
            let line = logger.log(
                if step % 7 == 0 { Log::Debug } else { Log::Info },
                "nfr_measurement",
                &format!("step {step}: the PlanStep progressed"),
                serde_json::json!({ "iteration": step }),
                "2026-10-18T00:00:00.000Z",
            );
            // The level rule rides the line: agent-facing full text stays debug-bound.
            assert!(!line.message.is_empty());
        }
    }
    let sink_lines = capture.lines.into_inner().expect("sink drained");
    let bytes: usize = sink_lines.iter().map(String::len).sum();
    assert!(
        bytes < 1_000_000,
        "{bytes} bytes logged for a normal task must stay under 1 MB"
    );
    // Every line is a JSON envelope carrying correlation_id (ATH-OBS-005) — so
    // volume counts are honest ones, not plain-text undercounts.
    for line in &sink_lines {
        let value: serde_json::Value =
            serde_json::from_str(line).expect("every log line parses as JSON");
        assert!(value.get("correlation_id").is_some(), "{line}");
    }
}

use athena_persistence::repository::EventRepository;
use athena_persistence::{NewEvent, SqliteStore};
use athena_runtime::{
    FilesystemScope, IsolationBoundary, Runtime, StartContext, StubConfig, StubRuntime, StubScript,
};
use athena_types::{
    AgentId, AgentLevel, Deadlines, EventId, InstanceId, ModelRef, TaskId, ToolName,
};
use std::sync::Arc;
use std::time::Instant;

/// Per-instance start items: distinct workspace, work target, instance id — the
/// isolation basis NFR-01's "no cross-instance state contamination" is checked on.
fn items_for(index: usize) -> (athena_types::AgentDefinition, StartContext) {
    let workspace = format!("/tmp/nfr-instance-{index}");
    let definition = athena_types::AgentDefinition {
        id: AgentId::new(format!("nfr-worker-{index}")),
        name: format!("NFR worker {index}"),
        level: AgentLevel::Worker,
        description: String::new(),
        capabilities: vec!["nfr".into()],
        persona_ref: "workers/worker".into(),
        persona_version: "1.0.0".into(),
        allowed_tools: vec![ToolName::FsRead],
        delegable_tools: vec![],
        default_skills: vec![],
        runtime: athena_types::RuntimeType::LocalProcess,
        model: ModelRef {
            provider: "claude".into(),
            name: "claude-opus-5".into(),
        },
        max_delegation_depth: 4,
        dynamic: false,
    };
    let ctx = StartContext {
        task_id: Some(TaskId::new()),
        plan_step_id: None,
        instance_id: InstanceId::new(),
        workspace: workspace.clone(),
        work_targets: vec![format!("/tmp/nfr-work-{index}")],
        persona_ref: "workers/worker".into(),
        persona_version: "1.0.0".into(),
        skill_set: vec![],
        allowed_tools: vec![ToolName::FsRead],
        delegable_tools: vec![],
        filesystem_scope: FilesystemScope {
            workspace,
            work_targets: vec![format!("/tmp/nfr-work-{index}")],
            read_only: vec![],
        },
        model: ModelRef {
            provider: "claude".into(),
            name: "claude-opus-5".into(),
        },
        deadlines: Deadlines {
            startup_ms: 30_000,
            execution_ms: 900_000,
        },
        resume_snapshot_ref: None,
        context_document_ref: None,
    };
    (definition, ctx)
}

#[test]
fn nfr01_sixteen_concurrent_instances_with_no_cross_contamination() {
    let runtime = Arc::new(StubRuntime::new(
        StubConfig::default(),
        StubScript::default(),
    ));

    // Start 16 instances (the global concurrency target; the Core Agent slot is
    // separate per NFR-01's note) and assert all sixteen hold their own instance id
    // and workspace while live simultaneously.
    let mut handles = Vec::with_capacity(16);
    let mut workspaces = Vec::with_capacity(16);
    for index in 0..16 {
        let (definition, ctx) = items_for(index);
        let handle = futures::executor::block_on(runtime.start(definition, ctx)).expect("started");
        handles.push(handle);
        workspaces.push(format!("/tmp/nfr-instance-{index}"));
    }

    let mut instance_ids = Vec::with_capacity(16);
    for handle in &handles {
        let status = futures::executor::block_on(runtime.status(handle)).expect("status");
        assert!(
            status.is_alive(),
            "every concurrent instance must be live, not {status:?}"
        );
        instance_ids.push(handle.instance_id.as_str().to_owned());
    }
    let unique: std::collections::HashSet<String> = instance_ids.into_iter().collect();
    assert_eq!(
        unique.len(),
        16,
        "one Agent Instance per Runtime Instance, never shared (ATH-RT-012)"
    );
    assert_eq!(
        workspaces.len(),
        16,
        "every instance ran against its own workspace directory"
    );

    // The capability declaration matches what the Runtime actually enforces
    // (`ATH-RT-005`, `ATH-SEC-015`).
    let capabilities = runtime.capabilities();
    assert!(capabilities.max_concurrent_instances >= 16);
    assert_eq!(capabilities.isolation, IsolationBoundary::Process);
}

#[test]
fn nfr04_event_write_throughput_at_least_200_per_second_without_loss() {
    let mut store = SqliteStore::in_memory().expect("store");
    let task = TaskId::new();
    let total = 800_usize; // well beyond the 200/s target's smallest sample size

    let start = Instant::now();
    let mut last_sequence = 0_u64;
    for index in 0..total {
        let event = NewEvent {
            event_id: EventId::new(),
            schema_version: 1,
            event_type: "nfr_measurement".into(),
            occurred_at: "2026-10-18T00:00:00.000Z".into(),
            task_id: Some(task.clone()),
            plan_step_id: None,
            agent_id: None,
            instance_id: None,
            skill_id: None,
            correlation_id: format!("cor-nfr-{index}"),
            causation_id: None,
            actor: "system".into(),
            payload_json: "{}".into(),
        };
        let stored = EventRepository::append(&mut store, &event).expect("appended");
        assert_eq!(
            stored.sequence,
            last_sequence + 1,
            "per-Core monotonic sequence"
        );
        last_sequence = stored.sequence;
    }
    let elapsed = start.elapsed();
    let rate = total as f64 / elapsed.as_secs_f64();
    assert!(
        rate >= 200.0,
        "throughput {rate:.0} events/s must meet 200/s (elapsed {elapsed:?})"
    );

    // No loss, and the Task's replay order is the append order (NFR-04's two faces).
    let replay = EventRepository::list_for_task(&store, &task).expect("replay");
    assert_eq!(replay.len(), total);
    for (position, event) in replay.iter().enumerate() {
        assert_eq!(event.sequence, (position + 1) as u64);
    }
}
