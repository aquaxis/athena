//! The Adapter skeleton (P3-01): the seven Runtime Interface operations realized over a
//! pluggable [`ProcessDriver`] — the real `tokio::process` spawn of `agent-cli` is the
//! default driver's command builder (P3-02…P3-04 land with the process-manager rows),
//! and the contract suite drives both the real shape and the driver double.

use crate::generation::generate_agent_cli;
use crate::manager::{Budgets, Clock, ProcessManager, Record, WallClock};
use crate::workspace::{WorkspaceFault, prepare_workspace};
use athena_runtime::interface::Runtime;
use athena_runtime::message::Message;
use athena_runtime::status::{IsolationBoundary, RuntimeCapabilities, RuntimeInstanceStatus};
use athena_runtime::{RuntimeError, RuntimeInstanceRef, StartContext};
use athena_types::{
    AgentDefinition, ExecutionResult, ResultId, ResultStatus, RuntimeInstanceId, RuntimeType,
    StopReason, ToolName,
};
use std::path::PathBuf;
use std::sync::Arc;

/// The facade a ProcessManager supplies (P3-02…P3-04's rows): spawn given the workspace
/// and command, alive-check, exit-code visibility, termination.
pub trait ProcessDriver: Send + Sync {
    /// Spawn the agent command rooted at the workspace; the handle id is the OS pid (or
    /// the driver's own counter for test doubles).
    ///
    /// # Errors
    /// Spawn failures surface whole.
    fn spawn(
        &self,
        workspace: &std::path::Path,
        command: &str,
    ) -> Result<SpawnedProcess, RuntimeError>;

    /// Whether the process is still running.
    #[must_use]
    fn is_alive(&self, pid: u32) -> bool;

    /// The exit code, once terminated.
    #[must_use]
    fn exit_code(&self, pid: u32) -> Option<i32>;

    /// Terminate the process (graceful `SIGTERM`; force-kill is the caller's
    /// `timeout.stop` overrun, ATH-FAIL-015).
    ///
    /// # Errors
    /// Kill failures surface whole.
    fn terminate(&self, pid: u32) -> Result<(), RuntimeError>;

    /// The `timeout.stop` overrun's decisive end (SIGKILL; ATH-FAIL-015) — called by the
    /// process manager, never together with the graceful signal.
    ///
    /// # Errors
    /// Kill failures surface whole.
    fn force_kill(&self, pid: u32) -> Result<(), RuntimeError>;
}

/// One spawned process's identifiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnedProcess {
    /// The OS pid the driver reports.
    pub pid: u32,
    /// The Runtime Instance id minted from it (`ri-…`).
    pub runtime_instance_id: RuntimeInstanceId,
    /// The peer agent-id the detached self-registration printed (`agent-…`), captured
    /// from the spawn launcher's stdout. Duplicate display names make *name* addressing
    /// ambiguous (0.29.0 verified: stale entries linger until pruned), so the IPC
    /// transport addresses the peer by this id.
    pub peer_id: Option<String>,
}

/// The `agent-cli` Runtime Adapter (spec §14.4).
///
/// The per-Instance bookkeeping — pid, exit code, heartbeat, deadlines, stop escalation,
/// restart reconciliation — is the [`ProcessManager`]'s (design §5.4); the Adapter is the
/// Runtime Interface projection over it.
pub struct AgentCliAdapter<D: ProcessDriver> {
    manager: ProcessManager<D>,
    template_dir: PathBuf,
    max_spawn_depth: u64,
    max_spawn_children: u64,
    /// The tool universe the Adapter projects into `[tools] enabled`
    /// (design §5.3.1's mapping; wired with the Process Manager rows).
    #[allow(dead_code)]
    tool_universe: Vec<ToolName>,
    /// The resolved short-term rendering facts (P4-07): `memory.short_term_max_tokens`
    /// and `memory.tokenizer`, passed through the boot wiring — never implicit.
    short_term: crate::generation::ShortTermRender,
    /// The IPC transport (the IPC row): `None` keeps the deterministic file-protocol
    /// delivery the CI lane plays; `Some(RealIpc)` delivers over the peer registry.
    transport: Option<std::sync::Arc<dyn IpcTransport>>,
}

