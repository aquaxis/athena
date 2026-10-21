//! Core Agent Session — instance #0's lifecycle (spec §7.5, design §3.3).
//!
//! The supervision pieces, none of them deciding anything Core-size (ATH-CORE-022):
//!
//! - **Boot (P1-30).** Instance #0 starts through the Runtime Interface as part of the
//!   ATHENA boot sequence, with no Task bound (ATH-CORE-021); its `CoreAgent*` Events are
//!   built here as typed catalog structures and reach the store through the Event Manager
//!   (ATH-PERS-003, D-1).
//! - **Control-surface integrity (P1-31, ATH-CORE-044).** The Core Agent's `.agent-cli/`,
//!   its `skills/`, and `<PROJECT_ROOT>/.aiprj/instructions.md` + `rules/` are checksummed
//!   at placement and re-verified at every start and restart — a mismatch escalates to a
//!   human instead of restarting, because a restarted agent would run altered instructions.
//! - **Deterministic recovery (P1-32, §7.5.3).** "Deterministic" describes the *procedure*,
//!   never the restarted agent's behaviour (ATH-CORE-049): freeze, verify, rebuild the
//!   context document from Working Memory + Events (§13.1 minimum, ATH-MEM-007 /
//!   ATH-CORE-046), restart with re-injection, inside the sliding restart window
//!   (ATH-CORE-045) that excludes `Transient` / resource-shortage failures and never
//!   substitutes its own judgement when exhausted (ATH-CORE-030). Safety after each
//!   restart rides on the idempotency guard (ATH-CORE-035) and the startup reconciliation
//!   (ATH-CORE-048), not on the agent behaving identically.

use athena_events::envelope::Actor;
use athena_events::events::{
    CoreAgentEscalated, CoreAgentHeartbeat, CoreAgentRestarted, CoreAgentStarted,
};
use athena_events::{EventContext, EventError, TypedEvent};
use athena_persistence::AthenaStore;
use athena_persistence::error::PersistenceError;
use athena_persistence::repository::{
    AgentRepository, ApprovalRepository, EventRepository, MemoryRepository, PlanRepository,
};
use athena_runtime::interface::Runtime;
use athena_runtime::{RuntimeError, RuntimeInstanceRef, StartContext};
use athena_types::{
    AgentDefinition, AgentInstance, AgentInstanceState, ApprovalState, InstanceId, MemoryTier,
    PlanStepId, PlanStepState, TaskId,
};
use sha2::{Digest as _, Sha256};

// --- P1-31: control-surface checksum ----------------------------------------------------

/// The integrity token of the Core Agent's control surface (ATH-CORE-044).
///
/// Computed over `(path, content)` pairs sorted by path, so a file set compares equal
/// however the caller walked the tree, and any rewrite changes the digest. Core Host
/// stores the value at placement; starts and restarts re-verify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlSurfaceChecksum(String);

impl ControlSurfaceChecksum {
    /// Compute over the authorized surface: paths *inside* the surface, with contents.
    #[must_use]
    pub fn of(entries: &[(&str, &[u8])]) -> Self {
        let mut hasher = Sha256::new();
        let mut sorted: Vec<(&str, &[u8])> = entries.to_vec();
        sorted.sort_by_key(|(path, _)| *path);
        for (path, content) in sorted {
            hasher.update(path.as_bytes());
            hasher.update([0u8, 0x1f]); // unit separator between path and content
            hasher.update(content);
        }
        Self(hex_lower(&hasher.finalize()))
    }

    /// The digest, 64 lower-case hex characters.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The reason a checksum verification refuses (ATH-CORE-044).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SurfaceMismatch {
    /// The recorded digest no longer matches what is on the surface: altered agent-side
    /// or by an outside writer.
    #[error("control surface changed since placement: recorded {published}, present {present}")]
    Altered {
        /// The digest Core Host recorded at placement.
        published: String,
        /// The digest of the files now on the surface.
        present: String,
    },
}

impl SurfaceMismatch {
    /// The reason text, as the `CoreAgentEscalated` escalation note.
    #[must_use]
    pub fn reason(&self) -> String {
        self.to_string()
    }
}

/// Verify the live surface against the recorded digest (ATH-CORE-044).
///
/// # Errors
/// [`SurfaceMismatch::Altered`] — the surface is not what was placed; the caller
/// escalates and does **not** restart (ATH-CORE-044).
pub fn verify_surface(
    placed: &ControlSurfaceChecksum,
    live: &ControlSurfaceChecksum,
) -> Result<(), SurfaceMismatch> {
    if placed.0 == live.0 {
        Ok(())
    } else {
        Err(SurfaceMismatch::Altered {
            published: placed.as_str().into(),
            present: live.as_str().into(),
        })
    }
}

