//! The deterministic stub Runtime (design §5.2, ATH-TEST-021).
//!
//! Phases 1–2 are carried by this implementation: Core is exercised against a scripted
//! agent with **no** `agent-cli` dependency at all (ATH-DEV-018, ATH-NFR-008). The stub is
//! deliberately simple:
//!
//! - **Virtual time.** An internal milliseconds counter advanced only by
//!   [`StubClock::advance`]. No wall-clock sleeping — tests stay fast and deterministic,
//!   and deadline / lease / heartbeat behaviour is observed by advancing the clock.
//! - **Scripted replies.** A [`StubScript`] maps an instruction body (first substring
//!   match wins, otherwise the default reaction) to the replies the agent emits, optionally
//!   delayed, and optionally to a result `collect_result` hands over.
//! - **Injectable failures per error class.** A queue of pre-set failures is consumed one
//!   per `start` / `send`, so a test decides the exact §20.1 class observed. A `Transient`
//!   failure here is not counted by Core against the restart window (ATH-CORE-031); the
//!   stub only reports the class.
//! - **Controllable heartbeat.** Renewals happen explicitly through
//!   [`StubRuntime::heartbeat`], or automatically at the configured interval. A lapsed
//!   lease self-terminates the instance with unknown side effects — the observable the
//!   heartbeat lease relies on (ATH-CORE-047).
//!
//! Delivery is a poll/snapshot model: each [`Runtime::receive`] call yields every message
//! currently due and marks it delivered (at-least-once per call). A re-delivered message is
//! deduplicated by the subscriber on `message_id`, where the specification places that duty
//! (ATH-EVENT-005, ATH-MSG-003).

use crate::context::StartContext;
use crate::error::{Result, RuntimeError};
use crate::interface::{Runtime, RuntimeInstanceRef, boxed_stream};
use crate::message::Message;
use crate::status::{IsolationBoundary, RuntimeCapabilities, RuntimeInstanceStatus};
use athena_types::{
    AgentDefinition, ExecutionResult, MessageId, MessageType, Priority, ResultStatus, RuntimeType,
    StopReason, ToolName,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

/// Virtual wall clock, in milliseconds, advanced only by tests (or Core).
#[derive(Debug, Default)]
pub struct StubClock(Arc<Mutex<u64>>);

impl StubClock {
    /// A clock at `0 ms`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The current virtual time.
    #[must_use]
    pub fn now(&self) -> u64 {
        *self.0.lock().expect("stub clock poisoned")
    }

    /// Advance virtual time.
    pub fn advance(&self, millis: u64) {
        *self.0.lock().expect("stub clock poisoned") += millis;
    }
}

/// One reply the scripted agent emits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StubReply {
    /// Kind carried by the produced message.
    pub message_type: MessageType,
    /// Body.
    pub body: String,
    /// Virtual delay before the message becomes deliverable.
    pub delay_ms: u64,
}

/// A result handed to `collect_result` (D-7: provisional identity).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StubResult {
    /// Agent-declared status; never sufficient for acceptance (ATH-EVAL-005).
    pub status: ResultStatus,
    /// Output text.
    pub output: String,
    /// Reported execution time.
    pub execution_time_ms: u64,
}

/// What a scripted reaction produces.
///
/// Replies and a result coexist (a Worker reports *and* completes); `status_after` names
/// the state the instance settles into, when the script dictates one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StubReaction {
    /// Messages the agent emits after receiving the instruction.
    pub replies: Vec<StubReply>,
    /// The result `collect_result` returns once this reaction ran; the id stays provisional
    /// for Core's Result Manager to re-mint (D-7).
    pub result: Option<StubResult>,
    /// Status the instance settles into after processing the message.
    pub status_after: Option<RuntimeInstanceStatus>,
}

/// Body-triggered reactions (first substring match wins; otherwise the default).
#[derive(Debug, Clone, Default)]
pub struct StubScript {
    /// Reactions matched on the message body.
    pub reactions: Vec<(String, StubReaction)>,
    /// The reaction used when nothing matched.
    pub default: StubReaction,
}

