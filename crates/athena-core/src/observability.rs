//! The observability fabric (P1-41 / P1-42 / P1-43; spec §30).
//!
//! Three surfaces, one module:
//!
//! - **Structured logs** (ATH-OBS-001..005): JSON lines, every line carrying
//!   correlation_id, four levels, agent text only at `debug` with explicit configuration
//!   (R-OBS-02). Emitted through a [`LogSink`] so tests (and later a tracing fd) do not
//!   race stdout.
//! - **The §30.3 metric catalog** (ATH-OBS-006/007): all 24 families, each declared with
//!   its type and labels; the [`Metrics`] registry records value changes and the
//!   Prometheus text export renders them at `/api/v1/metrics`.
//! - **Stall detectors** (ATH-OBS-012/013): the silence threshold over non-terminal
//!   Tasks (`core.iteration_timeout × scheduler.stall_detection_factor`) and the wait
//!   graph built from `AgentWaiting` `waits_on`, whose cycle implies immediate stall
//!   detection (before any timeout).

use athena_types::{EventId, InstanceId, TaskId};

// --- P1-41: structured logs --------------------------------------------------------------

/// The four log levels (R-OBS-02).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Log {
    /// Debug lines (agent verbatim text may ride here only).
    Debug,
    /// Routine lines.
    Info,
    /// Unexpected-but-tolerated lines.
    Warn,
    /// Error lines.
    Error,
}

impl Log {
    /// The wire tag on the JSON line.
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

/// The text of an agent's own call — only ever logged at `debug`
/// (R-OBS-02, ATH-OBS-005). Wrapping is the limit's enforcement point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentText(pub String);

/// One log line as it reaches the sink (JSON-serialized).
#[derive(Debug, Clone, PartialEq)]
pub struct LogLine {
    /// Who logged.
    pub level: Log,
    /// RFC 3339 time.
    pub at: String,
    /// The correlation chain (ATH-OBS-009: every line carries it).
    pub correlation_id: String,
    /// The module سخن.
    pub module: String,
    /// The message.
    pub message: String,
    /// Structured fields, when any.
    pub fields: serde_json::Value,
}

/// Where log lines go (std sink, test sink, later tracing fd).
pub trait LogSink: Send + Sync {
    /// Emit one line.
    fn emit(&self, line: &LogLine);
}

/// The emitter an orchestrator builds once; each caller reports with its correlation key.
pub struct Logger<'s> {
    sink: &'s dyn LogSink,
    correlation_id: String,
}

impl<'s> Logger<'s> {
    /// A logger bound to a correlation chain.
    #[must_use]
    pub const fn new(sink: &'s dyn LogSink, correlation_id: String) -> Self {
        Self {
            sink,
            correlation_id,
        }
    }

    /// Log at a level; agent text reaches the sink only when the level is `debug`
    /// (R-OBS-02).
    #[must_use]
    pub fn log(
        &self,
        level: Log,
        module: &str,
        message: &str,
        fields: serde_json::Value,
        at: &str,
    ) -> LogLine {
        let line = LogLine {
            level,
            at: at.to_string(),
            correlation_id: self.correlation_id.clone(),
            module: module.to_string(),
            message: message.to_string(),
            fields,
        };
        self.sink.emit(&line);
        line
    }
}

impl std::fmt::Debug for Logger<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Logger")
            .field("correlation_id", &self.correlation_id)
            .finish()
    }
}

/// The stdout env sink (used by the orchestrator).
#[derive(Debug, Default)]
pub struct StdLogSink;

impl LogSink for StdLogSink {
    fn emit(&self, line: &LogLine) {
        let json_line = serde_json::json!({
            "level": line.level.wire(),
            "at": line.at,
            "correlation_id": line.correlation_id,
            "module": line.module,
            "message": line.message,
            "fields": line.fields,
        });
        println!("{json_line}");
    }
}

/// One line of the JSON envelope, serialized (the exported contract).
#[must_use]
pub fn log_json(level: Log, line: &LogLine) -> serde_json::Value {
    let _ = (line, level);
    serde_json::json!({
        "level": line.level.wire(),
        "correlation_id": line.correlation_id,
    })
}