/// Lower-case hex of a digest.
fn hex_lower(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

// --- P1-32: the restart window ----------------------------------------------------------

/// Why instance #0's (re)start failed, at the granularity the count needs (ATH-CORE-045).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartFailureKind {
    /// Re-trying without a fresh decision may succeed (ATH-CORE-031's `TransientError`
    /// exclusion).
    Transient,
    /// The host cannot provide the resources (spec §7.5.3's 資源不足) — also excluded.
    ResourceShortage,
    /// Every other failure: counted.
    Counted,
}

impl StartFailureKind {
    /// Map a Runtime's start failure into the count's granularity. Every kind except the
    /// two specification exclusions is counted.
    #[must_use]
    pub fn from_runtime(error: &RuntimeError) -> Self {
        match error {
            RuntimeError::Transient(_) => Self::Transient,
            RuntimeError::Deadline(_)
            | RuntimeError::Failure(_)
            | RuntimeError::Internal(_)
            | RuntimeError::UnknownInstance(_)
            | RuntimeError::Unsupported(_)
            | RuntimeError::Validation(_) => Self::Counted,
        }
    }
}

/// The sliding restart window (ATH-CORE-045): 3 attempts per hour by default, with
/// `Transient` and resource-shortage failures uncounted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartWindow {
    attempts_ms: Vec<u64>,
    max: u64,
    window_ms: u64,
}

impl RestartWindow {
    /// The specification default: 3 attempts inside 1 hour.
    #[must_use]
    pub fn default_window() -> Self {
        Self {
            attempts_ms: Vec::new(),
            max: 3,
            window_ms: 3_600_000,
        }
    }

    /// Record one failed (re)start; excluded kinds consume no budget (ATH-CORE-045) and
    /// report `false` — not counted.
    pub fn record(&mut self, failure: StartFailureKind, at_ms: u64) -> bool {
        self.slide(at_ms);
        match failure {
            StartFailureKind::Transient | StartFailureKind::ResourceShortage => false,
            StartFailureKind::Counted => {
                self.attempts_ms.push(at_ms);
                true
            }
        }
    }

    /// The attempt budget the window grants (its ceiling, not the current count).
    #[must_use]
    pub const fn limit(&self) -> u64 {
        self.max
    }

    /// Whether the count inside the window is at the ceiling (used by probes); the
    /// *decision* escalates only when a counted failure exceeds the budget
    /// (see [`recovery_decision`]).
    #[must_use]
    pub fn exhausted(&mut self, at_ms: u64) -> bool {
        let counted = self.counted(at_ms);
        counted as u64 >= self.max
    }

    /// Counted attempts inside the window right now.
    #[must_use]
    pub fn counted(&mut self, at_ms: u64) -> usize {
        self.slide(at_ms);
        self.attempts_ms.len()
    }

    /// Entries older than the window drop off the count.
    fn slide(&mut self, now_ms: u64) {
        self.attempts_ms
            .retain(|stamp| now_ms.saturating_sub(*stamp) < self.window_ms);
    }
}

// --- P1-32: the deterministic recovery decision ------------------------------------------

/// What §7.5.3 prescribes on `CoreAgentFailed`.
#[derive(Debug, Clone, PartialEq)]
pub enum Recovery {
    /// Checksums held and budget allows: proceed to the context rebuild and restart;
    /// `attempt` is the 1-based attempt this restart consumes (for `CoreAgentRestarted`).
    Restart {
        /// 1-based restart attempt inside the window.
        attempt: u64,
    },
    /// Escalate to a human; autonomous execution halts (ATH-CORE-030), with the
    /// prescribed reason.
    Escalate(String),
    /// A `Transient` / resource-shortage failure: retry, consuming no budget
    /// (ATH-CORE-045).
    RetryNotCounted,
}