impl StubScript {
    /// Script that makes the agent accept, report, complete, and offer a result for any
    /// instruction — the minimal shape a Core Loop needs to finish a Task.
    #[must_use]
    pub fn completes_any_instruction(output: impl Into<String>) -> Self {
        let output = output.into();
        Self {
            default: StubReaction {
                replies: vec![
                    StubReply {
                        message_type: MessageType::Status,
                        body: "accepted".into(),
                        delay_ms: 0,
                    },
                    StubReply {
                        message_type: MessageType::Report,
                        body: "work reported".into(),
                        delay_ms: 10,
                    },
                    StubReply {
                        message_type: MessageType::Complete,
                        body: format!("completed: {output}"),
                        delay_ms: 20,
                    },
                ],
                result: Some(StubResult {
                    status: ResultStatus::Success,
                    output,
                    execution_time_ms: 100,
                }),
                status_after: Some(RuntimeInstanceStatus::Stopped { exit_code: Some(0) }),
            },
            reactions: vec![],
        }
    }
}

/// Stub configuration.
#[derive(Debug, Clone)]
pub struct StubConfig {
    /// The kind the stub declares; matters only for registry bookkeeping.
    pub runtime_kind: RuntimeType,
    /// The isolation boundary the stub declares (ATH-RT-005).
    pub isolation: IsolationBoundary,
    /// Failures injected on `start`, one per call, in order (ATH-TEST-021).
    pub fail_start: VecDeque<RuntimeError>,
    /// Failures injected on `send`, one per call, in order.
    pub fail_send: VecDeque<RuntimeError>,
    /// Heartbeat cadence: the agent renews the lease itself every interval. `None` means
    /// the lease is renewed only through [`StubRuntime::heartbeat`].
    pub heartbeat_interval_ms: Option<u64>,
    /// Lease duration after which a lapsed heartbeat self-terminates the instance with
    /// unknown side effects (ATH-CORE-047; the default is 60 s).
    pub lease_duration_ms: Option<u64>,
}

impl Default for StubConfig {
    fn default() -> Self {
        Self {
            runtime_kind: RuntimeType::LocalProcess,
            isolation: IsolationBoundary::Process,
            fail_start: VecDeque::new(),
            fail_send: VecDeque::new(),
            heartbeat_interval_ms: None,
            lease_duration_ms: Some(60_000),
        }
    }
}

/// An outbox entry that becomes deliverable at a virtual instant.
struct OutboxEntry {
    due_ms: u64,
    message: Message,
}

/// One stub-managed instance.
struct StubInstance {
    handle: RuntimeInstanceRef,
    definition: AgentDefinition,
    context: StartContext,
    started_at: u64,
    status: RuntimeInstanceStatus,
    outbox: VecDeque<OutboxEntry>,
    delivered: HashSet<MessageId>,
    last_heartbeat_at: u64,
    heartbeat_observations: Vec<u64>,
    result: Option<StubResult>,
}

/// The deterministic stub Runtime.
pub struct StubRuntime {
    clock: StubClock,
    config: Mutex<StubConfig>,
    script: StubScript,
    instances: Mutex<HashMap<String, StubInstance>>,
}

impl std::fmt::Debug for StubRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let instances = self
            .instances
            .lock()
            .expect("stub instances poisoned")
            .len();
        f.debug_struct("StubRuntime")
            .field("clock_ms", &self.clock.now())
            .field("instances", &instances)
            .finish()
    }
}

/// Live, message-routable statuses.
fn is_live(status: &RuntimeInstanceStatus) -> bool {
    !matches!(
        status,
        RuntimeInstanceStatus::Stopped { .. } | RuntimeInstanceStatus::Failed { .. }
    )
}

/// Stable map key: the Runtime's own instance id.
fn key_of(handle: &RuntimeInstanceRef) -> String {
    handle.runtime_instance_id.as_str().to_string()
}

impl StubRuntime {
    /// A stub with the given configuration and script.
    #[must_use]
    pub fn new(config: StubConfig, script: StubScript) -> Self {
        Self {
            clock: StubClock::new(),
            config: Mutex::new(config),
            script,
            instances: Mutex::new(HashMap::new()),
        }
    }

    /// A stub with the default configuration and the given script.
    #[must_use]
    pub fn scripted(script: StubScript) -> Self {
        Self::new(StubConfig::default(), script)
    }

    /// The virtual clock, for test control.
    #[must_use]
    pub fn clock(&self) -> &StubClock {
        &self.clock
    }