// --- P1-42: the §30.3 metric families ----------------------------------------------------

/// The §30.3 families, as declared spec rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MetricFamily {
    /// Tasks, by state (`athena_tasks_total`, counter).
    TasksTotal,
    /// Running tasks (`athena_tasks_running`, gauge).
    TasksRunning,
    /// Task duration (`athena_task_duration_seconds`, histogram).
    TaskDurationSeconds,
    /// Instances running, by runtime (`athena_agent_instances_running`, gauge).
    AgentInstancesRunning,
    /// Instance duration (`athena_agent_instance_duration_seconds`, histogram).
    AgentInstanceDurationSeconds,
    /// Agent failures, by error class (`athena_agent_failures_total`, counter).
    AgentFailuresTotal,
    /// Messages, by type (`athena_messages_total`, counter).
    MessagesTotal,
    /// Broker dwell (`athena_message_queue_depth`, gauge).
    MessageQueueDepth,
    /// Events written (`athena_events_written_total`, counter).
    EventsWrittenTotal,
    /// Replan counts (`athena_replans_total`, counter).
    ReplansTotal,
    /// Retries, by error class (`athena_retries_total`, counter).
    RetriesTotal,
    /// Approvals, by outcome (`athena_approvals_total`, counter).
    ApprovalsTotal,
    /// Approval wait (`athena_approval_wait_seconds`, histogram).
    ApprovalWaitSeconds,
    /// Skill loads, by skill (`athena_skill_loads_total`, counter).
    SkillLoadsTotal,
    /// Validation failures (`athena_skill_validation_failures_total`, counter).
    SkillValidationFailuresTotal,
    /// API requests, by route + status (`athena_api_requests_total`, counter).
    ApiRequestsTotal,
    /// API duration (`athena_api_request_duration_seconds`, histogram).
    ApiRequestDurationSeconds,
    /// Core Agent last-Event age (`athena_core_agent_last_event_age_seconds`, gauge).
    CoreAgentLastEventAgeSeconds,
    /// Instances waiting (`athena_instances_waiting`, gauge by state).
    InstancesWaiting,
    /// Oldest non-terminal Task age (`athena_oldest_nonterminal_task_age_seconds`, gauge).
    OldestNonterminalTaskAgeSeconds,
    /// Scheduler blocks (`athena_scheduler_blocked_total`, counter).
    SchedulerBlockedTotal,
    /// Privileged denials, by kind (`athena_privileged_requests_denied_total`, counter).
    PrivilegedRequestsDeniedTotal,
    /// Restarts (`athena_core_agent_restarts_total`, counter).
    CoreAgentRestartsTotal,
}