impl<D: ProcessDriver + 'static> AgentCliAdapter<D> {
    /// The Adapter with its driver and the template origin, on wall-clock budgets.
    #[must_use]
    pub fn new(
        driver: D,
        template_dir: PathBuf,
        max_spawn_depth: u64,
        max_spawn_children: u64,
        tool_universe: Vec<ToolName>,
    ) -> Self {
        let budgets = Budgets::new(
            std::time::Duration::from_secs(30),
            std::time::Duration::from_secs(600),
            std::time::Duration::from_secs(60),
            std::time::Duration::from_secs(60),
        );
        Self::with_budgets(
            driver,
            template_dir,
            budgets,
            Arc::new(WallClock),
            max_spawn_depth,
            max_spawn_children,
            tool_universe,
        )
    }

    /// Override the short-term rendering facts with the boot wiring's resolved
    /// configuration (`memory.short_term_max_tokens` / `memory.tokenizer`).
    #[must_use]
    pub fn with_short_term(mut self, short_term: crate::generation::ShortTermRender) -> Self {
        self.short_term = short_term;
        self
    }

    /// The Adapter with explicit budgets and clock (the deterministic test surface).
    #[must_use]
    pub fn with_budgets(
        driver: D,
        template_dir: PathBuf,
        budgets: Budgets,
        clock: Arc<dyn Clock>,
        max_spawn_depth: u64,
        max_spawn_children: u64,
        tool_universe: Vec<ToolName>,
    ) -> Self {
        Self {
            manager: ProcessManager::new(driver, budgets, clock),
            template_dir,
            max_spawn_depth,
            max_spawn_children,
            tool_universe,
            short_term: crate::generation::ShortTermRender::default(),
            transport: None,
        }
    }

    /// Select the real IPC transport (the composition root's choice): every
    /// `instruction` delivery becomes a real `agent-cli ask` round trip, and the
    /// agent's reply lands in the Workspace outbox for `receive` / `collect_result`.
    #[must_use]
    pub fn with_ipc_transport(mut self, transport: std::sync::Arc<dyn IpcTransport>) -> Self {
        self.transport = Some(transport);
        self
    }

    /// The tracked Core Agent record, when instance #0 is resident (P3-15's supervision
    /// read side: the Core Agent Session monitors this, never the subordinate list).
    #[must_use]
    pub fn core_record(&self) -> Option<Record> {
        self.manager.core_live_record()
    }

    /// One tracked record by its Runtime Instance id (any role).
    #[must_use]
    pub fn process_record(&self, runtime_instance_id: &str) -> Option<Record> {
        self.manager.record(runtime_instance_id)
    }

    /// Every tracked subordinate record — instance #0 excluded (P3-15's isolation:
    /// the subordinate flow and the Core Agent Session read disjoint record sets even
    /// inside one manager, and each process runs on an independent OS pid so neither
    /// side's crash can touch the other's record).
    #[must_use]
    pub fn subordinate_records(&self) -> Vec<Record> {
        self.manager.subordinate_records()
    }
}

/// The command builder (P3-02's spawn line): `agent-cli` runs with cwd = workspace, the
/// per-Instance `.agent-cli/` overlay supplying the definition, per the Phase-0
/// verification (the cwd overlay is sufficient, D-6).
#[must_use]
pub fn command_for(definition: &AgentDefinition) -> String {
    let (program, args) = argv_for(definition);
    format!("{program} {}", args.join(" "))
}

/// The subordinate spawn argv (the delegation line) — the real driver builds a
/// `Command` from this, never a shell string. The line is the **detached `spawn`
/// launcher** (0.29.0 verified 2026-10-18): the launcher exits within its registration
/// window once the child has self-registered as an IPC peer, and its stdout carries the
/// peer agent-id the IPC transport addresses; the Process Manager tracks the detached
/// child's pid, so `SIGTERM` (`timeout.stop`) reaches it. `run` is the REPL and offers
/// no non-interactive prompt (`ATH-ACLI-016`); the cwd is the driver's `current_dir` —
/// `agent-cli` has no `--dir` flag, and the per-Instance `.agent-cli/` overlay supplies
/// the definition (D-6).
#[must_use]
pub fn argv_for(definition: &AgentDefinition) -> (String, Vec<String>) {
    (
        "agent-cli".into(),
        vec![
            "spawn".into(),
            "--persona".into(),
            ".agent-cli/persona.md".into(),
            "--name".into(),
            definition.id.as_str().into(),
        ],
    )
}