    /// Renew the heartbeat lease of an instance, explicitly (ATH-CORE-047).
    ///
    /// # Errors
    /// [`RuntimeError::UnknownInstance`] when the handle is stale or the instance is no
    /// longer live.
    pub fn heartbeat(&self, instance: &RuntimeInstanceRef) -> Result<()> {
        let mut instances = self.instances.lock().expect("stub instances poisoned");
        let inst = instances.get_mut(&key_of(instance)).ok_or_else(|| {
            RuntimeError::UnknownInstance(instance.runtime_instance_id.as_str().into())
        })?;
        if !is_live(&inst.status) {
            return Err(RuntimeError::UnknownInstance(
                "instance is no longer live; its lease cannot be renewed".into(),
            ));
        }
        let stamp = self.clock.now();
        inst.last_heartbeat_at = stamp;
        inst.heartbeat_observations.push(stamp);
        Ok(())
    }

    /// The heartbeat renewal instants observed so far, for lease assertions.
    #[must_use]
    pub fn heartbeat_observations(&self, instance: &RuntimeInstanceRef) -> Vec<u64> {
        self.instances
            .lock()
            .expect("stub instances poisoned")
            .get(&key_of(instance))
            .map(|inst| inst.heartbeat_observations.clone())
            .unwrap_or_default()
    }

    /// Load the failure queues: one injected failure per subsequent `start` / `send`
    /// (ATH-TEST-021).
    pub fn arm_failures(&self, start: Vec<RuntimeError>, send: Vec<RuntimeError>) {
        let mut config = self.config.lock().expect("stub config poisoned");
        config.fail_start = start.into();
        config.fail_send = send.into();
    }

    /// The current status of an instance, for ad-hoc test inspection.
    #[must_use]
    pub fn status_of(&self, instance: &RuntimeInstanceRef) -> Option<RuntimeInstanceStatus> {
        self.instances
            .lock()
            .expect("stub instances poisoned")
            .get(&key_of(instance))
            .map(|inst| inst.status.clone())
    }

    /// Whether the heartbeat lease has lapsed on a live instance.
    fn lease_expired(&self, inst: &StubInstance) -> bool {
        if !is_live(&inst.status) {
            return false;
        }
        let lease = self
            .config
            .lock()
            .expect("stub config poisoned")
            .lease_duration_ms;
        match lease {
            None => false,
            Some(duration) => self.clock.now().saturating_sub(inst.last_heartbeat_at) > duration,
        }
    }

    /// Whether the automatic heartbeat cadence is due on a live instance.
    fn auto_heartbeat_due(&self, inst: &StubInstance) -> bool {
        if !is_live(&inst.status) {
            return false;
        }
        let interval = self
            .config
            .lock()
            .expect("stub config poisoned")
            .heartbeat_interval_ms;
        match interval {
            None => false,
            Some(interval) => self.clock.now().saturating_sub(inst.last_heartbeat_at) >= interval,
        }
    }

    /// Drain the currently due outbox entries of an instance, marking them delivered.
    fn drain_due(&self, inst: &mut StubInstance) -> Vec<Message> {
        let now = self.clock.now();
        let mut delivered = Vec::new();
        while let Some(front) = inst.outbox.front() {
            if front.due_ms > now {
                break;
            }
            if let Some(entry) = inst.outbox.pop_front()
                && inst.delivered.insert(entry.message.message_id.clone())
            {
                delivered.push(entry.message);
            }
        }
        delivered
    }

    /// Apply the reaction scripted for the trigger, scheduling replies and results.
    fn apply_reaction(&self, inst: &mut StubInstance, trigger: &Message) {
        let body = trigger.body.as_str();
        let reaction = self
            .script
            .reactions
            .iter()
            .find(|(needle, _)| body.contains(needle))
            .map(|(_, reaction)| reaction.clone())
            .unwrap_or_else(|| self.script.default.clone());

        let agent = Some(inst.definition.id.clone());
        let origin = Some(inst.handle.instance_id.clone());
        let thread = trigger.thread_id.clone();
        let now = self.clock.now();
        for reply in reaction.replies {
            inst.outbox.push_back(OutboxEntry {
                due_ms: now + reply.delay_ms,
                message: Message {
                    message_id: MessageId::new(),
                    message_type: reply.message_type,
                    from_agent: agent.clone(),
                    from_instance_id: origin.clone(),
                    thread_id: thread.clone(),
                    reply_to: Some(trigger.message_id.clone()),
                    priority: Priority::Normal,
                    body: reply.body,
                    payload_json: None,
                    deadline: None,
                },
            });
        }
        if reaction.result.is_some() {
            inst.result = reaction.result.clone();
        }
        if let Some(status) = reaction.status_after.clone() {
            inst.status = status;
        }
    }
}

