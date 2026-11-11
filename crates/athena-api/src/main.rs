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
    AthenaStore, Config, ConfigAuthMode, ConfigCliInputs, FilesystemScope, Log, Runtime,
    RuntimeInstanceRef, StartContext, load_config, start_core_agent,
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

/// The execution loop's cadence: how often a queued Task is dispatched or an in-flight
/// answer collected. Fast enough to feel responsive, slow enough that an idle resident
/// process costs nothing.
const EXECUTION_POLL: Duration = Duration::from_secs(2);

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
                resident_log(
                    Log::Error,
                    "api.args",
                    &format!("unknown argument {other} (--home, --config, --set)"),
                    resident_key(),
                    serde_json::json!({ "argument": other }),
                );
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
            resident_log(
                Log::Warn,
                "resident.stop",
                &format!("stop reported for {}: {}", record.instance_id, error),
                resident_key(),
                serde_json::json!({ "instance_id": record.instance_id.as_str() }),
            );
        }
    }
}
/// The resident's log discipline (X-39, ATH-OBS-001..005): every line a JSON envelope
/// through `athena_core::observability`'s `Logger`/`StdLogSink` — `at`, `level`,
/// `module`, `message`, the structured `fields`, and a `correlation_id` on every line
/// (ATH-OBS-003). The raw `eprintln!` writes they replace were the log-discipline gap
/// the third survey named.
static RESIDENT_SINK: athena_core::observability::StdLogSink =
    athena_core::observability::StdLogSink;

/// The correlation key the resident face carries: one minted per process, on first use,
/// so every non-Task-bound line is still on a chain (ATH-OBS-003/009).
fn resident_key() -> &'static String {
    static KEY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    KEY.get_or_init(|| {
        athena_core::digest(&format!(
            "athena-api-resident-{}",
            athena_core::now_rfc3339()
        ))
    })
}

/// One structured line on the resident chain: level, module, message, the correlation
/// key that carries it, and the structured fields — one JSON envelope per line (X-39).
fn resident_log(
    level: athena_core::observability::Log,
    module: &str,
    message: &str,
    correlation: &str,
    fields: serde_json::Value,
) {
    let _ = athena_core::observability::Logger::new(&RESIDENT_SINK, correlation.to_string()).log(
        level,
        module,
        message,
        fields,
        &athena_core::now_rfc3339(),
    );
}

/// Record one refused privileged request as the denial it is (`ATH-CORE-037`): the request
/// kind, the §20.1 class and the fault's own text, on the Task's chain. The record is the
/// audit the restarted context re-surfaces (`ATH-CORE-029`) and the source the
/// `athena_privileged_requests_denied_total` family counts (X-40), so the payload's
/// `request_kind` and `error_class` keys carry the vocabulary those readers expect.
///
/// A failure to record is logged and nothing else: the refusal has already been answered,
/// and losing its audit line must not take the resident down.
fn record_privileged_denial(
    store: &mut AthenaStore,
    task_id: &athena_types::TaskId,
    request_kind: &str,
    error_class: athena_core::ErrorClass,
    fault: &str,
    now: &str,
    correlation: &str,
) {
    let denial = NewEvent {
        event_id: athena_types::EventId::new(),
        schema_version: 1,
        event_type: "PrivilegedRequestDenied".into(),
        occurred_at: now.to_string(),
        task_id: Some(task_id.clone()),
        plan_step_id: None,
        agent_id: Some(athena_types::AgentId::core_agent()),
        instance_id: None,
        skill_id: None,
        correlation_id: correlation.to_string(),
        causation_id: None,
        actor: "core-agent".into(),
        payload_json: serde_json::json!({
            "request_kind": request_kind,
            "error_class": error_class.wire(),
            "fault": fault,
        })
        .to_string(),
    };
    if let Err(fault) = athena_core::EventRepository::append(store, &denial) {
        resident_log(
            Log::Error,
            "resident.execution",
            "recording the privileged-request denial failed",
            correlation,
            serde_json::json!({
                "task_id": task_id.as_str(),
                "request_kind": request_kind,
                "fault": fault.to_string()
            }),
        );
    }
}