/// The §7.5.3 decision, in specification order — a mismatched surface never reaches the
/// window.
///
/// Counting: the failure is recorded first (spec: the *attempt* that broke the budget
/// escalates), so the 4th counted failure inside the hour escalates instead of starting a
/// 4th restart.
pub fn recovery_decision(
    surface_ok: Result<(), SurfaceMismatch>,
    failure: StartFailureKind,
    window: &mut RestartWindow,
    now_ms: u64,
) -> Recovery {
    if let Err(mismatch) = surface_ok {
        return Recovery::Escalate(mismatch.reason());
    }
    if !window.record(failure, now_ms) {
        return Recovery::RetryNotCounted;
    }
    // The count inside the window may exceed the budget only at the *fourth* counted
    // failure: three restarts happen, the fourth escalation (ATH-CORE-030).
    if window.counted(now_ms) as u64 > window.limit() {
        return Recovery::Escalate("restart window exhausted; autonomous execution halts".into());
    }
    Recovery::Restart {
        attempt: window.counted(now_ms) as u64,
    }
}

// --- P1-30: the boot-start through the Runtime Interface ---------------------------------

/// Start the Core Agent instance #0 as part of boot (ATH-CORE-021).
///
/// The context must describe a session-scoped agent with no Task bound (ATH-CORE-020) —
/// a Task-bound context is refused before the Runtime sees it.
///
/// # Errors
/// Whatever the Runtime reports — `TransientError` here is the caller's retry-not-counted
/// trigger (ATH-CORE-045); this module never repairs it.
pub async fn start_core_agent(
    runtime: &dyn Runtime,
    definition: AgentDefinition,
    context: StartContext,
) -> Result<RuntimeInstanceRef, RuntimeError> {
    if context.task_id.is_some() {
        return Err(RuntimeError::Validation(
            "the Core Agent Instance is session-scoped; no Task may bind to instance #0
             (ATH-CORE-020)"
                .into(),
        ));
    }
    context.validate()?;
    runtime.start(definition, context).await
}

// --- P1-30: typed CoreAgent* event builders (Event Manager emits; this builds) -----------

/// `CoreAgentStarted` at boot (ATH-CORE-031).
///
/// # Errors
/// [`EventError::InvalidEnvelope`] when the context lacks a correlation chain — every
/// Event joins one (ATH-OBS-004, enforced at construction).
pub fn started_event(context: EventContext) -> Result<TypedEvent<CoreAgentStarted>, EventError> {
    TypedEvent::new(CoreAgentStarted {}, context)
}

/// `CoreAgentRestarted` with its restart-window record (ATH-CORE-031).
///
/// # Errors
/// [`EventError::InvalidEnvelope`] for a correlation-less context.
pub fn restarted_event(
    attempt: u64,
    window_start: impl Into<String>,
    context_document_ref: impl Into<String>,
    context: EventContext,
) -> Result<TypedEvent<CoreAgentRestarted>, EventError> {
    TypedEvent::new(
        CoreAgentRestarted {
            attempt,
            window_start: window_start.into(),
            context_document_ref: context_document_ref.into(),
        },
        context,
    )
}

/// `CoreAgentEscalated` — checksum mismatch or budget exhaustion (ATH-CORE-031).
///
/// # Errors
/// [`EventError::InvalidEnvelope`] for a correlation-less context.
pub fn escalated_event(
    context: EventContext,
) -> Result<TypedEvent<CoreAgentEscalated>, EventError> {
    TypedEvent::new(CoreAgentEscalated {}, context)
}

/// `CoreAgentHeartbeat`, every 20 s by default — the *absence* detector's input
/// (ATH-OBS-004).
///
/// # Errors
/// [`EventError::InvalidEnvelope`] for a correlation-less context.
pub fn heartbeat_event(
    iteration: u64,
    focus: Option<TaskId>,
    pending_requests: u64,
    context: EventContext,
) -> Result<TypedEvent<CoreAgentHeartbeat>, EventError> {
    TypedEvent::new(
        CoreAgentHeartbeat {
            iteration,
            focus_task_id: focus,
            pending_requests,
        },
        context,
    )
}

// --- P1-33 / P1-34: the boot reconciliation and the heartbeat lease ----------------------

/// Which §17.2 route a process-less row legally takes (ATH-CORE-048 × ATH-STATE-002).
///
/// The store is authoritative, so the row's *state*, not a guess, decides: a row in a
/// process-bearing state (`STARTING` / `IDLE` / `BUSY`) routes to `FAILED` through that
/// state's own trigger; `WAITING` / `SUSPENDED` legitimately hold no process
/// (ATH-STATE-019: suspension releases it); `CREATED` has not started yet; `STOPPING` /
/// `FAILED` rows are reclamation windows that proceed without a process because the
/// resources reclaim themselves; `STOPPED` is terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LostProcessRoute {
    /// Finalize the row: `failed_state` is what the state table resolves, `event_type`
    /// the §17.2 Event name, and the attached PlanStep — only when `RUNNING` — goes
    /// `ORPHANED` through the §17.5 table (`InstanceVanished`).
    Finalize {
        /// The legal destination state.
        failed_state: AgentInstanceState,
        /// The Event the transition names (always `AgentFailed`).
        event_type: &'static str,
        /// Whether the attached step orphans with this finalization.
        orphan_step: bool,
    },
    /// The row legitimately holds no process (kept, never fabricated into a failure).
    Kept {
        /// Why, for the boot record.
        reason: &'static str,
    },
}