#[async_trait::async_trait]
impl Runtime for StubRuntime {
    fn runtime_type(&self) -> RuntimeType {
        self.config
            .lock()
            .expect("stub config poisoned")
            .runtime_kind
    }

    async fn start(
        &self,
        definition: AgentDefinition,
        ctx: StartContext,
    ) -> Result<RuntimeInstanceRef> {
        ctx.validate()?;

        let injected = self
            .config
            .lock()
            .expect("stub config poisoned")
            .fail_start
            .pop_front();
        if let Some(failure) = injected {
            return Err(failure);
        }

        let handle = RuntimeInstanceRef {
            runtime_type: self.runtime_type(),
            runtime_instance_id: athena_types::RuntimeInstanceId::new(),
            instance_id: ctx.instance_id.clone(),
        };
        let inst = StubInstance {
            handle: handle.clone(),
            definition,
            context: ctx,
            started_at: self.clock.now(),
            status: RuntimeInstanceStatus::Running,
            outbox: VecDeque::new(),
            delivered: HashSet::new(),
            last_heartbeat_at: self.clock.now(),
            heartbeat_observations: Vec::new(),
            result: None,
        };
        let key = key_of(&handle);
        self.instances
            .lock()
            .expect("stub instances poisoned")
            .insert(key, inst);
        Ok(handle)
    }

    async fn send(&self, instance: &RuntimeInstanceRef, message: Message) -> Result<()> {
        let injected = self
            .config
            .lock()
            .expect("stub config poisoned")
            .fail_send
            .pop_front();
        if let Some(failure) = injected {
            return Err(failure);
        }

        let mut instances = self.instances.lock().expect("stub instances poisoned");
        let inst = instances.get_mut(&key_of(instance)).ok_or_else(|| {
            RuntimeError::UnknownInstance(instance.runtime_instance_id.as_str().into())
        })?;

        if !is_live(&inst.status) {
            return Err(RuntimeError::UnknownInstance(
                "instance is no longer live; routing ends in Core, not in a dead process".into(),
            ));
        }
        if self.lease_expired(inst) {
            // ATH-CORE-047: a lapsed heartbeat lease self-terminates the instance.
            inst.status = RuntimeInstanceStatus::Failed {
                detail: "heartbeat lease lapsed; self-terminated (ATH-CORE-047)".into(),
            };
            return Err(RuntimeError::Failure(
                "instance self-terminated on a lapsed heartbeat lease".into(),
            ));
        }

        match message.message_type {
            // Control messages jump the queue and stop the instance (ATH-MSG-017).
            MessageType::Cancel | MessageType::Interrupt => {
                inst.status = RuntimeInstanceStatus::Stopped { exit_code: None };
                Ok(())
            }
            // Instruction-like traffic is what the script reacts to.
            MessageType::Instruction
            | MessageType::Status
            | MessageType::Question
            | MessageType::Answer => {
                self.apply_reaction(inst, &message);
                Ok(())
            }
            // Upstream kinds are acknowledged; the Broker owns their durability.
            MessageType::Report
            | MessageType::Complete
            | MessageType::Error
            | MessageType::ApprovalRequest
            | MessageType::ControlRequest
            | MessageType::ControlResponse => Ok(()),
        }
    }

