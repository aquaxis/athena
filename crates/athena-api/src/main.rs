//! The resident Core API process (spec §4.3.2, design §2.1): the **composition root**
//! that links `athena-core` (Core Host) and the real `agent-cli` Adapter in one OS
//! process, boots instance #0 as part of the ATHENA boot sequence (ATH-CORE-021), and
//! serves `/api/v1` until `SIGINT` / `SIGTERM`.
//!
//! Boot sequence (spec §28, `ATH-CFG-003`'s fail-fast order): resolve configuration →
//! open the store (migrations verified) → record `ConfigurationLoaded` (secrets
//! excluded, R-CFG-02) → load the token registry → bind → ready → boot the Core Agent
//! (instance #0, `ATH-ACLI-008`'s `spawn` line).
//!
//! Shutdown (NFR-13): the signal stops every live Instance through the Runtime
//! Interface — instance #0 included, whose `STOPPED` is permitted only here
//! (ATH-STATE-024) — and the process exits once the store (closed on drop) is done.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use athena_api::{AppState, AuthMode, TokenRegistry, build_router};
use athena_core::NewEvent;
use athena_core::{
    Config, ConfigAuthMode, ConfigCliInputs, Database, FilesystemScope, Runtime,
    RuntimeInstanceRef, SqliteStore, StartContext, load_config, start_core_agent,
};
use athena_runtime_agent_cli::{
    AgentCliAdapter, Budgets, RealIpc, ShortTermRender, TokioDriver, adapter::ProcessDriver,
};
use athena_types::{
    AgentDefinition, AgentId, AgentLevel, Deadlines, EventId, InstanceId, ModelRef, RuntimeType,
    ToolName,
};

/// The supervision poll's cadence: death detection inside the NFR-14 budget.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// The command line: exactly the configuration inputs the config layer resolves
/// (`ATH-CFG-001`'s CLI layer) and nothing more.
struct Args {
    home: Option<PathBuf>,
    config: Option<PathBuf>,
    set: Vec<String>,
}

fn parse_args() -> Args {
    let mut args = Args {
        home: None,
        config: None,
        set: Vec::new(),
    };
    let mut words = std::env::args().skip(1);
    while let Some(word) = words.next() {
        match word.as_str() {
            "--home" => args.home = words.next().map(PathBuf::from),
            "--config" => args.config = words.next().map(PathBuf::from),
            "--set" => {
                if let Some(pair) = words.next() {
                    args.set.push(pair);
                }
            }
            other => {
                eprintln!("athena-api: unknown argument {other} (--home, --config, --set)");
                std::process::exit(70);
            }
        }
    }
    args
}

/// The Core Agent definition from the resolved configuration (`ATH-CORE-024`: producer
/// level; provider/model mandatory — validation refused anything unset before we got
/// here, ATH-AGENT-008).
fn core_definition(config: &Config) -> AgentDefinition {
    let agent = &config.core.agent;
    AgentDefinition {
        id: AgentId::core_agent(),
        name: "Core Agent".into(),
        level: AgentLevel::Producer,
        description: "ATHENA Core Agent (producer persona, instance #0)".into(),
        capabilities: vec![],
        persona_ref: agent.persona.clone(),
        persona_version: "1.0.0".into(),
        allowed_tools: agent.allowed_tools.clone(),
        delegable_tools: agent.delegable_tools.clone(),
        default_skills: vec![],
        runtime: RuntimeType::AgentCli,
        model: ModelRef {
            provider: agent.provider.clone(),
            name: agent.model.clone(),
        },
        // The delegation-depth ceiling (ATH-HIER-005's default 4); the value the
        // generated `[spawn] max_depth` floors at (ATH-ACLI-009).
        max_delegation_depth: 4,
        dynamic: false,
    }
}

/// The Core Agent's boot context (ATH-CORE-020: session-scoped, no Task bound;
/// ATH-OPS-009: cwd is its dedicated Workspace).
fn core_context(config: &Config) -> StartContext {
    let agent = &config.core.agent;
    StartContext {
        task_id: None,
        plan_step_id: None,
        instance_id: InstanceId::new(),
        workspace: agent.workspace.clone(),
        work_targets: vec![],
        persona_ref: agent.persona.clone(),
        persona_version: "1.0.0".into(),
        skill_set: vec![],
        allowed_tools: agent.allowed_tools.clone(),
        delegable_tools: agent.delegable_tools.clone(),
        filesystem_scope: FilesystemScope::default(),
        model: ModelRef {
            provider: agent.provider.clone(),
            name: agent.model.clone(),
        },
        deadlines: Deadlines {
            startup_ms: u64::try_from(config.timeout.startup.as_millis()).unwrap_or(30_000),
            execution_ms: u64::try_from(config.timeout.execution.as_millis()).unwrap_or(600_000),
        },
        resume_snapshot_ref: None,
        context_document_ref: None,
    }
}