/// The route for one row: every write reuses the §17.2 table, never bypasses it.
#[must_use]
pub fn lost_process_route(state: &AgentInstanceState) -> LostProcessRoute {
    use athena_state::{InstanceTrigger as Trigger, instance_transition};
    match state {
        AgentInstanceState::Starting | AgentInstanceState::Idle | AgentInstanceState::Busy => {
            let trigger: Trigger = if matches!(state, AgentInstanceState::Starting) {
                Trigger::StartFailed
            } else if matches!(state, AgentInstanceState::Idle) {
                Trigger::ProcessCrashed
            } else {
                Trigger::ExecutionFailed
            };
            match instance_transition(*state, trigger) {
                Some(resolved) if resolved.to == AgentInstanceState::Failed => {
                    LostProcessRoute::Finalize {
                        failed_state: resolved.to,
                        event_type: resolved.event,
                        orphan_step: true,
                    }
                }
                _ => LostProcessRoute::Kept {
                    reason: "the table lacks a process-vanished route; surfaced, not bypassed",
                },
            }
        }
        AgentInstanceState::Created => LostProcessRoute::Kept {
            reason: "not started yet; the boot start covers it",
        },
        AgentInstanceState::Waiting | AgentInstanceState::Suspended => LostProcessRoute::Kept {
            reason: "process-less by design while it waits",
        },
        AgentInstanceState::Stopping | AgentInstanceState::Failed => LostProcessRoute::Kept {
            reason: "reclamation window proceeds without a process",
        },
        AgentInstanceState::Stopped => LostProcessRoute::Kept { reason: "terminal" },
    }
}

/// One reconciliation outcome (ATH-CORE-048).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reconciliation {
    /// The row was finalized `FAILED` with unknown side effects; its `RUNNING` PlanStep
    /// went `ORPHANED`, so a re-run is gated behind Verification (ATH-EVAL-002,
    /// ATH-STATE-029).
    Finalized {
        /// The instance.
        instance: InstanceId,
        /// The orphaned step, when one was attached.
        step: Option<PlanStepId>,
    },
    /// The row stays as it is (see [`LostProcessRoute::Kept`]).
    Kept {
        /// The instance.
        instance: InstanceId,
        /// Why it stays.
        reason: &'static str,
    },
}

/// The boot reconciliation for one Task: every non-terminal row the `alive` probe calls
/// process-less is routed through its §17.2 trigger (ATH-CORE-048).
///
/// The store writes one transaction per finalized row — instance state, attached PlanStep,
/// and the Events describing both — which is why the store method lives in
/// `athena-persistence` (ATH-PERS-003). The caller records the returned decisions as
/// Events through the Event Manager where the transition tables have not already named
/// them (`AgentFailed` / `PlanStepOrphaned` are written inside the transaction).
///
/// # Errors
/// Store failures, surfaced whole.
pub fn reconcile_boot_states(
    store: &mut AthenaStore,
    task: &TaskId,
    alive: impl Fn(&AgentInstance) -> bool,
    now: &str,
    correlation_id: &'static str,
) -> Result<Vec<Reconciliation>, PersistenceError> {
    let mut output = Vec::new();
    for instance in AgentRepository::list_non_terminal_instances(store)? {
        if instance.task_id.as_ref() != Some(task) || alive(&instance) {
            continue;
        }
        match lost_process_route(&instance.state) {
            LostProcessRoute::Finalize {
                failed_state,
                event_type,
                orphan_step,
            } => {
                // The orphaning decision uses the step's *current* state; only a `RUNNING`
                // step orphans (a `READY` one has no process to lose, ATH-STATE-028).
                let orphaned = if orphan_step {
                    instance.plan_step_id.as_ref().and_then(|step| {
                        PlanRepository::get_step(store, step)
                            .ok()
                            .flatten()
                            .filter(|step| step.state == PlanStepState::Running)
                            .and_then(|step| {
                                athena_state::plan_step_transition(
                                    step.state,
                                    athena_state::PlanStepTrigger::InstanceVanished,
                                )
                                .filter(|resolved| resolved.to == PlanStepState::Orphaned)
                                .map(|resolved| (resolved.to, resolved.event, step.id.clone()))
                            })
                    })
                } else {
                    None
                };
                store.reconcile_instance(
                    &instance,
                    failed_state,
                    event_type,
                    orphaned.as_ref().map(|(_, _, step)| step),
                    now,
                    correlation_id,
                )?;
                output.push(Reconciliation::Finalized {
                    instance: instance.id.clone(),
                    step: orphaned.map(|(_, _, step)| step),
                });
            }
            LostProcessRoute::Kept { reason } => output.push(Reconciliation::Kept {
                instance: instance.id.clone(),
                reason,
            }),
        }
    }
    Ok(output)
}