fn fail(message: &str) -> ! {
    resident_log(
        Log::Error,
        "resident.fail",
        message,
        resident_key(),
        serde_json::json!({}),
    );
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
    let mut store = match AthenaStore::open(&db_path) {
        Ok(store) => store,
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
    let rows: Arc<Mutex<AthenaStore>> = Arc::new(Mutex::new(store));

    // 5. The token registry under var/ (ATH-SEC-005: hashes only on disk).
    let registry = match TokenRegistry::load(&home.var_dir().join("api-tokens.json")) {
        Ok(registry) => registry,
        Err(error) => fail(&format!("token registry unreadable: {error}")),
    };

    // The standing cell (X-29): the supervision poll publishes what it observes, the
    // `/status` route reads it — one writer, one reader, facts only (ATH-CORE-038).
    let standing: athena_api::StandingCell = athena_api::standing::new_cell();
    let state = AppState {
        registry: registry.shared(),
        mode,
        host: host.clone(),
        rows: Some(rows.clone()),
        standing: standing.clone(),
        config: config.clone(),
        tally: athena_api::routes::api_tally(),
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
                // The silence budget is the operator's stated tolerance, not a literal
                // (R-DIAG-04): `core_agent_restart.heartbeat_timeout` is validated against
                // `heartbeat_interval` (R-CFG-05) and read here so raising it in
                // `config.toml` actually takes effect.
                config.core_agent_restart.heartbeat_timeout,
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
        // X-15: the port is an explicit barrier — no auto-fallback to another port, so a
        // taken port must name itself and point at the configuration key that moves it
        // (`athena.api.port`). Booting on a silently different port would break every
        // client that resolved the configured one, and two boots would no longer be
        // distinguishable from one (the per-project isolation face, design §2.8).
        Err(error) => fail(&format!(
            "bind {host}:{port} failed: {error} — port {port} is already in use; \
             set `athena.api.port` to a free port for this project \
             (no automatic fallback, design §2.8)"
        )),
    };
    resident_log(
        Log::Info,
        "athena-api",
        &format!("ready on {host}:{port} (store: {})", db_path.display()),
        resident_key(),
        serde_json::json!({ "store": db_path.display().to_string() }),
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
        Ok(handle) => resident_log(
            Log::Info,
            "athena-api",
            &format!(
                "Core Agent resident (instance #0, runtime {})",
                handle.runtime_instance_id.as_str()
            ),
            resident_key(),
            serde_json::json!({ "runtime_instance_id": handle.runtime_instance_id.as_str() }),
        ),
        Err(error) => resident_log(
            Log::Error,
            "athena-api",
            &format!("Core Agent boot failed (escalation face, ATH-CORE-031): {error}"),
            resident_key(),
            serde_json::json!({}),
        ),
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
    let supervision = core_supervision(
        adapter.clone(),
        rows.clone(),
        control_surface,
        standing.clone(),
    );

    // 9b. The execution loop (X-26, R-DIAG-06): the queue a created Task lands in is
    //     drained here. Without this the resident process accepts work and never runs it
    //     — the gap the survey named first. The loop's semantics live in
    //     `athena_core::core_loop` (testable without a resident server); this task
    //     supplies only the cadence and the instance #0 handle it delivers to.
    let execution = core_execution(
        adapter.clone(),
        rows.clone(),
        athena_core::scheduler::SchedulerLimits::from_config(&config.scheduler),
        clarification_ttl_secs(&config),
        config.clone(),
    );

    // 9c. The maintenance cadence (X-34's Phase C): the retention sweeps run on the
    //     resident's own clock — bounded deletions whose authority is the configuration,
    //     never the request path (the same cadence/semantics split D-35 keeps).
    maintenance_cadence(rows.clone(), config.clone());

    // 9d. The skill-lifecycle cadence (X-35): the §12.5 async half — UPDATING rows
    //     complete through the Skill Manager's acquisition+validation leg and
    //     VALIDATING rows resolve through the Skill Validator's leg. The API's
    //     update/validate faces record the request (202); this cadence is what the
    //     record was for — the completion walks its §17.3 edges on the resident's
    //     own clock (the same cadence/semantics split D-35 keeps).
    skill_lifecycle_cadence(rows.clone(), config.clone());

    // 9e. The Core Agent heartbeat cadence (X-36/X-42): the typed `CoreAgentHeartbeat`
    //     Event and the Runtime Instance receipt are one beat on the configured interval
    //     (`core_agent_restart.heartbeat_interval`, default 20 s — ATH-OBS-011 /
    //     ATH-CORE-047, the cadence/semantics split D-35 keeps).
    core_agent_heartbeat_cadence(adapter.clone(), rows.clone(), config.clone());

    // 9f. The stall-detection cadence (X-37): the resident walks the two stall faces
    //     on its own clock — closed wait-graph cycles escalate immediately and the
    //     state-change silence line fires at the configured threshold — and the
    //     `TaskStalled` Event is the escalation record (ATH-OBS-012/013, the
    //     cadence/semantics split D-35 keeps).
    stall_detection_cadence(rows.clone(), config.clone());
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
        resident_log(
            Log::Info,
            "athena-api",
            "shutdown signal received; stopping instances",
            resident_key(),
            serde_json::json!({}),
        );
    });
    if let Err(error) = server.await {
        fail(&format!("server fault: {error}"));
    }
    supervision.abort();
    execution.abort();
    stop_all(adapter.as_ref());
    resident_log(
        Log::Info,
        "athena-api",
        "stopped",
        resident_key(),
        serde_json::json!({}),
    );
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

/// The clarification deadline, from the configuration (`timeout.clarification`, validated
/// `R-CFG-05`): an `ask_human` route opens its row with this TTL — silence never answers,
/// and the deadline's expiry fails the Task (`ATH-HITL-018`).
fn clarification_ttl_secs(config: &Config) -> u64 {
    config.timeout.clarification.as_secs()
}

/// The execution loop (X-26, `R-DIAG-06`): drain the `QUEUED` Tasks into instance #0 and
/// carry each answer back as the Task's Result.
///
/// This is the cadence only — every semantic step is `athena_core::core_loop`'s, so the
/// loop is testable without a resident server and the binary stays free of state-machine
/// knowledge. The store lock is never held across an `await`: each poll takes the lock to
/// reserve work (or to record an answer), releases it for the agent round trip, and takes
/// it again to persist — so a slow agent never blocks the API's request handling.
fn core_execution<D>(
    adapter: Arc<AgentCliAdapter<D>>,
    rows: Arc<Mutex<AthenaStore>>,
    limits: athena_core::scheduler::SchedulerLimits,
    clarification_ttl_secs: u64,
    config: athena_core::Config,
) -> tokio::task::JoinHandle<()>
where
    D: ProcessDriver + 'static,
{
    tokio::spawn(async move {
        // The instances tree a delegation composes its Workspace under (X-44): computed
        // once, since the home is fixed for the process's life (R-CFG-09 — configuration
        // is applied at startup).
        let instances_root = std::path::Path::new(&config.athena.home)
            .join("var")
            .join("instances");
        loop {
            tokio::time::sleep(EXECUTION_POLL).await;

            // Instance #0 must be resident before anything can be delegated to it. Its
            // absence is not this task's problem — the supervision poll owns restarting it.
            let Some(record) = adapter.core_record() else {
                continue;
            };
            let instance = RuntimeInstanceRef {
                runtime_type: RuntimeType::AgentCli,
                runtime_instance_id: record.runtime_instance_id.clone(),
                instance_id: record.instance_id.clone(),
            };
            let now = athena_types::now_rfc3339();

            // 1. Reserve one unit of work under the lock, then release it. The reservation
            //    passes the Scheduler first (X-26's survey face 1-6): the queue is drained
            //    through `tick_tasks`' admission decision — the §18.3 ceilings
            //    (`max_concurrent_tasks` / `max_concurrent_instances` /
            //    `max_instances_per_task`, ATH-SCHED-003/009/010) shape the drain, a
            //    refusal is observed and logged (ATH-SCHED-014) and the Task stays
            //    queued — instead of an unconditional `first()`.
            enum Step {
                Deliver(Box<athena_core::core_loop::PendingDelivery>),
                Intake(athena_types::TaskId),
                Idle,
            }
            let step = {
                let Ok(mut guard) = rows.lock() else {
                    continue;
                };
                let queued = athena_core::core_loop::queued_tasks(&guard).unwrap_or_default();
                // The admission facts, read from the store under the lock: every instance
                // row the inventory can count (Repository trait method through
                // `athena-core`'s re-export edge — the §32.2 boundary keeps
                // `athena-api → athena-core` only), and the running numerator. The Core
                // Agent's own Instance row carries no Task (`ATH-STATE-012`) and its slots
                // live outside the instance ceiling (ATH-SCHED-008), so a `task_id: None`
                // row is dropped from the projection instead of being inventoried against
                // a Task it does not belong to.
                let instances: Vec<athena_core::scheduler::InventoryInstance> =
                    athena_core::AgentRepository::list_all_instances(&*guard)
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(|row| {
                            row.task_id
                                .map(|task_id| athena_core::scheduler::InventoryInstance {
                                    task_id,
                                    state: row.state,
                                })
                        })
                        .collect();
                let running_ids: Vec<athena_types::TaskId> =
                    athena_core::core_loop::running_tasks(&guard)
                        .unwrap_or_default()
                        .iter()
                        .map(|task| task.id.clone())
                        .collect();
                // `queued_tasks` returns oldest submission first (created_at, id), so the
                // position in it *is* the FIFO rank the Scheduler ties on (ATH-SCHED-005).
                let request: Vec<athena_core::scheduler::QueuedTask> = queued
                    .iter()
                    .enumerate()
                    .map(|(order, task)| athena_core::scheduler::QueuedTask {
                        task_id: task.id.clone(),
                        priority: task.priority,
                        enqueue_order: order as u64,
                    })
                    .collect();
                let decision =
                    athena_core::scheduler::tick_tasks(&request, &running_ids, &instances, limits);
                // The observed-block face (ATH-SCHED-014): a ready Task a limit refused
                // is named, never silently held.
                if decision.admitted.is_empty() && !decision.blocked.is_empty() {
                    let (task, limit) = decision.blocked[0].clone();
                    resident_log(
                        Log::Info,
                        "resident.execution",
                        "a scheduler limit holds the task; stays queued",
                        resident_key(),
                        serde_json::json!({ "task_id": task.as_str(), "limit": limit.wire() }),
                    );
                }
                match decision.admitted.first() {
                    // The admitted face is a Task the loop must now reserve. The admitted
                    // list is decision order, so the first entry is the one admission.
                    Some(admitted) => {
                        let task_id = admitted.clone();
                        let queued_row = queued.iter().find(|task| task.id == task_id);
                        let Some(task) = queued_row else {
                            // Unreachable by construction (the admitted id comes from the
                            // queue just read), but the honest diverge is the loop's own.
                            continue;
                        };
                        let correlation =
                            athena_core::digest(&format!("deliver-{}-{now}", task.id.as_str()));
                        match athena_core::core_loop::prepare_delivery(
                            &mut guard,
                            &task.id,
                            &instance.instance_id,
                            &now,
                            &correlation,
                        ) {
                            Ok(pending) => Step::Deliver(Box::new(pending)),
                            Err(fault) => {
                                resident_log(
                                    Log::Error,
                                    "resident.execution",
                                    "reserving the task failed",
                                    &correlation,
                                    serde_json::json!({
                                        "task_id": task.id.as_str(),
                                        "fault": fault.to_string()
                                    }),
                                );
                                Step::Idle
                            }
                        }
                    }
                    None => match athena_core::core_loop::running_tasks(&guard)
                        .unwrap_or_default()
                        .first()
                    {
                        // An admitted-but-not-yet-answered reserve: intake of the
                        // delegation that is still in flight.
                        Some(task) => Step::Intake(task.id.clone()),
                        None => Step::Idle,
                    },
                }
            };

            match step {
                Step::Idle => {}
                // 2a. Deliver: the agent round trip runs with no store lock held.
                Step::Deliver(pending) => {
                    match athena_core::core_loop::deliver(adapter.as_ref(), &instance, &pending)
                        .await
                    {
                        Ok(()) => resident_log(
                            Log::Info,
                            "resident.execution",
                            "the task is dispatched to the Core Agent",
                            resident_key(),
                            serde_json::json!({ "task_id": pending.task_id.as_str() }),
                        ),
                        Err(fault) => {
                            let correlation = athena_core::digest(&format!(
                                "deliver-failed-{}-{now}",
                                pending.task_id.as_str()
                            ));
                            resident_log(
                                Log::Error,
                                "resident.execution",
                                "the task delivery failed",
                                &correlation,
                                serde_json::json!({
                                    "task_id": pending.task_id.as_str(),
                                    "fault": fault.to_string()
                                }),
                            );
                            if let Ok(mut guard) = rows.lock() {
                                let _ = athena_core::core_loop::fail_task(
                                    &mut guard,
                                    &pending.task_id,
                                    &fault.to_string(),
                                    &now,
                                    &correlation,
                                );
                            }
                        }
                    }
                }
                // 2b. Intake: read the answer off the stream with no lock held, then
                //     interpret it against the Core Control Protocol and persist under
                //     the lock (X-26's survey face 1-2: the twelve `CoreRequest` kinds are
                //     received and routed, not just recorded).
                Step::Intake(task_id) => {
                    let answer =
                        athena_core::core_loop::next_answer(adapter.as_ref(), &instance).await;
                    let Ok(answer) = answer else {
                        // No answer yet: the delegation is still in flight.
                        continue;
                    };
                    // The protocol face (ATH-CORE-033): a completion product, a human
                    // question, or one of the ten routed requests. A parse refusal is
                    // still an answer — it is recorded as the Result, and the Task's
                    // settlement then reflects the unmet protocol, never a silent drop.
                    let interpreted = match athena_core::core_loop::interpret_answer(&answer) {
                        Ok(value) => value,
                        Err(fault) => {
                            resident_log(
                                Log::Error,
                                "resident.execution",
                                "the answer failed the protocol gate",
                                resident_key(),
                                serde_json::json!({
                                    "task_id": task_id.as_str(),
                                    "fault": fault.to_string()
                                }),
                            );
                            continue;
                        }
                    };
                    let correlation =
                        athena_core::digest(&format!("intake-{}-{now}", task_id.as_str()));
                    // The answer a privileged request earns (ATH-CORE-034). It is collected
                    // under the lock and sent after the lock's own block ends: the store
                    // guard never spans an await, which is the same discipline the delivery
                    // leg keeps (a `drop` would not satisfy the future's Send bound — the
                    // guard's scope has to close).
                    let mut pending_answer: Option<athena_core::CoreResponse> = None;
                    {
                        let Ok(mut guard) = rows.lock() else {
                            continue;
                        };
                        match &interpreted {
                            // `ask_human`: open the clarification (RUNNING → WAITING_INPUT);
                            // the Task does not settle while a question waits — the answer
                            // endpoint resumes it, and a later intake records the settled
                            // product (`ATH-HITL-017..020`).
                            athena_core::core_loop::Interpreted::AskHuman { question, .. } => {
                                let deadline = athena_types::clock::format_rfc3339(
                                    std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map(|age| age.as_secs() + clarification_ttl_secs)
                                        .unwrap_or(clarification_ttl_secs),
                                    0,
                                );
                                match athena_core::core_loop::open_clarification(
                                    &mut guard,
                                    &task_id,
                                    question,
                                    &deadline,
                                    &now,
                                    &correlation,
                                ) {
                                    Ok(id) => resident_log(
                                        Log::Info,
                                        "resident.execution",
                                        "the task asks a human",
                                        &correlation,
                                        serde_json::json!({
                                            "task_id": task_id.as_str(),
                                            "clarification_id": id.as_str()
                                        }),
                                    ),
                                    Err(fault) => resident_log(
                                        Log::Error,
                                        "resident.execution",
                                        "clarifying the task failed",
                                        &correlation,
                                        serde_json::json!({
                                            "task_id": task_id.as_str(),
                                            "fault": fault.to_string()
                                        }),
                                    ),
                                }
                            }
                            // Any completion face — a `complete_task` request or the v1 bare
                            // prose product: record the Result and settle.
                            athena_core::core_loop::Interpreted::Completed { .. } => {
                                match athena_core::core_loop::ingest_answer(
                                    &mut guard,
                                    &task_id,
                                    &instance,
                                    &answer,
                                    &now,
                                    &correlation,
                                ) {
                                    Ok(received) => {
                                        let status = received.status;
                                        match athena_core::core_loop::settle_task(
                                            &mut guard,
                                            &task_id,
                                            status,
                                            &now,
                                            &correlation,
                                        ) {
                                            Ok(outcome) => resident_log(
                                                Log::Info,
                                                "resident.execution",
                                                "the task settled",
                                                &correlation,
                                                serde_json::json!({
                                                    "task_id": task_id.as_str(),
                                                    "outcome": format!("{outcome:?}"),
                                                    "result_id": received.result_id.as_str()
                                                }),
                                            ),
                                            Err(fault) => resident_log(
                                                Log::Error,
                                                "resident.execution",
                                                "settling the task failed",
                                                &correlation,
                                                serde_json::json!({
                                                    "task_id": task_id.as_str(),
                                                    "fault": fault.to_string()
                                                }),
                                            ),
                                        }
                                    }
                                    Err(fault) => resident_log(
                                        Log::Error,
                                        "resident.execution",
                                        "recording the answer failed",
                                        &correlation,
                                        serde_json::json!({
                                            "task_id": task_id.as_str(),
                                            "fault": fault.to_string()
                                        }),
                                    ),
                                }
                            }
                            // `submit_plan` (X-43): the plan is accepted, recorded and answered
                            // — and the Task **stays running**. A plan is the work's shape, not
                            // its product, so neither `ingest_answer` nor `settle_task` runs
                            // here: settling a Task on its own plan submission is the
                            // conflation this arm ends (`ATH-PLAN-001`).
                            athena_core::core_loop::Interpreted::PlanSubmitted {
                                task_id: plan_task,
                                base_revision,
                                steps,
                            } => {
                                match athena_core::plan_intake::accept_plan(
                                    &mut guard,
                                    plan_task,
                                    *base_revision,
                                    steps.clone(),
                                    &now,
                                ) {
                                    Ok(accepted) => {
                                        resident_log(
                                            Log::Info,
                                            "resident.execution",
                                            if accepted.first_revision {
                                                "the task's plan landed"
                                            } else {
                                                "the task's plan was revised"
                                            },
                                            &correlation,
                                            serde_json::json!({
                                                "task_id": plan_task.as_str(),
                                                "plan_id": accepted.plan_id.as_str(),
                                                "revision": accepted.revision,
                                                "step_count": accepted.step_count,
                                                "rationale_source": accepted.rationale_source.as_str()
                                            }),
                                        );
                                        // The answer the agent receives names what Core Host
                                        // minted; the confirmation Event's id is the request
                                        // identifier, so request, answer and record share one
                                        // key (ATH-OBS-009).
                                        pending_answer =
                                            Some(athena_core::plan_intake::acceptance_response(
                                                accepted.event_id.as_str(),
                                                &accepted,
                                            ));
                                    }
                                    Err(fault) => {
                                        // A refusal is recorded as the denial it is
                                        // (ATH-CORE-037) and answered with the specification's
                                        // envelope — the class and code are the refusal's own.
                                        record_privileged_denial(
                                            &mut guard,
                                            plan_task,
                                            "submit_plan",
                                            fault.class(),
                                            &fault.to_string(),
                                            &now,
                                            &correlation,
                                        );
                                        resident_log(
                                            Log::Warn,
                                            "resident.execution",
                                            "the task's plan was refused",
                                            &correlation,
                                            serde_json::json!({
                                                "task_id": plan_task.as_str(),
                                                "fault": fault.to_string()
                                            }),
                                        );
                                        pending_answer =
                                            Some(athena_core::plan_intake::refusal_response(
                                                &correlation,
                                                &correlation,
                                                &fault,
                                            ));
                                    }
                                }
                            }
                            // `delegate` (X-44): the admission rules answer, a row lands or
                            // a refusal is recorded, and the Task **stays running** — a
                            // delegation is work being handed down, not the Task's product.
                            // Starting the admitted Instance is the `CREATED → STARTING`
                            // edge, which the supervision/runtime side owns; the row this
                            // arm writes is what makes that edge reachable at all.
                            athena_core::core_loop::Interpreted::DelegationRequested(
                                delegation,
                            ) => {
                                match athena_core::delegation::accept_delegation(
                                    &mut guard,
                                    delegation,
                                    instances_root.as_path(),
                                    &now,
                                ) {
                                    Ok(accepted) => {
                                        resident_log(
                                            Log::Info,
                                            "resident.execution",
                                            if accepted.replayed {
                                                "the delegation replayed its claimed key"
                                            } else {
                                                "the delegation landed"
                                            },
                                            &correlation,
                                            serde_json::json!({
                                                "task_id": delegation.task_id.as_str(),
                                                "plan_step_id": delegation.plan_step_id.as_str(),
                                                "agent_id": delegation.agent_id.as_str(),
                                                "instance_id": accepted.instance_id.as_str(),
                                                "attempt": accepted.attempt,
                                                "delegation_depth": accepted.delegation_depth,
                                                "replayed": accepted.replayed
                                            }),
                                        );
                                        pending_answer =
                                            Some(athena_core::delegation::acceptance_response(
                                                accepted.event_id.as_str(),
                                                &accepted,
                                            ));
                                    }
                                    Err(fault) => {
                                        record_privileged_denial(
                                            &mut guard,
                                            &delegation.task_id,
                                            "delegate",
                                            fault.class(),
                                            &fault.to_string(),
                                            &now,
                                            &correlation,
                                        );
                                        resident_log(
                                            Log::Warn,
                                            "resident.execution",
                                            "the delegation was refused",
                                            &correlation,
                                            serde_json::json!({
                                                "task_id": delegation.task_id.as_str(),
                                                "plan_step_id": delegation.plan_step_id.as_str(),
                                                "fault": fault.to_string()
                                            }),
                                        );
                                        pending_answer =
                                            Some(athena_core::delegation::refusal_response(
                                                &correlation,
                                                &correlation,
                                                &fault,
                                            ));
                                    }
                                }
                            }
                            // A routed `CoreRequest`: `write_memory` is the one kind whose
                            // effect the Core Host applies (X-34 — `athena_core::write_memory`'s
                            // face writes under the resident's clock), the rest stay observed
                            // (the v1 model gives the answer itself as the product). Note that
                            // neither path answers the agent with a `CoreResponse`; only the
                            // plan arm above does, so ATH-CORE-034's every-request face is
                            // still open for the remaining kinds.
                            athena_core::core_loop::Interpreted::Requested { kind, request } => {
                                if let athena_core::protocol::CoreRequest::WriteMemory {
                                    tier,
                                    content,
                                } = request
                                {
                                    // The request carries no Task (ATH-CORE-033's own face);
                                    // the requesting Task anchors the write.
                                    let tier = match tier {
                                        athena_core::WritableMemoryTier::Working => {
                                            athena_core::write_memory::WritableTier::Working
                                        }
                                        athena_core::WritableMemoryTier::Long => {
                                            athena_core::write_memory::WritableTier::Long
                                        }
                                    };
                                    let requester =
                                        athena_core::write_memory::Requester::CoreAgentJudgement;
                                    match athena_core::write_memory::apply_write_memory(
                                        &mut guard,
                                        &requester,
                                        tier,
                                        content,
                                        &task_id,
                                        None,
                                        config.memory.long_term.max_records,
                                        &now,
                                        &correlation,
                                    ) {
                                        Ok(report) => resident_log(
                                            Log::Info,
                                            "resident.execution",
                                            "the task requested a write_memory",
                                            &correlation,
                                            serde_json::json!({
                                                "task_id": task_id.as_str(),
                                                "tier": format!("{:?}", report.tier),
                                                "record_id": report.record_id.as_str(),
                                                "scope_id": report.scope_id.as_str()
                                            }),
                                        ),
                                        Err(fault) => {
                                            // The refusal is recorded (ATH-CORE-037), not
                                            // swallowed; the record's fault text stays the
                                            // whole story.
                                            let denial = NewEvent {
                                                event_id: athena_types::EventId::new(),
                                                schema_version: 1,
                                                event_type: "PrivilegedRequestDenied".into(),
                                                occurred_at: now.to_string(),
                                                task_id: Some(task_id.clone()),
                                                plan_step_id: None,
                                                agent_id: Some(athena_types::AgentId::core_agent()),
                                                instance_id: None,
                                                skill_id: None,
                                                correlation_id: correlation.to_string(),
                                                causation_id: None,
                                                actor: "core-agent".into(),
                                                payload_json: serde_json::json!({
                                                    "request_kind": "write_memory",
                                                    "error_class": "validation_error",
                                                    "policy_id": "memory.tier",
                                                    "fault": fault.to_string(),
                                                })
                                                .to_string(),
                                            };
                                            if let Err(fault) = athena_core::EventRepository::append(
                                                &mut *guard,
                                                &denial,
                                            ) {
                                                resident_log(
                                                    Log::Error,
                                                    "resident.execution",
                                                    "recording the write_memory denial failed",
                                                    &correlation,
                                                    serde_json::json!({
                                                        "task_id": task_id.as_str(),
                                                        "fault": fault.to_string()
                                                    }),
                                                );
                                            }
                                            resident_log(
                                                Log::Warn,
                                                "resident.execution",
                                                "the write_memory request was refused",
                                                &correlation,
                                                serde_json::json!({
                                                    "task_id": task_id.as_str(),
                                                    "fault": fault.to_string()
                                                }),
                                            );
                                        }
                                    }
                                }
                                match athena_core::core_loop::ingest_answer(
                                    &mut guard,
                                    &task_id,
                                    &instance,
                                    &answer,
                                    &now,
                                    &correlation,
                                ) {
                                    Ok(received) => {
                                        let status = received.status;
                                        match athena_core::core_loop::settle_task(
                                            &mut guard,
                                            &task_id,
                                            status,
                                            &now,
                                            &correlation,
                                        ) {
                                            Ok(outcome) => resident_log(
                                                Log::Info,
                                                "resident.execution",
                                                &format!(
                                                    "task {} settled ({outcome:?}, result {}, request {kind} observed)",
                                                    task_id.as_str(),
                                                    received.result_id.as_str()
                                                ),
                                                &correlation,
                                                serde_json::json!({
                                                    "task_id": task_id.as_str(),
                                                    "outcome": format!("{outcome:?}"),
                                                    "result_id": received.result_id.as_str(),
                                                    "request_kind": kind
                                                }),
                                            ),
                                            Err(fault) => resident_log(
                                                Log::Error,
                                                "resident.execution",
                                                "settling the task failed",
                                                &correlation,
                                                serde_json::json!({
                                                    "task_id": task_id.as_str(),
                                                    "fault": fault.to_string()
                                                }),
                                            ),
                                        }
                                    }
                                    Err(fault) => resident_log(
                                        Log::Error,
                                        "resident.execution",
                                        "recording the answer failed",
                                        &correlation,
                                        serde_json::json!({
                                            "task_id": task_id.as_str(),
                                            "fault": fault.to_string()
                                        }),
                                    ),
                                }
                            }
                        }
                    }
                    // The lock is released before the answer travels: a `CoreResponse` is
                    // agent IO, and the store guard never crosses an await.
                    if let Some(response) = pending_answer {
                        match athena_core::plan_intake::control_response_message(
                            &task_id, &response,
                        ) {
                            Ok(message) => {
                                if let Err(fault) = adapter.send(&instance, message).await {
                                    // The record stands either way: the plan landed (or was
                                    // refused) and its Event says so. A send fault is the
                                    // agent's next read problem, logged, never a reason to
                                    // undo what was recorded.
                                    resident_log(
                                        Log::Warn,
                                        "resident.execution",
                                        "answering the privileged request failed",
                                        &correlation,
                                        serde_json::json!({
                                            "task_id": task_id.as_str(),
                                            "fault": fault.to_string()
                                        }),
                                    );
                                }
                            }
                            Err(fault) => resident_log(
                                Log::Error,
                                "resident.execution",
                                "serializing the privileged-request answer failed",
                                &correlation,
                                serde_json::json!({
                                    "task_id": task_id.as_str(),
                                    "fault": fault.to_string()
                                }),
                            ),
                        }
                    }
                }
            }
        }
    })
}

/// The supervision poll: detect instance #0's death, run the deterministic recovery
/// (checksum → rebuild → restart with re-injection), count the sliding window, escalate
/// on exhaustion. The *procedure* is deterministic; the restarted agent's behaviour is
/// not (ATH-CORE-049) — nothing here decides on the agent's behalf.
fn core_supervision<D>(
    adapter: Arc<AgentCliAdapter<D>>,
    rows: Arc<Mutex<AthenaStore>>,
    control_surface: Option<athena_core::ControlSurfaceChecksum>,
    standing: athena_api::StandingCell,
) -> tokio::task::JoinHandle<()>
where
    D: ProcessDriver + 'static,
{
    tokio::spawn(async move {
        let mut window = athena_core::RestartWindow::default_window();
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            let Some(record) = adapter.core_record() else {
                athena_api::CoreAgentStanding::publish(
                    &standing,
                    athena_api::CoreAgentStanding {
                        alive: false,
                        restarts: window.counted(u64_ms()) as u64,
                        restart_limit: window.limit(),
                        runtime_instance_id: String::new(),
                        observed_at: athena_types::now_rfc3339(),
                    },
                );
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
            // The poll publishes what it observed — the standing `GET /api/v1/status`
            // renders (X-29's Core-Agent standing face): the liveness verdict, the
            // window's consumed budget and its ceiling, and the resident instance id.
            athena_api::CoreAgentStanding::publish(
                &standing,
                athena_api::CoreAgentStanding {
                    alive: !dead,
                    restarts: window.counted(u64_ms()) as u64,
                    restart_limit: window.limit(),
                    runtime_instance_id: record.runtime_instance_id.as_str().to_string(),
                    observed_at: athena_types::now_rfc3339(),
                },
            );
            if !dead {
                continue;
            }
            resident_log(
                Log::Warn,
                "resident.supervision",
                "instance #0 process death observed (ATH-CORE-028)",
                resident_key(),
                serde_json::json!({}),
            );
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
                        Ok(handle) => resident_log(
                            Log::Info,
                            "resident.supervision",
                            &format!(
                                "Core Agent restarted (attempt {attempt}, runtime {})",
                                handle.runtime_instance_id.as_str()
                            ),
                            resident_key(),
                            serde_json::json!({ "attempt": attempt }),
                        ),
                        Err(error) => resident_log(
                            Log::Error,
                            "resident.supervision",
                            &format!("Core Agent restart attempt {attempt} failed: {error}"),
                            resident_key(),
                            serde_json::json!({ "attempt": attempt }),
                        ),
                    }
                }
                athena_core::Recovery::RetryNotCounted => {
                    resident_log(
                        Log::Info,
                        "resident.supervision",
                        "transient start failure; the next poll retries uncounted (ATH-CORE-045)",
                        resident_key(),
                        serde_json::json!({}),
                    );
                }
                athena_core::Recovery::Escalate(reason) => {
                    // The recovery reason already states the halt ("restart window
                    // exhausted; autonomous execution halts"), so the note appends nothing
                    // to it — the earlier doubled clause came from repeating it here
                    // (R-DIAG-01 finding (d)).
                    let reason = reason.trim_end_matches(|c: char| c == '.' || c.is_whitespace());
                    resident_log(
                        Log::Error,
                        "resident.escalation",
                        &format!("CoreAgentEscalated — {reason} (ATH-CORE-030)"),
                        resident_key(),
                        serde_json::json!({ "reason": reason }),
                    );
                    return;
                }
            }
        }
    })
}

