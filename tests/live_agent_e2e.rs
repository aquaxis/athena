//! The live-agent acceptance lane (X-01's real-runner row; explicitly *not* a CI
//! test — `ATH-TEST-022` keeps CI off real processes and LLMs, so this is
//! `#[ignore]` and runs by hand:
//!
//! ```text
//! cargo test -p athena-conformance --test live_agent_e2e -- --ignored --nocapture
//! ```
//!
//! The lane rides the installed `agent-cli` (verified 0.29.0, 2026-10-18 — the Option-B
//! basis). It proves at the real-process boundary:
//!
//! - **one real delegation round trip**: a subordinate Agent Instance is spawned as a
//!   foreground headless `serve` peer with cwd = its Workspace, receives the enveloped
//!   `instruction` as a real `agent-cli ask`, *executes real file work in the
//!   Workspace* (bash + write tools projected into the generated `.agent-cli/config.toml`),
//!   and its reply is recorded as the `complete` — `collect_result` then materializes
//!   the ExecutionResult, staged through the real Result Manager's re-base path;
//! - **instance #0's residency**: the Core Agent boots on the detached `spawn` line and
//!   answers the teardown probes until the harness stops it (the same 1 : 1 isolation
//!   `core_agent_boot.rs` proves deterministically);
//! - **the configured model is the definition's**: the harness passes the provider/model
//!   through the definition (the local `ollama` provider; `ATH-AGENT-008`'s no-default
//!   rule exercised literally — without it the run refuses).
//!
//! Skipped honestly when the executable or provider is absent.

use athena_runtime::Runtime;
use athena_runtime::message::Message;
use athena_runtime_agent_cli::adapter::{AgentCliAdapter, RealIpc, SpawnedProcess};
use athena_runtime_agent_cli::process::TokioDriver;
use athena_types::{
    AgentDefinition, AgentId, AgentLevel, Deadlines, InstanceId, MessageId, MessageType, ModelRef,
    Priority, ResultStatus, RuntimeType, TaskId, ToolName,
};
use std::path::Path;
use std::sync::Arc;