/// The heartbeat lease between Core Host and every Agent Instance (ATH-CORE-047).
///
/// A 60 s lapse (default) is the point the *instance self-terminates*; the tracker records
/// the receipts so the boot reconciliation and the observability layer read the same fact,
/// and `CoreAgentHeartbeat` events every 20 s make even an absent transition observable.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HeartbeatLease {
    last_renewed_ms: std::collections::HashMap<InstanceId, u64>,
    lease_ms: u64,
}

impl HeartbeatLease {
    /// The specification default: 60 s.
    #[must_use]
    pub fn default_lease() -> Self {
        Self {
            last_renewed_ms: std::collections::HashMap::new(),
            lease_ms: 60_000,
        }
    }

    /// An instance came up or renews its lease.
    pub fn renew(&mut self, instance: &InstanceId, now_ms: u64) {
        self.last_renewed_ms.insert(instance.clone(), now_ms);
    }

    /// Whether one instance's heartbeat has lapsed at `now_ms`.
    #[must_use]
    pub fn has_lapsed(&self, instance: &InstanceId, now_ms: u64) -> bool {
        self.last_renewed_ms
            .get(instance)
            .is_some_and(|stamp| now_ms.saturating_sub(*stamp) >= self.lease_ms)
    }

    /// Every lapsed instance, oldest lapse first.
    #[must_use]
    pub fn lapsed(&self, now_ms: u64) -> Vec<InstanceId> {
        let mut lapsed: Vec<(InstanceId, u64)> = self
            .last_renewed_ms
            .iter()
            .filter(|(_, stamp)| now_ms.saturating_sub(**stamp) >= self.lease_ms)
            .map(|(instance, stamp)| (instance.clone(), now_ms - *stamp))
            .collect();
        lapsed.sort_by_key(|(_, since)| std::cmp::Reverse(*since));
        lapsed.into_iter().map(|(instance, _)| instance).collect()
    }

    /// Drop the lapsed entry — the instance finalized or the lease removed.
    pub fn forget(&mut self, instance: &InstanceId) {
        self.last_renewed_ms.remove(instance);
    }
}

/// The actor the Core Agent's events carry.
pub const CORE_AGENT_ACTOR: Actor = Actor::CoreAgent;

// --- P1-32: the context document rebuild ------------------------------------------------

/// The §13.1 / ATH-CORE-046 context document, rebuilt from Working Memory + the store.
///
/// References, not prose: `MessageId` for reports, `MemoryId` for snapshot summaries —
/// Event payloads never carried bodies, and this document does not invent them
/// (ATH-EVENT-007). Instance #0 is re-injected with this after a restart (ATH-CORE-029);
/// one document per Task, because Working Memory's scope is the Task (§13.1).
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize)]
pub struct ContextDocument {
    /// The current Plan with its revision.
    pub plan: Option<(String, u64)>,
    /// Incomplete PlanSteps of that Plan.
    pub incomplete_steps: Vec<(String, String)>,
    /// Unterminated delegations: step, agent, attempt, instance state (ATH-CORE-046).
    pub delegations: Vec<(Option<String>, String, u64, String)>,
    /// Each reporting instance's latest report, referenced (ATH-EVENT-007).
    pub latest_report_refs: Vec<(String, String)>,
    /// Approvals a human has not answered (ATH-HITL-003: silence becomes rejection).
    pub pending_approvals: Vec<String>,
    /// `GRANTED` and unconsumed (ATH-CORE-046).
    pub granted_unconsumed: Vec<String>,
    /// Recent privileged-request denials (ATH-CORE-037).
    pub recent_denials: Vec<DenialRecord>,
    /// `SUSPENDED` instances (by agent slug) whose Working Memory snapshot record exists
    /// (ATH-MEM-007's last row). An absent entry means no snapshot ever landed; the
    /// rebuilt agent is not handed one to invent (ATH-MEM-008's fallback).
    pub suspended_snapshots: Vec<(String, String)>,
}