/// The current time in Unix milliseconds (the window's clock argument).
fn u64_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(u64::MAX)
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

/// The maintenance polling interval (X-34's Phase C): the retained faces are pruned at
/// a one-minute cadence — bounded work, never in the request path.
const MAINTENANCE_POLL: Duration = Duration::from_secs(60);

/// The §12.5 async half's poll spacing: the in-flight completion legs walk every 2 s —
/// the same spacing as the Core Agent execution loop, since a settlement is one disk
/// gate away once the material is located.
const SKILL_LIFECYCLE_POLL: Duration = Duration::from_secs(2);

/// The Core Agent heartbeat's spacing fallback when the configured interval is degenerate
/// (zero): beating without pause would burn the Event log, so the interval is floored at
/// the specification's default — the configuration validator (`R-CFG-05`) rejects it first.
const HEARTBEAT_POLL_FALLBACK: Duration = Duration::from_secs(20);

/// One `duration`-ago RFC 3339 stamp (the cutoff arithmetic): now minus the retention.
fn cutoff(duration: std::time::Duration) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|age| age.as_secs())
        .unwrap_or(0);
    let past = now.saturating_sub(duration.as_secs());
    athena_types::clock::format_rfc3339(past, 0)
}

/// The retention sweep cadence (X-34's Phase C): the implemented deletions get their
/// production callers through `athena_core::maintenance::sweep_once` — Events
/// (`events.retention_days`, ATH-EVENT-014), clarifications (ATH-HITL-018), idempotency
/// keys (the repository's 7-day contract), delivery records (`messaging.dedup_retention`),
/// and the Instance `.aiprj` work areas (X-41, ATH-AIPRJ-029 — the areas outlive the
/// terminal state and age out with the Events they are referenced from, each deletion
/// gated by the step's recognition fence). The instances root passed here is the only
/// directory the area step may delete under. Every cycle's counts are recorded by
/// `record_swept` (the audit face), failures are logged and retried at the next tick — a
/// sweep fault never halts the resident.
fn maintenance_cadence(rows: Arc<Mutex<AthenaStore>>, config: athena_core::Config) {
    tokio::spawn(async move {
        let instances_root = std::path::Path::new(&config.athena.home)
            .join("var")
            .join("instances");
        loop {
            tokio::time::sleep(MAINTENANCE_POLL).await;
            let events_cutoff = cutoff(std::time::Duration::from_secs(
                config.events.retention_days * 24 * 60 * 60,
            ));
            let idempotency_cutoff = cutoff(std::time::Duration::from_secs(7 * 24 * 60 * 60));
            let deliveries_cutoff = cutoff(config.messaging.dedup_retention);
            let now = athena_types::now_rfc3339();
            let correlation = athena_core::digest(&format!("maintenance-{now}"));
            let Ok(mut guard) = rows.lock() else {
                continue;
            };
            match athena_core::maintenance::sweep_once(
                &mut guard,
                &now,
                &events_cutoff,
                &idempotency_cutoff,
                &deliveries_cutoff,
                Some(instances_root.as_path()),
            ) {
                Ok(report) => {
                    if report.events_pruned
                        + report.clarifications_expired
                        + report.idempotency_keys_pruned
                        + report.delivery_records_pruned
                        + report.approvals_expired
                        + report.aiprj_areas_deleted
                        + report.aiprj_areas_skipped
                        > 0
                    {
                        resident_log(
                            Log::Info,
                            "maintenance",
                            &format!(
                                "retention sweep removed events {}, clarifications {}, \
                                 idempotency keys {}, deliveries {}, aiprj areas {} \
                                 (areas left by the recognition fence: {}); \
                                 approvals expired as refusals {}",
                                report.events_pruned,
                                report.clarifications_expired,
                                report.idempotency_keys_pruned,
                                report.delivery_records_pruned,
                                report.aiprj_areas_deleted,
                                report.aiprj_areas_skipped,
                                report.approvals_expired
                            ),
                            &correlation,
                            serde_json::json!({
                                "events_pruned": report.events_pruned,
                                "clarifications_expired": report.clarifications_expired,
                                "idempotency_keys_pruned": report.idempotency_keys_pruned,
                                "delivery_records_pruned": report.delivery_records_pruned,
                                "approvals_expired": report.approvals_expired,
                                "aiprj_areas_deleted": report.aiprj_areas_deleted,
                                "aiprj_areas_skipped": report.aiprj_areas_skipped
                            }),
                        );
                        if let Err(fault) = athena_core::maintenance::record_swept(
                            &mut guard,
                            &report,
                            &now,
                            &correlation,
                        ) {
                            resident_log(
                                Log::Error,
                                "maintenance",
                                &format!("recording the sweep failed: {fault}"),
                                &correlation,
                                serde_json::json!({}),
                            );
                        }
                    }
                }
                Err(fault) => {
                    resident_log(
                        Log::Error,
                        "maintenance",
                        &format!("retention sweep faulted: {fault}; retries next tick"),
                        &correlation,
                        serde_json::json!({}),
                    );
                }
            }
        }
    });
}