/// Stop every live Instance through the Runtime Interface — instance #0 included,
/// whose `STOPPED` is the only legitimate route out of residency (ATH-STATE-024), and
/// the subordinates release their resources at terminal state (ATH-RT-010).
fn stop_all<D: ProcessDriver + 'static>(adapter: &AgentCliAdapter<D>) {
    let mut records = Vec::new();
    if let Some(record) = adapter.core_record() {
        records.push(record);
    }
    records.extend(adapter.subordinate_records());
    for record in records {
        let reference = RuntimeInstanceRef {
            runtime_type: RuntimeType::AgentCli,
            runtime_instance_id: record.runtime_instance_id.clone(),
            instance_id: record.instance_id.clone(),
        };
        if let Err(error) = futures::executor::block_on(Runtime::stop(
            adapter,
            &reference,
            athena_types::StopReason::Shutdown,
        )) {
            eprintln!(
                "athena-api: stop reported for {}: {error}",
                record.instance_id
            );
        }
    }
}

fn fail(message: &str) -> ! {
    eprintln!("athena-api: {message}");
    std::process::exit(70);
}

#[tokio::main]
async fn main() {
    let args = parse_args();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // 1. Configuration: resolve, validate, compile the policy — abort on any violation
    //    (ATH-CFG-003, exit 70 the internal-error face).
    let loaded = match load_config(
        cwd,
        &ConfigCliInputs {
            home: args.home.clone(),
            config: args.config.clone(),
            set: args.set.clone(),
        },
    ) {
        Ok(loaded) => loaded,
        Err(error) => fail(&format!("configuration failed: {error}")),
    };
    let config = loaded.config.clone();
    let home = loaded.home.clone();

    // 2. The loopback rule belt at the bind (ATH-CFG-006; validation already refuses
    //    the combination — the bind re-checks the effective host).
    let host = config.athena.api.host.clone();
    let port = config.athena.api.port;
    let mode = match config.athena.api.auth.mode {
        ConfigAuthMode::Token => AuthMode::Token,
        ConfigAuthMode::None => {
            if host != "127.0.0.1" && host != "localhost" {
                fail("auth.mode none is permitted only on 127.0.0.1 (ATH-CFG-006)");
            }
            AuthMode::None
        }
    };

    // 3. The store: open (or create) with migrations verified; a newer-than-build
    //    database aborts (ATH-PERS-010). The store is authoritative (ATH-PERS-002).
    let db_path = PathBuf::from(&config.athena.data.database);
    let mut store = match Database::open(&db_path) {
        Ok(db) => SqliteStore::new(db),
        Err(error) => fail(&format!(
            "store open failed at {}: {error}",
            db_path.display()
        )),
    };

    // 4. `ConfigurationLoaded` — the resolved effective configuration, secrets excluded
    //    (R-CFG-02, ATH-CFG-004), appended in the same store everything else lives in.
    let snapshot = athena_core::config_redacted(&loaded.effective);
    let configuration_loaded = NewEvent {
        event_id: EventId::new(),
        schema_version: 1,
        event_type: "ConfigurationLoaded".into(),
        occurred_at: athena_types::now_rfc3339(),
        task_id: None,
        plan_step_id: None,
        agent_id: None,
        instance_id: None,
        skill_id: None,
        correlation_id: athena_core::digest(&format!("boot-{}", home.project_root().display())),
        causation_id: None,
        actor: "system".into(),
        payload_json: serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".into()),
    };
    if let Err(error) = athena_core::EventRepository::append(&mut store, &configuration_loaded) {
        fail(&format!("recording ConfigurationLoaded failed: {error}"));
    }

    // From here the store lives behind the shared handle the API's handlers and the
    // supervision poll lock.
    let rows: Arc<Mutex<SqliteStore>> = Arc::new(Mutex::new(store));

    // 5. The token registry under var/ (ATH-SEC-005: hashes only on disk).
    let registry = match TokenRegistry::load(&home.var_dir().join("api-tokens.json")) {
        Ok(registry) => registry,
        Err(error) => fail(&format!("token registry unreadable: {error}")),
    };

    let state = AppState {
        registry: registry.shared(),
        mode,
        host: host.clone(),
        rows: Some(rows.clone()),
    };

    // 6. The composition root's Adapter link: the real process driver, the template
    //    origin (`<ATHENA_HOME>/var/agent-cli-template/`, ATH-ACLI-013), the
    //    scheduler-facing spawn floors (ATH-ACLI-009), the resolved short-term facts
    //    (P4-07), and the real IPC transport (the verified 0.29.0 `ask` round trip).
    let adapter = Arc::new(
        AgentCliAdapter::with_budgets(
            TokioDriver::default(),
            home.var_dir().join("agent-cli-template"),
            Budgets::new(
                config.timeout.startup,
                config.timeout.execution,
                config.timeout.stop,
                Duration::from_secs(60),
            ),
            Arc::new(athena_runtime_agent_cli::WallClock),
            4,
            config.scheduler.max_instances_per_task,
            Vec::<ToolName>::new(),
        )
        .with_short_term(ShortTermRender {
            max_tokens: config.memory.short_term_max_tokens,
            tokenizer: config.memory.tokenizer.clone(),
        })
        .with_ipc_transport(Arc::new(RealIpc)),
    );

    // 7. Bind before instance #0 boots: readiness (NFR-11) is the API answering, and the
    //    Core Agent's registration must not hold the listener hostage.
    let listener = match tokio::net::TcpListener::bind((host.as_str(), port)).await {
        Ok(listener) => listener,
        Err(error) => fail(&format!("bind {host}:{port} failed: {error}")),
    };
    eprintln!(
        "athena-api: ready on {host}:{port} (store: {})",
        db_path.display()
    );

    // 8. Instance #0 boots as part of the ATHENA boot sequence (ATH-CORE-021): the
    //    spawn is detached and self-registers, so this returns at process birth. The
    //    boot also publishes the control-surface checksum every restart verifies
    //    (ATH-CORE-044).
    let boot_config = config.clone();
    match start_core_agent(
        adapter.as_ref(),
        core_definition(&boot_config),
        core_context(&boot_config),
    )
    .await
    {
        Ok(handle) => eprintln!(
            "athena-api: Core Agent resident (instance #0, runtime {})",
            handle.runtime_instance_id.as_str()
        ),
        Err(error) => {
            eprintln!("athena-api: Core Agent boot failed (escalation face, ATH-CORE-031): {error}")
        }
    }

    // The checksum captures the *placed* surface (ATH-CORE-044): computed after the
    // boot has written config.toml / persona.md / commands/, so a restart verifies
    // against what the agent actually received and a pre-placement empty hash never
    // masquerades as the published basis.
    let control_surface =
        core_workspace_agent_cli(&boot_config).map(|dir| control_surface_checksum(&dir));

    // 9. The Core Agent Session's supervision poll (§7.5.3; NFR-14): the process-level
    //    truth observed every few seconds; a dead instance #0 walks the deterministic
    //    recovery — checksum verify, context rebuild, restart with re-injection — and
    //    the sliding window (3 / h) escalates beyond it (ATH-CORE-030).
    let supervision = core_supervision(adapter.clone(), rows.clone(), control_surface);

    // 10. Serve until the signal; then the safe-shutdown sequence (NFR-13).
    let server = axum::serve(listener, build_router(state)).with_graceful_shutdown(async {
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("SIGTERM handler");
            tokio::select! {
                _ = ctrl_c => {},
                _ = term.recv() => {},
            }
        }
        #[cfg(not(unix))]
        {
            let _ = ctrl_c.await;
        }
        eprintln!("athena-api: shutdown signal received; stopping instances");
    });
    if let Err(error) = server.await {
        fail(&format!("server fault: {error}"));
    }
    supervision.abort();
    stop_all(adapter.as_ref());
    eprintln!("athena-api: stopped");
}