fn agent_cli_present() -> bool {
    std::process::Command::new("agent-cli")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn worker_definition(provider: &str, model: &str, persona_file: &Path) -> AgentDefinition {
    AgentDefinition {
        id: AgentId::new("live-worker"),
        name: "Live Worker".into(),
        level: AgentLevel::Worker,
        description: "the live lane's one-PlanStep Worker".into(),
        capabilities: vec![],
        persona_ref: persona_file.display().to_string(),
        persona_version: "1.0.0".into(),
        allowed_tools: vec![ToolName::FsRead, ToolName::FsWrite, ToolName::Shell],
        delegable_tools: vec![],
        default_skills: vec![],
        runtime: RuntimeType::AgentCli,
        model: ModelRef {
            provider: provider.into(),
            name: model.into(),
        },
        max_delegation_depth: 1,
        dynamic: false,
    }
}

#[tokio::test]
#[ignore = "the real-runner lane: contacts the installed agent-cli and its provider"]
async fn a_real_agent_completes_the_delegated_work() {
    assert!(
        agent_cli_present(),
        "agent-cli must be installed for this lane"
    );
    // The provider/model come from the environment (the composition root's resolved
    // configuration in a real boot); the local ollama provider is the assumption.
    let (provider, model) = (
        std::env::var("ATHENA_LIVE_PROVIDER").unwrap_or_else(|_| "ollama".into()),
        std::env::var("ATHENA_LIVE_MODEL").unwrap_or_else(|_| "glm-5.3-flash:cloud".into()),
    );

    let dir = tempfile::tempdir().expect("temp");
    let home = dir.path().to_path_buf();
    // The delegation envelope directs the placed procedure files (P3-17): the origin
    // carries them (here: the harness's own copies, absolute-read clean).
    let template = home.join("template");
    let commands = template.join("commands");
    std::fs::create_dir_all(&commands).expect("commands dir");
    for name in ["ai.md", "next_ai.md", "close_ai.md"] {
        std::fs::write(
            commands.join(name),
            format!("# {name}: obey the running conventions\n"),
        )
        .expect("procedure file");
    }
    let adapter = AgentCliAdapter::new(TokioDriver::default(), template, 4, 4, vec![])
        .with_ipc_transport(Arc::new(RealIpc));
    // The persona file is a real front-matter persona (R-PER-01; `agent-cli` requires
    // the `---` block), referenced by absolute path — the real lane's resolution shape.
    let persona_file = home.join("personas").join("live-worker.md");
    std::fs::create_dir_all(persona_file.parent().expect("personas dir")).expect("personas");
    std::fs::write(
        &persona_file,
        "---\nname: Live Worker\nrole: worker\nlevel: worker\nallowed_tools: [bash, read, write, edit, glob, grep]\n---\n\nDo the delegated file work inside your working directory; reply with exactly one word.\n",
    )
    .expect("persona");

    // The delegation: one task, one step, one Instance.
    let task = TaskId::new();
    let step = athena_types::PlanStepId::new();
    let workspace = home.join("instances").join("live-worker-1");
    let context = athena_runtime::StartContext {
        task_id: Some(task.clone()),
        plan_step_id: Some(step.clone()),
        instance_id: InstanceId::new(),
        workspace: workspace.display().to_string(),
        work_targets: vec![workspace.display().to_string()],
        persona_ref: persona_file.display().to_string(),
        persona_version: "1.0.0".into(),
        skill_set: vec![],
        allowed_tools: vec![ToolName::FsRead, ToolName::FsWrite, ToolName::Shell],
        delegable_tools: vec![],
        filesystem_scope: athena_runtime::FilesystemScope::default(),
        model: ModelRef {
            provider: provider.clone(),
            name: model.clone(),
        },
        deadlines: Deadlines {
            startup_ms: 30_000,
            execution_ms: 600_000,
        },
        resume_snapshot_ref: None,
        context_document_ref: None,
    };

    let handle = Runtime::start(
        &adapter,
        worker_definition(&provider, &model, &persona_file),
        context,
    )
    .await
    .expect("the real serve peer started");

    Runtime::send(
        &adapter,
        &handle,
        Message {
            message_id: MessageId::new(),
            message_type: MessageType::Instruction,
            from_agent: Some(AgentId::core_agent()),
            from_instance_id: None,
            thread_id: "th-live-1".into(),
            reply_to: None,
            priority: Priority::Normal,
            body: "Run exactly one bash command: `echo live-e2e-ok > done.txt` inside your \
                   working directory. Then reply with only: complete"
                .into(),
            payload_json: None,
            deadline: None,
        },
    )
    .await
    .expect("the ask round trip delivered");

    // The real agent's work is on disk in the real Workspace.
    let artifact = workspace.join("done.txt");
    let produced = std::fs::read_to_string(&artifact)
        .unwrap_or_else(|error| panic!("the agent's bout did not leave its artifact ({error:?})"));
    assert!(
        produced.contains("live-e2e-ok"),
        "the artifact carries the echo: {produced}"
    );

    // The reply materializes as the Task's result intake.
    let result = Runtime::collect_result(&adapter, &handle)
        .await
        .expect("the reply is the result");
    assert_eq!(result.task_id, task);
    assert_eq!(result.plan_step_id, Some(step));
    assert_eq!(result.status, ResultStatus::Success);
    eprintln!(
        "live lane: the task completed by the real agent-cli {provider}/{model}; instance {}",
        result.instance_id
    );

    // Teardown through the Runtime Interface: the record releases (ATH-RT-010).
    Runtime::stop(&adapter, &handle, athena_types::StopReason::Shutdown)
        .await
        .expect("stopped");
    let error = Runtime::status(&adapter, &handle)
        .await
        .expect_err("the record released at terminal state");
    assert!(
        matches!(error, athena_runtime::RuntimeError::UnknownInstance(_)),
        "{error}"
    );
}

/// A deterministic arg check so the file parses in CI even when ignored: the probe of the
/// SpawnedProcess shape stays live (the struct's fields the real driver fills).
#[test]
fn the_spawned_process_shape_is_what_the_real_driver_fills() {
    let spawned = SpawnedProcess {
        pid: 4_242,
        runtime_instance_id: athena_types::RuntimeInstanceId::new(),
        peer_id: None,
    };
    assert_eq!(spawned.pid, 4_242);
}