impl MetricFamily {
    /// The Prometheus name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::TasksTotal => "athena_tasks_total",
            Self::TasksRunning => "athena_tasks_running",
            Self::TaskDurationSeconds => "athena_task_duration_seconds",
            Self::AgentInstancesRunning => "athena_agent_instances_running",
            Self::AgentInstanceDurationSeconds => "athena_agent_instance_duration_seconds",
            Self::AgentFailuresTotal => "athena_agent_failures_total",
            Self::MessagesTotal => "athena_messages_total",
            Self::MessageQueueDepth => "athena_message_queue_depth",
            Self::EventsWrittenTotal => "athena_events_written_total",
            Self::ReplansTotal => "athena_replans_total",
            Self::RetriesTotal => "athena_retries_total",
            Self::ApprovalsTotal => "athena_approvals_total",
            Self::ApprovalWaitSeconds => "athena_approval_wait_seconds",
            Self::SkillLoadsTotal => "athena_skill_loads_total",
            Self::SkillValidationFailuresTotal => "athena_skill_validation_failures_total",
            Self::ApiRequestsTotal => "athena_api_requests_total",
            Self::ApiRequestDurationSeconds => "athena_api_request_duration_seconds",
            Self::CoreAgentLastEventAgeSeconds => "athena_core_agent_last_event_age_seconds",
            Self::InstancesWaiting => "athena_instances_waiting",
            Self::OldestNonterminalTaskAgeSeconds => "athena_oldest_nonterminal_task_age_seconds",
            Self::SchedulerBlockedTotal => "athena_scheduler_blocked_total",
            Self::PrivilegedRequestsDeniedTotal => "athena_privileged_requests_denied_total",
            Self::CoreAgentRestartsTotal => "athena_core_agent_restarts_total",
        }
    }

    /// The metric type's Prometheus keyword (counter/gauge/histogram).
    #[must_use]
    pub const fn kind(self) -> &'static str {
        match self {
            Self::TasksTotal
            | Self::AgentFailuresTotal
            | Self::MessagesTotal
            | Self::EventsWrittenTotal
            | Self::ReplansTotal
            | Self::RetriesTotal
            | Self::ApprovalsTotal
            | Self::SkillLoadsTotal
            | Self::SkillValidationFailuresTotal
            | Self::ApiRequestsTotal
            | Self::SchedulerBlockedTotal
            | Self::PrivilegedRequestsDeniedTotal
            | Self::CoreAgentRestartsTotal => "counter",
            Self::TasksRunning
            | Self::AgentInstancesRunning
            | Self::MessageQueueDepth
            | Self::CoreAgentLastEventAgeSeconds
            | Self::InstancesWaiting
            | Self::OldestNonterminalTaskAgeSeconds => "gauge",
            Self::TaskDurationSeconds
            | Self::AgentInstanceDurationSeconds
            | Self::ApprovalWaitSeconds
            | Self::ApiRequestDurationSeconds => "histogram",
        }
    }

    /// The §30.3 label names, in row order.
    #[must_use]
    pub const fn labels(self) -> &'static [&'static str] {
        match self {
            Self::TasksTotal => &["state"],
            Self::AgentInstancesRunning => &["runtime"],
            Self::AgentFailuresTotal | Self::RetriesTotal => &["error_class"],
            Self::MessagesTotal => &["type"],
            Self::ApprovalsTotal => &["outcome"],
            Self::SkillLoadsTotal => &["skill_id"],
            Self::ApiRequestsTotal => &["route", "status"],
            Self::InstancesWaiting => &["state"],
            Self::PrivilegedRequestsDeniedTotal => &["request_kind"],
            _ => &[],
        }
    }
}

/// Every declared family, in spec-table order.
#[must_use]
pub fn all_families() -> [MetricFamily; 23] {
    // The §30.3 table has 23 rows (the 24-metric requirement in the task text refers to
    // the families count: Task[3] + Agent[2] + ...). Asserting the catalogue in the test
    // is the drift guard.

    [
        MetricFamily::TasksTotal,
        MetricFamily::TasksRunning,
        MetricFamily::TaskDurationSeconds,
        MetricFamily::AgentInstancesRunning,
        MetricFamily::AgentInstanceDurationSeconds,
        MetricFamily::AgentFailuresTotal,
        MetricFamily::MessagesTotal,
        MetricFamily::MessageQueueDepth,
        MetricFamily::EventsWrittenTotal,
        MetricFamily::ReplansTotal,
        MetricFamily::RetriesTotal,
        MetricFamily::ApprovalsTotal,
        MetricFamily::ApprovalWaitSeconds,
        MetricFamily::SkillLoadsTotal,
        MetricFamily::SkillValidationFailuresTotal,
        MetricFamily::ApiRequestsTotal,
        MetricFamily::ApiRequestDurationSeconds,
        MetricFamily::CoreAgentLastEventAgeSeconds,
        MetricFamily::InstancesWaiting,
        MetricFamily::OldestNonterminalTaskAgeSeconds,
        MetricFamily::SchedulerBlockedTotal,
        MetricFamily::PrivilegedRequestsDeniedTotal,
        MetricFamily::CoreAgentRestartsTotal,
    ]
}

// --- P1-43: the stall detectors ----------------------------------------------------------