/// The §17.3 in-flight poll (2 s, same cadence/semantics split D-35 keeps): the Skill
/// Manager's and the Skill Validator's §12.5 completion legs run on the resident's own
/// clock through `athena_core::skill_lifecycle::run_once`. A 202 request-recorded row
/// (`UPDATING`/`VALIDATING`) is settled here — located material, the P5-02 gate, and
/// the §17.3 completion Event (`SkillUpdated`/`SkillValidated`/`SkillValidationFailed`).
/// Faults are logged and retried at the next tick; a faulted tick never halts the
/// resident, and a quiet tick emits nothing.
fn skill_lifecycle_cadence(rows: Arc<Mutex<AthenaStore>>, config: Config) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(SKILL_LIFECYCLE_POLL).await;
            let now = athena_types::now_rfc3339();
            let correlation = athena_core::digest(&format!("skill-lifecycle-{now}"));
            let local_dir = PathBuf::from(&config.skills.local_dir);
            let registered_dir = PathBuf::from(&config.skills.dir);
            let verify_checksum = config.skills.verify_checksum;
            let Ok(mut guard) = rows.lock() else {
                continue;
            };
            match athena_core::skill_lifecycle::run_once(
                &mut guard,
                &local_dir,
                &registered_dir,
                verify_checksum,
                &now,
                &correlation,
            ) {
                Ok(report) => {
                    if !report.is_quiet() {
                        for line in &report.faults {
                            resident_log(
                                Log::Warn,
                                "skill.lifecycle",
                                line,
                                &correlation,
                                serde_json::json!({}),
                            );
                        }
                        resident_log(
                            Log::Info,
                            "skill.lifecycle",
                            "skill lifecycle settled",
                            &correlation,
                            serde_json::json!({
                                "updated": report.updated,
                                "update_failures": report.update_failures,
                                "revalidations": report.validated,
                                "validation_failures": report.validation_failures
                            }),
                        );
                    }
                }
                Err(fault) => {
                    resident_log(
                        Log::Error,
                        "skill.lifecycle",
                        &format!("skill lifecycle faulted: {fault}; retries next tick"),
                        &correlation,
                        serde_json::json!({}),
                    );
                }
            }
        }
    });
}