    fn receive(
        &self,
        instance: &RuntimeInstanceRef,
    ) -> futures::stream::BoxStream<'static, Message> {
        let mut messages: Vec<Message> = {
            let mut instances = self.instances.lock().expect("stub instances poisoned");
            let inst = instances.get_mut(&key_of(instance));
            match inst {
                None => Vec::new(),
                Some(inst) => {
                    let due = self.drain_due(inst);
                    if self.auto_heartbeat_due(inst) {
                        // A healthy agent renews its own lease on cadence.
                        inst.last_heartbeat_at = self.clock.now();
                        inst.heartbeat_observations.push(self.clock.now());
                    }
                    due
                }
            }
        };
        messages.sort_by_key(|message| message.message_id.as_str().to_string());
        boxed_stream(futures::stream::iter(messages))
    }

    async fn status(&self, instance: &RuntimeInstanceRef) -> Result<RuntimeInstanceStatus> {
        let instances = self.instances.lock().expect("stub instances poisoned");
        let inst = instances.get(&key_of(instance)).ok_or_else(|| {
            RuntimeError::UnknownInstance(instance.runtime_instance_id.as_str().into())
        })?;

        if !is_live(&inst.status) {
            return Ok(inst.status.clone());
        }
        // ATH-RT-006: the Runtime enforces both deadlines (§6.6 Deadline semantics).
        // The startup budget applies while the INSTANCE has not become Running yet; the
        // execution budget applies to everything after that, excluding human waiting
        // (ATH-FAIL-014), which this stub tracks as virtual time the caller controls.
        match &inst.status {
            RuntimeInstanceStatus::Starting => {
                if self.clock.now().saturating_sub(inst.started_at)
                    > inst.context.deadlines.startup_ms
                {
                    return Err(RuntimeError::Deadline("startup deadline passed".into()));
                }
            }
            _ => {
                if self.clock.now().saturating_sub(inst.started_at)
                    > inst.context.deadlines.execution_ms
                {
                    return Err(RuntimeError::Deadline("execution deadline passed".into()));
                }
            }
        }
        if self.lease_expired(inst) {
            return Ok(RuntimeInstanceStatus::Failed {
                detail: "heartbeat lease lapsed; self-terminated (ATH-CORE-047)".into(),
            });
        }
        Ok(inst.status.clone())
    }

    async fn stop(&self, instance: &RuntimeInstanceRef, reason: StopReason) -> Result<()> {
        let mut instances = self.instances.lock().expect("stub instances poisoned");
        let inst = instances.get_mut(&key_of(instance)).ok_or_else(|| {
            RuntimeError::UnknownInstance(instance.runtime_instance_id.as_str().into())
        })?;
        inst.status = match reason {
            StopReason::Completed => RuntimeInstanceStatus::Stopped { exit_code: Some(0) },
            StopReason::Failed
            | StopReason::Timeout
            | StopReason::Cancelled
            | StopReason::Shutdown
            | StopReason::Suspend => RuntimeInstanceStatus::Stopped { exit_code: None },
        };
        Ok(())
    }

    async fn collect_result(&self, instance: &RuntimeInstanceRef) -> Result<ExecutionResult> {
        let mut instances = self.instances.lock().expect("stub instances poisoned");
        let inst = instances.get_mut(&key_of(instance)).ok_or_else(|| {
            RuntimeError::UnknownInstance(instance.runtime_instance_id.as_str().into())
        })?;

        let spec = inst.result.take().ok_or_else(|| {
            RuntimeError::Failure("no scripted result was produced (ATH-ACLI-005)".into())
        })?;
        let task_id = inst.context.task_id.clone().ok_or_else(|| {
            RuntimeError::Validation(
                "no Task is bound to this instance, so a result cannot be attributed".into(),
            )
        })?;
        let now = self.clock.now();
        Ok(ExecutionResult {
            id: athena_types::ResultId::new(),
            task_id,
            plan_step_id: inst.context.plan_step_id.clone(),
            agent_id: inst.definition.id.clone(),
            instance_id: inst.handle.instance_id.clone(),
            status: spec.status,
            output: spec.output,
            artifacts: vec![],
            logs_ref: None,
            skills_used: vec![],
            model: Some(inst.context.model.clone()),
            started_at: format!("{now}"),
            finished_at: format!("{}", now + spec.execution_time_ms),
            execution_time_ms: spec.execution_time_ms,
            metadata: serde_json::json!({ "provisional": true }),
        })
    }

    fn capabilities(&self) -> RuntimeCapabilities {
        let config = self.config.lock().expect("stub config poisoned");
        RuntimeCapabilities {
            runtime_type: config.runtime_kind,
            isolation: config.isolation,
            supported_tools: vec![
                ToolName::FsRead,
                ToolName::FsWrite,
                ToolName::Search,
                ToolName::SendTo,
            ],
            max_concurrent_instances: u64::from(u8::MAX),
        }
    }
}