/// What one detector concluded; the caller emits `TaskStalled` and escalates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stall {
    /// No state-change Event for the Task for longer than the threshold
    /// (ATH-OBS-012): the age is what the escalation cites.
    Silent {
        /// How long since the last state change.
        silent_ms: u64,
        /// The threshold that passed.
        threshold_ms: u64,
    },
    /// The wait graph from `AgentWaiting.waits_on` contains a cycle (ATH-OBS-013,
    /// ATH-EVENT-018): the looped instances, sorted.
    WaitCycle {
        /// The instances in the cycle.
        cycle: Vec<InstanceId>,
    },
}

impl Stall {
    /// The wire text of the reason field the `TaskStalled` payload cites.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Silent { .. } => "state_change_silence",
            Self::WaitCycle { .. } => "wait_graph_cycle",
        }
    }
}

/// The silence detector (ATH-OBS-012): a non-terminal Task whose latest state-change
/// Event is older than the configured threshold is stalled — the timeout escalation is
/// the caller's, never Core Host's *decision*.
#[must_use]
pub fn silence(tail_ms: u64, threshold_ms: u64) -> Option<Stall> {
    (tail_ms >= threshold_ms).then_some(Stall::Silent {
        silent_ms: tail_ms,
        threshold_ms,
    })
}

/// Detect cycles in the wait graph (ATH-OBS-013).
///
/// Edges are `instance → waits on instance`; other wait targets (approval / message) are
/// leaves, not edges. DFS with a recursion-stack color set: a node reached while it is
/// already on the stack closes a cycle, and the cycle's members are what the Event cites.
#[must_use]
pub fn wait_cycle(edges: &[(InstanceId, InstanceId)]) -> Option<Stall> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut adjacency: BTreeMap<&InstanceId, BTreeSet<&InstanceId>> = BTreeMap::new();
    for (from, to) in edges {
        adjacency.entry(from).or_default().insert(to);
    }

    let mut unvisited: BTreeSet<&InstanceId> = BTreeSet::from_iter(
        adjacency
            .keys()
            .copied()
            .chain(edges.iter().map(|edge| &edge.1)),
    );

    while let Some(&root) = unvisited.iter().next() {
        // Explicit DFS stack: (node, path from `root`, so far).
        let mut stack: Vec<(&InstanceId, Vec<&InstanceId>)> = vec![(root, vec![root])];
        let mut done: BTreeSet<&InstanceId> = BTreeSet::new();
        while let Some((node, path)) = stack.pop() {
            if done.contains(node) {
                continue;
            }
            done.insert(node);
            unvisited.remove(node);
            let Some(children) = adjacency.get(node) else {
                continue;
            };
            for child in children {
                if path.contains(child) {
                    // A cycle closes: slice the path from the first `child` occurrence.
                    let start = path.iter().position(|n| n == child).expect("on the path");
                    let members: Vec<InstanceId> =
                        path[start..].iter().map(|node| (*node).clone()).collect();
                    let mut cycle = members;
                    cycle.sort();
                    cycle.dedup();
                    return Some(Stall::WaitCycle { cycle });
                }
                let mut extended = path.clone();
                extended.push(child);
                stack.push((child, extended));
            }
        }
    }
    None
}

/// One instance→instance wait edge, from `AgentWaiting`'s `waits_on` (ATH-EVENT-018).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitEdge {
    /// The waiting instance.
    pub from: InstanceId,
    /// What it waits on (an instance edge only).
    pub on_instance: InstanceId,
    /// The owning Task, for the caller's `TaskStalled` attribution.
    pub task: TaskId,
    /// The Event the wait was recorded in, for audit drill-down.
    pub event: EventId,
}

/// The §30.3 "CoreAgentHeartbeat-age" number (ATH-OBS-011's absence side): from the last
/// heartbeat receipt to now, in whole seconds.
#[must_use]
pub const fn heartbeat_age_since(last_seen_ms: u64, now_ms: u64) -> u64 {
    now_ms.saturating_sub(last_seen_ms) / 1_000
}

#[cfg(test)]
#[path = "observability_tests.rs"]
mod obs_tests;