/// The Core Agent heartbeat cadence (X-36/X-42): one tick per
/// `core_agent_restart.heartbeat_interval` (ATH-OBS-011's default 20 s), beating through
/// `athena_core::heartbeat::beat_once` — the typed `CoreAgentHeartbeat` Event plus the
/// Runtime Instance receipt are one fact apiece on a single beat. Instance #0's residency
/// is observed per tick, never invented: when its record is absent the beat's Event still
/// emits (the Core Agent's own fact) and the receipt's refusal rides the report as a note.
/// Faults are logged and retried at the next tick — a faulted beat never halts the
/// resident.
fn core_agent_heartbeat_cadence<D>(
    adapter: Arc<AgentCliAdapter<D>>,
    rows: Arc<Mutex<AthenaStore>>,
    config: Config,
) -> tokio::task::JoinHandle<()>
where
    D: ProcessDriver + 'static,
{
    let interval = config
        .core_agent_restart
        .heartbeat_interval
        .max(HEARTBEAT_POLL_FALLBACK);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            let now = athena_types::now_rfc3339();
            // The resident's own runtime id, observed per tick: the supervision poll may
            // restart instance #0 between beats, so the receipt's row key is never cached.
            let runtime_instance = adapter
                .core_record()
                .map(|record| record.runtime_instance_id);
            let iteration = {
                let Ok(guard) = rows.lock() else {
                    continue;
                };
                match athena_core::heartbeat::observed_iteration(&guard) {
                    Ok(count) => count,
                    Err(fault) => {
                        resident_log(
                            Log::Error,
                            "heartbeat",
                            &format!("heartbeat read faulted: {fault}; retries next tick"),
                            resident_key(),
                            serde_json::json!({}),
                        );
                        continue;
                    }
                }
            };
            let Ok(mut guard) = rows.lock() else {
                continue;
            };
            match athena_core::heartbeat::beat_once(
                &mut guard,
                iteration,
                runtime_instance.as_ref(),
                &now,
            ) {
                Ok(report) => {
                    if let Some(note) = &report.receipt_note {
                        resident_log(
                            Log::Warn,
                            "heartbeat",
                            &format!("heartbeat receipt not recorded: {note}"),
                            resident_key(),
                            serde_json::json!({}),
                        );
                    }
                }
                Err(fault) => {
                    resident_log(
                        Log::Error,
                        "heartbeat",
                        &format!("heartbeat faulted: {fault}; retries next tick"),
                        resident_key(),
                        serde_json::json!({}),
                    );
                }
            }
        }
    })
}