/// The Core Agent's `.agent-cli/` control surface under its Workspace.
fn core_workspace_agent_cli(config: &Config) -> Option<PathBuf> {
    let workspace = PathBuf::from(&config.core.agent.workspace);
    if workspace.is_absolute() {
        Some(workspace.join(".agent-cli"))
    } else {
        None
    }
}

/// The control-surface checksum over the placed files, order-independent
/// (`ATH-CORE-044`; the boot publishes it, every restart re-verifies it).
fn control_surface_checksum(agent_cli_dir: &Path) -> athena_core::ControlSurfaceChecksum {
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for relative in ["config.toml", "persona.md"] {
        let path = agent_cli_dir.join(relative);
        if let Ok(bytes) = std::fs::read(&path) {
            entries.push((relative.into(), bytes));
        }
    }
    if let Ok(list) = std::fs::read_dir(agent_cli_dir.join("commands")) {
        for file in list.flatten() {
            if let Ok(bytes) = std::fs::read(file.path()) {
                entries.push((
                    format!("commands/{}", file.file_name().to_string_lossy()),
                    bytes,
                ));
            }
        }
    }
    let borrowed: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    athena_core::ControlSurfaceChecksum::of(&borrowed)
}

/// The supervision poll: detect instance #0's death, run the deterministic recovery
/// (checksum → rebuild → restart with re-injection), count the sliding window, escalate
/// on exhaustion. The *procedure* is deterministic; the restarted agent's behaviour is
/// not (ATH-CORE-049) — nothing here decides on the agent's behalf.
fn core_supervision<D>(
    adapter: Arc<AgentCliAdapter<D>>,
    rows: Arc<Mutex<SqliteStore>>,
    control_surface: Option<athena_core::ControlSurfaceChecksum>,
) -> tokio::task::JoinHandle<()>
where
    D: ProcessDriver + 'static,
{
    tokio::spawn(async move {
        let mut window = athena_core::RestartWindow::default_window();
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            let Some(record) = adapter.core_record() else {
                continue;
            };
            let reference = RuntimeInstanceRef {
                runtime_type: RuntimeType::AgentCli,
                runtime_instance_id: record.runtime_instance_id.clone(),
                instance_id: record.instance_id.clone(),
            };
            let dead = match Runtime::status(adapter.as_ref(), &reference).await {
                Ok(status) => !status.is_alive(),
                Err(_) => true,
            };
            if !dead {
                continue;
            }
            eprintln!("athena-api: instance #0 process death observed (ATH-CORE-028)");
            // 1. The control-surface checksum (ATH-CORE-044): a mismatch escalates —
            //    nothing restarts through a tampered surface.
            let surface_ok = control_surface
                .as_ref()
                .map(|placed| {
                    let workspace = PathBuf::from(&record.workspace);
                    let live = control_surface_checksum(&workspace.join(".agent-cli"));
                    athena_core::verify_surface(placed, &live)
                })
                .unwrap_or(Ok(()));
            // 2. The deterministic decision: the sliding window (3 / h); transient and
            //    resource-shortage start failures retry uncounted (ATH-CORE-045).
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
                .unwrap_or(u64::MAX);
            let decision = athena_core::recovery_decision(
                surface_ok,
                athena_core::StartFailureKind::Counted,
                &mut window,
                now_ms,
            );
            match decision {
                athena_core::Recovery::Restart { attempt } => {
                    // 3. Rebuild the context document per non-terminal Task from Working
                    //    Memory + Events (ATH-CORE-029), written where the restarted
                    //    agent reads it; references only, no prose invented
                    //    (ATH-EVENT-007).
                    let mut context_refs: Vec<String> = Vec::new();
                    if let Ok(mut guard) = rows.lock() {
                        let store = &mut *guard;
                        let tasks = athena_core::TaskRepository::list_non_terminal(store)
                            .unwrap_or_default();
                        for task in tasks {
                            if let Ok(document) =
                                athena_core::ContextDocument::rebuild(store, &task.id)
                            {
                                let path = PathBuf::from(&record.workspace)
                                    .join(format!("context-{}.json", task.id.as_str()));
                                if let Ok(json) = serde_json::to_string_pretty(&document) {
                                    let _ = std::fs::write(&path, json);
                                    context_refs.push(path.display().to_string());
                                }
                            }
                        }
                    }
                    let mut context = core_context(&recovered_config(&record));
                    context.context_document_ref = context_refs.first().cloned();
                    match start_core_agent(adapter.as_ref(), record.definition.clone(), context)
                        .await
                    {
                        Ok(handle) => eprintln!(
                            "athena-api: Core Agent restarted (attempt {attempt}, runtime {})",
                            handle.runtime_instance_id.as_str()
                        ),
                        Err(error) => eprintln!(
                            "athena-api: Core Agent restart attempt {attempt} failed: {error}"
                        ),
                    }
                }
                athena_core::Recovery::RetryNotCounted => {
                    eprintln!(
                        "athena-api: transient start failure; the next poll retries uncounted (ATH-CORE-045)"
                    );
                }
                athena_core::Recovery::Escalate(reason) => {
                    eprintln!(
                        "athena-api: CoreAgentEscalated — {reason}; autonomous execution halts (ATH-CORE-030)"
                    );
                    return;
                }
            }
        }
    })
}

/// The configuration slice a restart rebuilds from: the record carries the definition's
/// coordinates; the workspace fact re-reads from the definition itself.
fn recovered_config(record: &athena_runtime_agent_cli::Record) -> Config {
    let mut config = Config::default();
    config.core.agent.provider = record.definition.model.provider.clone();
    config.core.agent.model = record.definition.model.name.clone();
    config.core.agent.workspace = record.workspace.display().to_string();
    config.core.agent.persona = record.definition.persona_ref.clone();
    config
}