/// One recorded privileged-request denial.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DenialRecord {
    /// The refused kind's wire tag.
    pub request_kind: String,
    /// The §20.1 class.
    pub error_class: String,
    /// The rule or grant the refusal came from.
    pub policy_id: String,
    /// The Event that recorded it, for audit drill-down.
    pub event_id: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct DenialWire {
    request_kind: String,
    error_class: String,
    policy_id: String,
}

impl ContextDocument {
    /// Rebuild for one Task from the store — a deterministic procedure (ATH-CORE-029).
    ///
    /// # Errors
    /// Store read failures, surfaced whole for the caller to classify.
    pub fn rebuild(store: &mut AthenaStore, task: &TaskId) -> Result<Self, PersistenceError> {
        let mut document = Self::default();

        // The Plan and its incomplete steps (ATH-MEM-007's first two rows).
        if let Some(plan) = PlanRepository::latest_for_task(store, task)? {
            document.incomplete_steps = plan
                .steps
                .iter()
                .filter(|step| !step.state.is_terminal())
                .map(|step| (step.id.as_str().into(), step.state.as_str().into()))
                .collect();
            document.plan = Some((plan.id.as_str().into(), plan.revision));
        }

        // Unterminated delegations: the Task's instance rows (1:1, ATH-RT-012).
        for inst in AgentRepository::list_non_terminal_instances(store)? {
            if inst.task_id.as_ref() == Some(task) {
                document.delegations.push((
                    inst.plan_step_id
                        .as_ref()
                        .map(PlanStepId::as_str)
                        .map(Into::into),
                    inst.agent_id.as_str().into(),
                    inst.attempt,
                    inst.state.as_str().into(),
                ));
            }
        }

        // Approvals: pending and granted-but-unconsumed (ATH-MEM-007 rows 5–6).
        for approval in ApprovalRepository::list_by_state(store, ApprovalState::Requested)? {
            if &approval.task_id == task {
                document.pending_approvals.push(approval.id.as_str().into());
            }
        }
        for approval in ApprovalRepository::list_granted_unconsumed(store)? {
            if &approval.task_id == task {
                document
                    .granted_unconsumed
                    .push(approval.id.as_str().into());
            }
        }

        // Events carry references only (ATH-EVENT-007): report message ids, denial facts.
        let events = EventRepository::list_for_task(store, task)?;
        let mut reports: std::collections::HashMap<String, (u64, String)> =
            std::collections::HashMap::new();
        for event in &events {
            if event.event_type == "AgentMessageReceived"
                && let Some(instance) = &event.instance_id
                && let Ok(payload) = serde_json::from_str::<serde_json::Value>(&event.payload_json)
                && let Some(message_id) = payload.get("message_id").and_then(|v| v.as_str())
            {
                let entry = reports
                    .entry(instance.as_str().into())
                    .or_insert((event.sequence, message_id.into()));
                if entry.0 < event.sequence {
                    *entry = (event.sequence, message_id.into());
                }
            }
            if event.event_type == "PrivilegedRequestDenied"
                && let Ok(wire) = serde_json::from_str::<DenialWire>(&event.payload_json)
            {
                document.recent_denials.push(DenialRecord {
                    request_kind: wire.request_kind,
                    error_class: wire.error_class,
                    policy_id: wire.policy_id,
                    event_id: event.event_id.as_str().into(),
                });
            }
        }
        for (instance, (_, message)) in reports {
            document.latest_report_refs.push((instance, message));
        }
        document.latest_report_refs.sort();

        // `SUSPENDED` instances and their Working Memory snapshot record (last row).
        for inst in AgentRepository::list_non_terminal_instances(store)? {
            if inst.task_id.as_ref() == Some(task)
                && inst.state == AgentInstanceState::Suspended
                && let Some(record) =
                    MemoryRepository::list(store, MemoryTier::Working, inst.id.as_str())?
                        .into_iter()
                        .next()
            {
                document
                    .suspended_snapshots
                    .push((inst.agent_id.as_str().into(), record.id.as_str().into()));
            }
        }

        Ok(document)
    }

    /// The re-injection payload, serialized.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }
}