/// The stall-detection cadence (X-37): the resident's own clock walks the two stall
/// faces each tick — the closed-wait-cycle face escalates immediately and the
/// state-change silence line fires at the configured threshold
/// (`core.iteration_timeout` x `scheduler.stall_detection_factor`, ATH-OBS-012) —
/// while the module owns the rule and the tick owns the clock (D-35). A tick fault
/// is logged and retried on the next tick: the cadence never halts the resident.
fn stall_detection_cadence(rows: Arc<Mutex<AthenaStore>>, config: Config) {
    tokio::spawn(async move {
        let threshold_ms = athena_core::stall_detection::stall_threshold_ms(
            u64::try_from(config.core.iteration_timeout.as_millis()).unwrap_or(1_800_000),
            config.scheduler.stall_detection_factor,
        );
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let now = athena_types::now_rfc3339();
            let Ok(mut guard) = rows.lock() else {
                continue;
            };
            match athena_core::stall_detection::scan_once(&mut guard, threshold_ms, &now) {
                Ok(report) => {
                    if !report.stalled.is_empty() {
                        for stalled in &report.stalled {
                            resident_log(
                                Log::Warn,
                                "stall.scan",
                                &format!("task {} stalled", stalled.task_id),
                                resident_key(),
                                serde_json::json!({ "reason": stalled.reason, "stall_seconds": 0 }),
                            );
                        }
                    }
                }
                Err(fault) => {
                    resident_log(
                        Log::Error,
                        "stall.scan",
                        &format!("stall scan faulted: {fault}; retries next tick"),
                        resident_key(),
                        serde_json::json!({}),
                    );
                }
            }
        }
    });
}