/// The Core Agent's `spawn`-mode argv (P3-15; `ATH-ACLI-008`).
///
/// Instance #0 is a resident, session-scoped peer: verified against `agent-cli` 0.29.0
/// (2026-10-18) that `spawn` re-execs `serve` detached (PPID 1, own process group),
/// inherits cwd — so the `.agent-cli/` cwd overlay survives — and self-registers into the
/// peer registry the IPC `send`/`ask` path resolves, within the registration window.
/// `run` is the REPL and offers no non-interactive prompt (`ATH-ACLI-016`), so it is not
/// the Core Agent's execution line either. The cwd is the driver's `current_dir`; the
/// persona option is `--persona` (0.29.0's canonical flag).
#[must_use]
pub fn argv_for_core(definition: &AgentDefinition) -> (String, Vec<String>) {
    (
        "agent-cli".into(),
        vec![
            "spawn".into(),
            "--persona".into(),
            ".agent-cli/persona.md".into(),
            "--name".into(),
            definition.id.as_str().into(),
        ],
    )
}

/// The Core Agent's command form behind [`argv_for_core`].
#[must_use]
pub fn command_for_core(definition: &AgentDefinition) -> String {
    let (program, args) = argv_for_core(definition);
    format!("{program} {}", args.join(" "))
}

#[async_trait::async_trait]
impl<D: ProcessDriver + 'static> Runtime for AgentCliAdapter<D> {
    fn runtime_type(&self) -> RuntimeType {
        RuntimeType::AgentCli
    }

    async fn start(
        &self,
        definition: AgentDefinition,
        ctx: StartContext,
    ) -> Result<RuntimeInstanceRef, RuntimeError> {
        ctx.validate()?;
        // P3-16: aiprj readiness first — refuse read-side contamination, initialize only
        // when the areas are not ready (ATH-AIPRJ-024); P3-06: write areas only;
        // P3-09: .agent-cli generation.
        let prepared = prepare_workspace(PathBuf::from(&ctx.workspace).as_path(), true).map_err(
            |fault: WorkspaceFault| {
                if let crate::workspace::WorkspaceFault::Contaminated { .. } = fault {
                    RuntimeError::Validation(fault.to_string())
                } else {
                    RuntimeError::Transient(fault.to_string())
                }
            },
        )?;
        let base = athena_runtime::StartContext {
            deadlines: athena_types::Deadlines {
                startup_ms: 10_000,
                execution_ms: 600_000,
            },
            ..loop_base(&ctx)
        };
        let generated = crate::generation::render_config(
            &definition,
            &base.deadlines,
            self.max_spawn_depth,
            self.max_spawn_children,
            &self.short_term,
        )
        .map_err(|fault| RuntimeError::Validation(fault.to_string()))?;
        // The persona file rides the definition's `persona_ref` resolution (the Adapter
        // reads `<ATHENA_HOME>/personas/<persona_ref>` — the caller passes the bytes
        // through the workspace contract; the template origin's persona is a copy).
        // P3-10/P3-13: the placed persona carries the procedure files and work targets by
        // absolute path; the commands copy refuses a cwd-relative read origin.
        generate_agent_cli(
            &prepared.layout.agent_cli,
            &self.template_dir,
            // The placed persona.md is the *body* of the referenced persona: a
            // `persona_ref` that names a readable file (the config's resolved path,
            // `<ATHENA_HOME>/personas/<slug>.md`) is read here; a bare slug rides
            // verbatim as the body the deterministic tests use. The front matter rides
            // inside the body — `agent-cli` requires the YAML block (0.29.0 verified).
            &persona_body_of(&ctx.persona_ref),
            &ctx.work_targets,
            &generated,
        )
        .map_err(|fault| RuntimeError::Validation(fault.to_string()))?;
        // P3-15: the Core Agent's start line — a Task-less context naming
        // `athena-core-agent` routes to instance #0's `spawn`-mode residency; everything
        // else keeps the delegation (`run`) line.
        let handle = if ctx.task_id.is_none() && definition.id.is_core_agent() {
            self.manager.spawn_core_agent(&definition, &ctx)?
        } else {
            self.manager.spawn(&definition, &ctx)?
        };
        // P3-16: the workspace fact rides the record so Core Host can materialize the
        // `WorkspaceInitialized` Event (`ATH-AIPRJ-024`; emission is Core's, D-1).
        self.manager
            .set_workspace_initialized(handle.runtime_instance_id.as_str(), prepared.initialized);
        Ok(handle)
    }

    async fn send(
        &self,
        instance: &RuntimeInstanceRef,
        mut message: Message,
    ) -> Result<(), RuntimeError> {
        if !self
            .manager
            .is_running(instance.runtime_instance_id.as_str())?
        {
            return Err(RuntimeError::UnknownInstance(
                "the process exited; the Runtime reports, never fabricates".into(),
            ));
        }
        // P3-17: an `instruction` reaches the agent wrapped in the operating-model
        // direction — the placed procedure files by absolute path, never as bare slash
        // commands (ATH-AIPRJ-011, ATH-ACLI-016/017). Every other message type carries
        // its body unchanged.
        if message.message_type == athena_types::MessageType::Instruction {
            let workspace = self
                .manager
                .record(instance.runtime_instance_id.as_str())
                .expect("record present; is_running checked it")
                .workspace;
            message.body = crate::instruction::delegation_envelope(&workspace, &message.body)
                .map_err(|fault| RuntimeError::Validation(fault.to_string()))?;
        }
        self.manager.beat(instance.runtime_instance_id.as_str());
        // The IPC handoff: the delivery record lands in the Workspace's inbox first
        // (durable, the Broker's at-least-once face reads the workspace record), then —
        // when the composition root selected the real transport — the same body goes out
        // as a real `agent-cli ask` round trip and the agent's reply is recorded as its
        // `complete` in the outbox. v1's execution model is one delegation = one
        // PlanStep = one Instance (ATH-HIER-014), so the round trip *is* the step's
        // execution; a `send` that awaits it is the contract, not a blocking accident.
        let record = self
            .manager
            .record(instance.runtime_instance_id.as_str())
            .expect("record present; is_running checked it");
        let inbox = record.workspace.join(".athena-delegations");
        std::fs::create_dir_all(&inbox)
            .map_err(|error| RuntimeError::Validation(format!("inbox create failed: {error}")))?;
        std::fs::write(
            inbox.join(format!("{}.json", message.message_id.as_str())),
            serde_json::json!({
                "message_id": message.message_id.as_str(),
                "thread": message.thread_id,
                "body": message.body,
            })
            .to_string(),
        )
        .map_err(|error| RuntimeError::Transient(format!("inbox write failed: {error}")))?;
        if let Some(transport) = &self.transport {
            // Name-addressing is ambiguous once display names collide (stale
            // registers linger until pruned): the captured peer agent-id is the route.
            let peer = record
                .peer_id
                .clone()
                .unwrap_or_else(|| record.definition.id.as_str().to_string());
            let reply = transport
                .ask(
                    &record.workspace,
                    &peer,
                    &message.body,
                )
                .await?;
            let outbox = record.workspace.join(".athena-outbox");
            std::fs::create_dir_all(&outbox).map_err(|error| {
                RuntimeError::Transient(format!("outbox create failed: {error}"))
            })?;
            let reply_id = athena_types::MessageId::new();
            std::fs::write(
                outbox.join(format!("{}.json", reply_id.as_str())),
                serde_json::json!({
                    "message_id": reply_id.as_str(),
                    "type": "complete",
                    "thread": message.thread_id,
                    "body": reply,
                })
                .to_string(),
            )
            .map_err(|error| RuntimeError::Transient(format!("outbox write failed: {error}")))?;
        }
        Ok(())
    }

    fn receive(
        &self,
        instance: &RuntimeInstanceRef,
    ) -> futures::stream::BoxStream<'static, Message> {
        self.manager.beat(instance.runtime_instance_id.as_str());
        let record = self.manager.record(instance.runtime_instance_id.as_str());
        let Some(entry) = record else {
            return athena_runtime::interface::boxed_stream(futures::stream::iter(
                Vec::<Message>::new(),
            ));
        };
        let outbox = entry.workspace.join(".athena-outbox");
        let messages: Vec<Message> = std::fs::read_dir(&outbox)
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
                    .filter_map(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                    .map(|value| Message {
                        message_id: athena_types::MessageId::parse(
                            value
                                .get("message_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("msg-0")
                                .to_string(),
                        )
                        .unwrap_or_default(),
                        message_type: serde_json::from_value::<athena_types::MessageType>(
                            value.get("type").cloned().unwrap_or_default(),
                        )
                        .unwrap_or(athena_types::MessageType::Status),
                        from_agent: Some(entry.definition.id.clone()),
                        from_instance_id: Some(instance.instance_id.clone()),
                        thread_id: value
                            .get("thread")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .into(),
                        reply_to: None,
                        priority: athena_types::Priority::Normal,
                        body: value
                            .get("body")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .into(),
                        payload_json: None,
                        deadline: None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        athena_runtime::interface::boxed_stream(futures::stream::iter(messages))
    }

    async fn status(
        &self,
        instance: &RuntimeInstanceRef,
    ) -> Result<RuntimeInstanceStatus, RuntimeError> {
        self.manager
            .status(instance.runtime_instance_id.as_str())
            .await
    }

    async fn stop(
        &self,
        instance: &RuntimeInstanceRef,
        reason: StopReason,
    ) -> Result<(), RuntimeError> {
        // A stopped Instance is no longer live: the record releases at terminal state
        // (`ATH-RT-010`), so later probes report (`UnknownInstance`) rather than fabricate.
        self.manager
            .stop(instance.runtime_instance_id.as_str(), reason)
            .await
    }

    async fn collect_result(
        &self,
        instance: &RuntimeInstanceRef,
    ) -> Result<ExecutionResult, RuntimeError> {
        // The result intake: the agent's `complete` reply — recorded by the IPC
        // transport into the Workspace outbox — materializes as the ExecutionResult.
        // Everything before is RuntimeError-shaped (nothing collected yet), never a
        // fabricated success (ATH-EVAL-005's face at the Runtime boundary).
        let record = self
            .manager
            .record(instance.runtime_instance_id.as_str())
            .ok_or_else(|| {
                RuntimeError::UnknownInstance(
                    "the process exited; the Runtime reports, never fabricates".into(),
                )
            })?;
        // The Core Agent's output is a Task's decision input, never a Result row: the
        // intake refuses the instance-#0 line before looking for anything to collect.
        let Some(task_id) = record.task_id.clone() else {
            return Err(RuntimeError::Validation(
                "instance #0 collects no Task result: the Core Agent's output is a Task's \
                 decision input, never a Result row"
                    .into(),
            ));
        };
        let outbox = record.workspace.join(".athena-outbox");
        let mut completions: Vec<(std::path::PathBuf, serde_json::Value)> =
            std::fs::read_dir(&outbox)
                .map(|entries| {
                    entries
                        .flatten()
                        .filter_map(|entry| {
                            let path = entry.path();
                            let text = std::fs::read_to_string(&path).ok()?;
                            let value = serde_json::from_str::<serde_json::Value>(&text).ok()?;
                            if value.get("type")? == "complete" { Some((path, value)) } else { None }
                        })
                        .collect()
                })
                .unwrap_or_default();
        completions.sort_by(|a, b| a.0.file_name().cmp(&b.0.file_name()));
        let (_, value) = completions.pop().ok_or_else(|| {
            RuntimeError::Failure(
                "nothing to collect: the agent's `complete` has not arrived".into(),
            )
        })?;
        let body = value
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let now = athena_types::now_rfc3339();
        Ok(ExecutionResult {
            id: ResultId::new(),
            task_id,
            plan_step_id: record.plan_step_id.clone(),
            agent_id: record.definition.id.clone(),
            instance_id: record.instance_id.clone(),
            status: ResultStatus::Success,
            output: body,
            artifacts: vec![],
            logs_ref: None,
            skills_used: vec![],
            model: Some(record.definition.model.clone()),
            started_at: now.clone(),
            finished_at: now,
            execution_time_ms: 0,
            metadata: serde_json::json!({
                "source": "agent-cli ask round trip",
                "self_declared": true,
                "acceptance_basis": "ATH-EVAL-005: never sufficient alone"
            }),
        })
    }

    fn capabilities(&self) -> RuntimeCapabilities {
        RuntimeCapabilities {
            runtime_type: RuntimeType::AgentCli,
            isolation: IsolationBoundary::Process,
            supported_tools: vec![
                ToolName::FsRead,
                ToolName::FsWrite,
                ToolName::Search,
                ToolName::SendTo,
                ToolName::Git,
            ],
            max_concurrent_instances: 16,
        }
    }
}

/// The IPC transport seam (the IPC row; verified against `agent-cli` 0.29.0,
/// 2026-10-18). The Adapter's default delivery is the workspace file protocol the
/// deterministic CI lane plays (`ATH-TEST-022` — no real process, no LLM); selecting
/// [`RealIpc`] — which only the composition root (the resident binary) and the live
/// acceptance lane do — delivers every `instruction` as a real `agent-cli ask` round
/// trip over the peer registry, and records the agent's reply into the Workspace outbox
/// where `receive` / `collect_result` read it.
pub trait IpcTransport: Send + Sync {
    /// Deliver one enveloped `instruction` to the peer and return its reply text.
    ///
    /// # Errors
    /// Transport failures surface whole (the caller maps them onto `RuntimeError`).
    fn ask(
        &self,
        workspace: &std::path::Path,
        peer: &str,
        body: &str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, RuntimeError>> + Send + '_>,
    >;
}

/// The real transport: one `agent-cli ask <PEER> <BODY>` child per delivery, cwd =
/// the Workspace (the per-Instance overlay applies), stdout captured as the reply.
#[derive(Debug, Default)]
pub struct RealIpc;

impl IpcTransport for RealIpc {
    fn ask(
        &self,
        workspace: &std::path::Path,
        peer: &str,
        body: &str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<String, RuntimeError>> + Send + '_>,
    > {
        let workspace = workspace.to_path_buf();
        let peer = peer.to_string();
        let body = body.to_string();
        Box::pin(async move {
            let output = tokio::process::Command::new("agent-cli")
                .arg("ask")
                .arg(&peer)
                .arg(&body)
                .current_dir(&workspace)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()
                .await
                .map_err(|error| {
                    RuntimeError::Transient(format!("agent-cli ask failed: {error}"))
                })?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(RuntimeError::Transient(format!(
                    "agent-cli ask reported failure: {}",
                    stderr.trim()
                )));
            }
            Ok(String::from_utf8_lossy(&output.stdout)
                .trim_end()
                .to_string())
        })
    }
}

/// The persona body behind a `persona_ref`: a readable file's content, else the
/// reference verbatim (the deterministic tests' bare-slug body).
#[must_use]
fn persona_body_of(persona_ref: &str) -> String {
    std::fs::read_to_string(persona_ref).unwrap_or_else(|_| persona_ref.to_string())
}

/// The base context copy the Adapter renders against (the Workspace and work targets
/// ride the original; the template only borrows deadlines).
fn loop_base(ctx: &StartContext) -> StartContext {
    StartContext {
        task_id: ctx.task_id.clone(),
        plan_step_id: ctx.plan_step_id.clone(),
        instance_id: ctx.instance_id.clone(),
        workspace: ctx.workspace.clone(),
        work_targets: ctx.work_targets.clone(),
        persona_ref: ctx.persona_ref.clone(),
        persona_version: ctx.persona_version.clone(),
        skill_set: ctx.skill_set.clone(),
        allowed_tools: ctx.allowed_tools.clone(),
        delegable_tools: ctx.delegable_tools.clone(),
        filesystem_scope: ctx.filesystem_scope.clone(),
        model: ctx.model.clone(),
        deadlines: ctx.deadlines,
        resume_snapshot_ref: ctx.resume_snapshot_ref.clone(),
        context_document_ref: ctx.context_document_ref.clone(),
    }
}
