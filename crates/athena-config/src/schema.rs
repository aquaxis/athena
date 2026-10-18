//! Configuration schema (spec section 28.1).
//!
//! Every section of the specification's `config.yaml` is represented. `deny_unknown_fields` is
//! set throughout: a mistyped key must abort startup rather than silently leave a default in
//! place (ATH-CFG-003).
//!
//! Default values are the specification's documented defaults and constitute the lowest
//! resolution layer (ATH-CFG-001). Where the specification marks a default as provisional
//! pending measurement, the field documents that (ATH-SCHED-015).

use athena_types::{Priority, RiskLevel, ToolName};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

/// Seconds helper for default durations.
const fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// The whole configuration.
///
/// `Default` is derived: every section supplies its own specification default, so the derived
/// impl *is* the defaults layer (ATH-CFG-001).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Paths, API binding and data locations.
    pub athena: AthenaSection,
    /// Core Agent Loop and Core Agent process settings.
    pub core: CoreSection,
    /// Scheduler limits.
    pub scheduler: SchedulerSection,
    /// Message Broker settings.
    pub messaging: MessagingSection,
    /// Timeouts shared by all Runtimes.
    pub timeout: TimeoutSection,
    /// Risk classification and approval scopes.
    pub policy: PolicySection,
    /// Runtime settings, keyed by Runtime type.
    pub runtime: RuntimeSection,
    /// Skill source, registry and allow-lists.
    pub skills: SkillsSection,
    /// Memory tiers.
    pub memory: MemorySection,
    /// Approval behaviour.
    pub approval: ApprovalSection,
    /// Core Agent restart policy.
    pub core_agent_restart: CoreAgentRestartSection,
    /// Retry policy.
    pub retry: RetrySection,
    /// Event retention.
    pub events: EventsSection,
    /// Logging.
    pub observability: ObservabilitySection,
    /// Agent definitions registered from configuration.
    pub agents: Vec<AgentEntry>,
}

/// `athena` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AthenaSection {
    /// `ATHENA_HOME`: where ATHENA's files live, always `<PROJECT_ROOT>/.athena`
    /// (ATH-OPS-005). Resolved by search rather than taken from this value alone
    /// (ATH-CFG-010).
    pub home: String,
    /// API binding.
    pub api: ApiSection,
    /// Data locations.
    pub data: DataSection,
}

impl Default for AthenaSection {
    fn default() -> Self {
        Self {
            home: "./.athena".into(),
            api: ApiSection::default(),
            data: DataSection::default(),
        }
    }
}

/// `athena.api` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ApiSection {
    /// Bind host. Loopback by default, since v1 is single-user single-host (ATH-OPS-004).
    pub host: String,
    /// Bind port. Must differ per project when several run on one host (ATH-OPS-007).
    pub port: u16,
    /// Authentication.
    pub auth: AuthSection,
}

impl Default for ApiSection {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8080,
            auth: AuthSection::default(),
        }
    }
}

/// Authentication mode (spec section 28.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    /// Static bearer tokens, the v1 scheme (ATH-SEC-027).
    Token,
    /// No authentication. Permitted only on loopback (ATH-CFG-006).
    None,
}

/// `athena.api.auth` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AuthSection {
    /// Mode.
    pub mode: AuthMode,
}

impl Default for AuthSection {
    fn default() -> Self {
        Self {
            mode: AuthMode::Token,
        }
    }
}

/// `athena.data` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DataSection {
    /// Runtime data directory. Must stay under `var/` so the version-control boundary is one
    /// path (ATH-OPS-006).
    pub dir: String,
    /// SQLite database location (ATH-PERS-011).
    pub database: String,
}

impl Default for DataSection {
    fn default() -> Self {
        Self {
            dir: "${athena.home}/var".into(),
            database: "${athena.home}/var/athena.db".into(),
        }
    }
}

/// `core` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CoreSection {
    /// Agent Loop iteration ceiling (ATH-CORE-013).
    pub max_iterations: u64,
    /// Per-iteration time budget.
    #[serde(with = "humantime_serde")]
    pub iteration_timeout: Duration,
    /// Evaluation policy.
    pub evaluation: EvaluationSection,
    /// Core Agent process settings.
    pub agent: CoreAgentSection,
}

impl Default for CoreSection {
    fn default() -> Self {
        Self {
            max_iterations: 50,
            iteration_timeout: secs(30 * 60),
            evaluation: EvaluationSection::default(),
            agent: CoreAgentSection::default(),
        }
    }
}

/// `core.evaluation` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct EvaluationSection {
    /// Whether verifiable deliverables must pass Verification before acceptance
    /// (ATH-EVAL-002).
    pub require_verification: bool,
}

impl Default for EvaluationSection {
    fn default() -> Self {
        Self {
            require_verification: true,
        }
    }
}

/// `core.agent` section: the Core Agent on `agent-cli` instance #0.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CoreAgentSection {
    /// Producer persona (ATH-CORE-024).
    pub persona: String,
    /// Dedicated Workspace, never shared with subordinates (ATH-CORE-023).
    pub workspace: String,
    /// Runtime type name.
    pub runtime: String,
    /// LLM provider. No ATHENA-side default exists, so an empty value is a validation error
    /// (ATH-AGENT-008).
    pub provider: String,
    /// Model name. Same rule as `provider`.
    pub model: String,
    /// Skills granted to the Core Agent.
    pub skills: Vec<SkillEntry>,
    /// Restart policy.
    pub restart: RestartSection,
    /// Tools the Core Agent may execute itself. Minimal by default; `shell`, `process` and
    /// `network` must not appear here (ATH-SEC-025).
    pub allowed_tools: Vec<ToolName>,
    /// Tools the Core Agent may grant downward. This is ATHENA's overall permission ceiling,
    /// so it must be stated explicitly and is never unlimited (ATH-SEC-023).
    pub delegable_tools: Vec<ToolName>,
}

impl Default for CoreAgentSection {
    fn default() -> Self {
        Self {
            persona: "${athena.home}/personas/producer.md".into(),
            workspace: "${athena.home}/var/core-agent".into(),
            runtime: "agent-cli".into(),
            // Deliberately empty: there is no ATHENA-side default provider or model, and
            // validation rejects an unset value (ATH-AGENT-008).
            provider: String::new(),
            model: String::new(),
            skills: Vec::new(),
            restart: RestartSection::default(),
            allowed_tools: vec![ToolName::FsRead, ToolName::FsWrite, ToolName::SendTo],
            // Empty by default (ATH-SEC-024); the operator must opt in to what subordinates
            // may receive.
            delegable_tools: Vec::new(),
        }
    }
}

/// What to do when Core Agent restarts are exhausted. Only escalation is permitted
/// (ATH-CORE-030).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnExhausted {
    /// Hand control to a human and stop autonomous execution.
    Escalate,
}

/// `core.agent.restart` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RestartSection {
    /// Attempt ceiling.
    pub max_attempts: u64,
    /// Behaviour on exhaustion.
    pub on_exhausted: OnExhausted,
}

impl Default for RestartSection {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            on_exhausted: OnExhausted::Escalate,
        }
    }
}

/// A skill reference in configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillEntry {
    /// Skill slug.
    pub id: String,
    /// Semantic version.
    pub version: String,
}

/// An agent definition reference in configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentEntry {
    /// Agent slug.
    pub id: String,
    /// Runtime type name.
    pub runtime: String,
}

/// `scheduler` section.
///
/// Every default here is a provisional initial value for a single development host, to be
/// settled by measurement and a spec revision (ATH-SCHED-015). They are all overridable, which
/// is the requirement that forbids hard-coding them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SchedulerSection {
    /// Concurrent Tasks in `RUNNING` or later.
    pub max_concurrent_tasks: u64,
    /// Concurrent Agent Instances. `WAITING` and `SUSPENDED` are excluded from the count
    /// (ATH-SCHED-010).
    pub max_concurrent_instances: u64,
    /// Concurrent Instances under one Task.
    pub max_instances_per_task: u64,
    /// Queue length ceiling; beyond it, `TransientError` (ATH-SCHED-004).
    pub max_queue_length: u64,
    /// Anti-starvation promotion interval (ATH-SCHED-006).
    #[serde(with = "humantime_serde")]
    pub priority_promotion_interval: Duration,
    /// Core Agent slots, held outside `max_concurrent_instances` (ATH-SCHED-008).
    pub reserved_core_agent_slots: u64,
    /// Share of instances that may be awaiting approval (ATH-HITL-016).
    pub max_waiting_approval_ratio: f64,
    /// Multiple of `core.iteration_timeout` after which `TaskStalled` fires (ATH-OBS-012).
    pub stall_detection_factor: u64,
}

impl Default for SchedulerSection {
    fn default() -> Self {
        Self {
            max_concurrent_tasks: 8,
            max_concurrent_instances: 16,
            max_instances_per_task: 4,
            max_queue_length: 1000,
            priority_promotion_interval: secs(30 * 60),
            reserved_core_agent_slots: 2,
            max_waiting_approval_ratio: 0.25,
            stall_detection_factor: 2,
        }
    }
}

/// `messaging` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MessagingSection {
    /// Broker backlog ceiling (ATH-MSG-009).
    pub max_queue_depth: u64,
    /// How long delivery records are kept for deduplication (ATH-MSG-014).
    #[serde(with = "humantime_serde")]
    pub dedup_retention: Duration,
}

impl Default for MessagingSection {
    fn default() -> Self {
        Self {
            max_queue_depth: 1000,
            dedup_retention: secs(7 * 24 * 60 * 60),
        }
    }
}

/// `timeout` section (spec sections 20.3 and 28.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TimeoutSection {
    /// Instance startup budget.
    #[serde(with = "humantime_serde")]
    pub startup: Duration,
    /// Instance execution budget. Excludes time awaiting a human (ATH-FAIL-014).
    #[serde(with = "humantime_serde")]
    pub execution: Duration,
    /// Reply deadline. Must exceed `startup + execution` (ATH-CFG-008).
    #[serde(with = "humantime_serde")]
    pub message_reply: Duration,
    /// Approval response deadline (ATH-HITL-004).
    #[serde(with = "humantime_serde")]
    pub approval: Duration,
    /// Graceful stop budget before force-kill (ATH-FAIL-015).
    #[serde(with = "humantime_serde")]
    pub stop: Duration,
    /// Clarification response deadline (ATH-HITL-019).
    #[serde(with = "humantime_serde")]
    pub clarification: Duration,
}

impl Default for TimeoutSection {
    fn default() -> Self {
        Self {
            startup: secs(30),
            execution: secs(30 * 60),
            message_reply: secs(45 * 60),
            approval: secs(60 * 60),
            stop: secs(60),
            clarification: secs(24 * 60 * 60),
        }
    }
}

/// How a policy rule matches an operation (spec section 28.1).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PolicyMatch {
    /// Tool the rule applies to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<ToolName>,
    /// Regular expression over the operation's arguments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args_pattern: Option<String>,
    /// Host list, where `*` means any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hosts: Option<Vec<String>>,
    /// Coarse path scope, e.g. `outside_workspace`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_scope: Option<String>,
}

/// One risk classification rule. Rules are evaluated top-down, first match wins
/// (ATH-CFG-009).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRule {
    /// Match criteria.
    #[serde(rename = "match")]
    pub matcher: PolicyMatch,
    /// Risk assigned on match.
    pub risk: RiskLevel,
    /// Whether an approval is required.
    #[serde(default)]
    pub requires_approval: bool,
}

/// `policy` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PolicySection {
    /// Ordered rules.
    pub rules: Vec<PolicyRule>,
    /// Selectable approval scopes. An agent may not invent a pattern (ATH-HITL-012).
    pub approval_scopes: Vec<String>,
    /// Ceiling on a scoped approval's lifetime (ATH-HITL-013).
    #[serde(with = "humantime_serde")]
    pub max_approval_ttl: Duration,
}

impl Default for PolicySection {
    fn default() -> Self {
        // The specification's default rule set (spec section 28.1). Order is significant.
        let rule = |matcher, risk, requires_approval| PolicyRule {
            matcher,
            risk,
            requires_approval,
        };
        Self {
            rules: vec![
                rule(
                    PolicyMatch {
                        tool: Some(ToolName::Shell),
                        args_pattern: Some(r"^git\s+push".into()),
                        ..PolicyMatch::default()
                    },
                    RiskLevel::High,
                    true,
                ),
                rule(
                    PolicyMatch {
                        tool: Some(ToolName::Shell),
                        args_pattern: Some(r"^rm\s+-rf".into()),
                        ..PolicyMatch::default()
                    },
                    RiskLevel::High,
                    true,
                ),
                rule(
                    PolicyMatch {
                        tool: Some(ToolName::Network),
                        hosts: Some(vec!["*".into()]),
                        ..PolicyMatch::default()
                    },
                    RiskLevel::High,
                    true,
                ),
                rule(
                    PolicyMatch {
                        tool: Some(ToolName::FsWrite),
                        path_scope: Some("outside_workspace".into()),
                        ..PolicyMatch::default()
                    },
                    RiskLevel::High,
                    true,
                ),
                rule(
                    PolicyMatch {
                        tool: Some(ToolName::Shell),
                        ..PolicyMatch::default()
                    },
                    RiskLevel::Medium,
                    false,
                ),
                rule(
                    PolicyMatch {
                        tool: Some(ToolName::FsRead),
                        ..PolicyMatch::default()
                    },
                    RiskLevel::Low,
                    false,
                ),
            ],
            approval_scopes: vec![
                "operation_only".into(),
                "tool:shell:same_argv".into(),
                "tool:fs_write:same_path".into(),
            ],
            max_approval_ttl: secs(24 * 60 * 60),
        }
    }
}

/// Per-Runtime settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RuntimeEntry {
    /// Concurrent instance ceiling for this Runtime (ATH-RT-009).
    pub max_instances: u64,
    /// Executable or command name.
    pub command: String,
    /// Optional per-Runtime override of `timeout.startup` (ATH-CFG-008).
    #[serde(default, with = "humantime_serde")]
    pub startup_timeout: Option<Duration>,
    /// Optional per-Runtime override of `timeout.execution`.
    #[serde(default, with = "humantime_serde")]
    pub execution_timeout: Option<Duration>,
}

impl Default for RuntimeEntry {
    fn default() -> Self {
        Self {
            max_instances: 16,
            command: "agent-cli".into(),
            startup_timeout: None,
            execution_timeout: None,
        }
    }
}

/// `runtime` section.
///
/// This is the one section without `deny_unknown_fields`, because the per-Runtime settings are
/// a flattened open map and serde cannot combine `flatten` with `deny_unknown_fields`. The
/// openness is deliberate — adding a Runtime must not require a Core change (ATH-RT-008) — but
/// it means a mistyped Runtime name becomes a new entry instead of an error, so validation
/// requires `default` to name an entry that exists.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeSection {
    /// Runtime used when a definition does not say.
    pub default: String,
    /// Settings per Runtime type name.
    ///
    /// A map rather than named fields, so adding a Runtime needs no Core change
    /// (ATH-RT-008).
    #[serde(flatten)]
    pub entries: BTreeMap<String, RuntimeEntry>,
}

impl Default for RuntimeSection {
    fn default() -> Self {
        let mut entries = BTreeMap::new();
        entries.insert("agent-cli".to_string(), RuntimeEntry::default());
        Self {
            default: "agent-cli".into(),
            entries,
        }
    }
}

/// `skills` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SkillsSection {
    /// In-project Skill Package sources (ATH-SKILL-037).
    pub local_dir: String,
    /// Registered skills (ATH-SKILL-038). Must differ from `local_dir`.
    pub dir: String,
    /// Whether registration enables immediately. False, because validation must complete
    /// first (ATH-API-014).
    pub auto_enable_on_register: bool,
    /// Whether to verify checksums.
    pub verify_checksum: bool,
    /// Publishers whose externally fetched skills may be registered (ATH-SKILL-035).
    pub allowed_publishers: Vec<String>,
    /// Origins from which skills may be fetched (ATH-SKILL-035).
    pub allowed_sources: Vec<String>,
}

impl Default for SkillsSection {
    fn default() -> Self {
        Self {
            local_dir: "${athena.home}/skills".into(),
            dir: "${athena.home}/var/skills".into(),
            auto_enable_on_register: false,
            verify_checksum: true,
            // Empty by default: an empty allow-list denies every external fetch, which is the
            // safe starting point (ATH-SKILL-035).
            allowed_publishers: Vec::new(),
            allowed_sources: Vec::new(),
        }
    }
}

/// `memory.long_term` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LongTermSection {
    /// Whether long-term memory is active.
    pub enabled: bool,
    /// Record ceiling.
    pub max_records: u64,
}

impl Default for LongTermSection {
    fn default() -> Self {
        Self {
            enabled: true,
            max_records: 10_000,
        }
    }
}

/// `memory` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MemorySection {
    /// Tokenizer used for counting. Must be stated; relying on a provider default implicitly
    /// is forbidden (ATH-MEM-009).
    pub tokenizer: String,
    /// Agent definition that performs eviction summarization. Summarizing is inference, so
    /// Core Host delegates it rather than implementing it (ATH-MEM-008).
    pub summarizer: String,
    /// Short-term token ceiling, mirrored into `agent-cli`'s `[history]` (ATH-ACLI-010).
    pub short_term_max_tokens: u64,
    /// Working-memory record ceiling.
    pub working_max_records: u64,
    /// Long-term settings.
    pub long_term: LongTermSection,
}

impl Default for MemorySection {
    fn default() -> Self {
        Self {
            tokenizer: "provider-default-claude".into(),
            summarizer: "memory-summarizer-agent".into(),
            short_term_max_tokens: 32_000,
            working_max_records: 500,
            long_term: LongTermSection::default(),
        }
    }
}

/// What happens when an approval deadline passes. Rejection is the only permitted behaviour
/// (ATH-HITL-005).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnApprovalTimeout {
    /// Treat as rejected.
    Reject,
}

/// `approval` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ApprovalSection {
    /// Deadline behaviour.
    pub on_timeout: OnApprovalTimeout,
}

impl Default for ApprovalSection {
    fn default() -> Self {
        Self {
            on_timeout: OnApprovalTimeout::Reject,
        }
    }
}

/// `core_agent_restart` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CoreAgentRestartSection {
    /// Attempts permitted within `window` (ATH-CORE-030).
    pub max_attempts: u64,
    /// Sliding window (ATH-CORE-045).
    #[serde(with = "humantime_serde")]
    pub window: Duration,
    /// Error classes excluded from the attempt count, because a resource shortage is not the
    /// Core Agent misbehaving (ATH-CORE-045).
    pub exclude_error_classes: Vec<String>,
    /// Heartbeat emission interval (ATH-OBS-011).
    #[serde(with = "humantime_serde")]
    pub heartbeat_interval: Duration,
    /// Heartbeat lapse after which an instance self-terminates (ATH-CORE-047).
    #[serde(with = "humantime_serde")]
    pub heartbeat_timeout: Duration,
}

impl Default for CoreAgentRestartSection {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            window: secs(60 * 60),
            exclude_error_classes: vec!["TransientError".into()],
            heartbeat_interval: secs(20),
            heartbeat_timeout: secs(60),
        }
    }
}

/// Retry strategy (spec section 20.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RetryStrategy {
    /// Retry with the same agent.
    SameAgent,
    /// Try a different agent.
    AlternateAgent,
    /// Re-plan instead of retrying.
    Replan,
}

/// `retry.backoff` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BackoffSection {
    /// First delay.
    pub initial_ms: u64,
    /// Growth factor, applied to `TransientError` retries (ATH-FAIL-006).
    pub multiplier: f64,
    /// Delay ceiling.
    pub max_ms: u64,
}

impl Default for BackoffSection {
    fn default() -> Self {
        Self {
            initial_ms: 1000,
            multiplier: 2.0,
            max_ms: 60_000,
        }
    }
}

/// `retry` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RetrySection {
    /// Attempt ceiling (ATH-FAIL-005).
    pub max_attempts: u64,
    /// Strategy.
    pub strategy: RetryStrategy,
    /// Backoff.
    pub backoff: BackoffSection,
}

impl Default for RetrySection {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            strategy: RetryStrategy::AlternateAgent,
            backoff: BackoffSection::default(),
        }
    }
}

/// `events` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct EventsSection {
    /// Retention in days (ATH-EVENT-011). The sweep must preserve final results
    /// (ATH-EVENT-014) and also governs Instance area deletion (ATH-AIPRJ-029).
    pub retention_days: u64,
}

impl Default for EventsSection {
    fn default() -> Self {
        Self { retention_days: 90 }
    }
}

/// Log level (spec section 30.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// Verbose. Full agent text is logged only here, and only when explicitly configured
    /// (ATH-OBS-005).
    Debug,
    /// Normal.
    Info,
    /// Warnings.
    Warn,
    /// Errors.
    Error,
}

/// Log format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Structured JSON, required by ATH-OBS-002.
    Json,
}

/// `observability` section.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ObservabilitySection {
    /// Level.
    pub log_level: LogLevel,
    /// Format.
    pub log_format: LogFormat,
}

impl Default for ObservabilitySection {
    fn default() -> Self {
        Self {
            log_level: LogLevel::Info,
            log_format: LogFormat::Json,
        }
    }
}

/// Priority helper used by callers that need the configured default.
#[must_use]
pub fn default_priority() -> Priority {
    Priority::Normal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_specification_section_28_1() {
        let c = Config::default();
        assert_eq!(c.athena.api.host, "127.0.0.1");
        assert_eq!(c.athena.api.port, 8080);
        assert_eq!(c.core.max_iterations, 50);
        assert_eq!(c.core.iteration_timeout, secs(1800));
        assert!(c.core.evaluation.require_verification);
        assert_eq!(c.scheduler.max_concurrent_tasks, 8);
        assert_eq!(c.scheduler.max_concurrent_instances, 16);
        assert_eq!(c.scheduler.max_instances_per_task, 4);
        assert_eq!(c.scheduler.max_queue_length, 1000);
        assert_eq!(c.scheduler.reserved_core_agent_slots, 2);
        assert!((c.scheduler.max_waiting_approval_ratio - 0.25).abs() < f64::EPSILON);
        assert_eq!(c.scheduler.stall_detection_factor, 2);
        assert_eq!(c.messaging.max_queue_depth, 1000);
        assert_eq!(c.timeout.startup, secs(30));
        assert_eq!(c.timeout.execution, secs(1800));
        assert_eq!(c.timeout.message_reply, secs(2700));
        assert_eq!(c.timeout.approval, secs(3600));
        assert_eq!(c.timeout.stop, secs(60));
        assert_eq!(c.timeout.clarification, secs(86_400));
        assert_eq!(c.memory.short_term_max_tokens, 32_000);
        assert_eq!(c.memory.working_max_records, 500);
        assert_eq!(c.memory.long_term.max_records, 10_000);
        assert_eq!(c.events.retention_days, 90);
        assert_eq!(c.retry.max_attempts, 3);
        assert_eq!(c.retry.backoff.initial_ms, 1000);
        assert_eq!(c.retry.backoff.max_ms, 60_000);
        assert_eq!(c.core_agent_restart.max_attempts, 3);
        assert_eq!(c.core_agent_restart.heartbeat_interval, secs(20));
        assert_eq!(c.core_agent_restart.heartbeat_timeout, secs(60));
        assert_eq!(c.policy.max_approval_ttl, secs(86_400));
    }

    #[test]
    fn provider_and_model_have_no_default() {
        // ATH-AGENT-008: ATHENA must not carry a provider or model default, so that cost and
        // quality stay judgeable from the definition alone. Validation rejects the empty value.
        let c = Config::default();
        assert!(c.core.agent.provider.is_empty());
        assert!(c.core.agent.model.is_empty());
    }

    #[test]
    fn core_agent_default_tools_are_minimal_and_exclude_the_dangerous_ones() {
        // ATH-SEC-025.
        let c = Config::default();
        assert_eq!(
            c.core.agent.allowed_tools,
            vec![ToolName::FsRead, ToolName::FsWrite, ToolName::SendTo]
        );
        for forbidden in [ToolName::Shell, ToolName::Process, ToolName::Network] {
            assert!(
                !c.core.agent.allowed_tools.contains(&forbidden),
                "{forbidden} must not be a default Core Agent tool"
            );
        }
    }

    #[test]
    fn delegable_tools_and_skill_allow_lists_default_to_empty() {
        // ATH-SEC-024 and ATH-SKILL-035: deny by default, opt in explicitly.
        let c = Config::default();
        assert!(c.core.agent.delegable_tools.is_empty());
        assert!(c.skills.allowed_publishers.is_empty());
        assert!(c.skills.allowed_sources.is_empty());
    }

    #[test]
    fn skill_source_and_registry_directories_differ() {
        // ATH-SKILL-038: source and registered must not be the same directory.
        let c = Config::default();
        assert_ne!(c.skills.local_dir, c.skills.dir);
    }

    #[test]
    fn default_policy_rules_are_in_specification_order() {
        // ATH-CFG-009 makes order significant: first match wins, so the two high-risk shell
        // patterns must precede the general shell rule.
        let c = Config::default();
        assert_eq!(c.policy.rules.len(), 6);
        assert_eq!(c.policy.rules[0].risk, RiskLevel::High);
        assert_eq!(
            c.policy.rules[0].matcher.args_pattern.as_deref(),
            Some(r"^git\s+push")
        );
        assert_eq!(c.policy.rules[4].risk, RiskLevel::Medium);
        assert_eq!(c.policy.rules[4].matcher.args_pattern, None);
        assert_eq!(c.policy.rules[5].risk, RiskLevel::Low);
    }

    #[test]
    fn durations_parse_from_the_specification_spellings() {
        let yaml = r"
timeout:
  startup: 30s
  execution: 30m
  message_reply: 45m
  approval: 1h
  stop: 60s
  clarification: 24h
messaging:
  dedup_retention: 7d
";
        let c: Config = serde_yaml::from_str(yaml).expect("parse");
        assert_eq!(c.timeout.startup, secs(30));
        assert_eq!(c.timeout.execution, secs(1800));
        assert_eq!(c.timeout.message_reply, secs(2700));
        assert_eq!(c.timeout.approval, secs(3600));
        assert_eq!(c.timeout.clarification, secs(86_400));
        assert_eq!(c.messaging.dedup_retention, secs(604_800));
    }

    #[test]
    fn an_unknown_key_is_rejected_rather_than_ignored() {
        // ATH-CFG-003: a typo must abort startup, not silently leave a default in place.
        let err = serde_yaml::from_str::<Config>("scheduler:\n  max_concurent_tasks: 4\n")
            .expect_err("typo must be refused");
        assert!(
            err.to_string().contains("max_concurent_tasks")
                || err.to_string().contains("unknown field"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn an_unknown_top_level_section_is_rejected() {
        let err = serde_yaml::from_str::<Config>("scheduling:\n  foo: 1\n")
            .expect_err("unknown section must be refused");
        assert!(err.to_string().contains("unknown field"), "{err}");
    }

    #[test]
    fn a_partial_file_keeps_defaults_for_unmentioned_keys() {
        // The defaults layer (ATH-CFG-001) still applies to keys the file omits; what must not
        // happen is defaulting around an *invalid* value.
        let c: Config =
            serde_yaml::from_str("scheduler:\n  max_concurrent_tasks: 3\n").expect("parse");
        assert_eq!(c.scheduler.max_concurrent_tasks, 3);
        assert_eq!(c.scheduler.max_concurrent_instances, 16);
        assert_eq!(c.timeout.startup, secs(30));
    }

    #[test]
    fn runtime_entries_are_an_open_map_so_adding_a_runtime_needs_no_core_change() {
        // ATH-RT-008.
        let yaml = r#"
runtime:
  default: agent-cli
  agent-cli:
    max_instances: 16
    command: agent-cli
  docker:
    max_instances: 4
    command: docker
"#;
        let c: Config = serde_yaml::from_str(yaml).expect("parse");
        assert_eq!(c.runtime.entries.len(), 2);
        assert_eq!(c.runtime.entries["docker"].max_instances, 4);
    }

    #[test]
    fn per_runtime_timeout_overrides_are_optional() {
        let yaml = r"
runtime:
  agent-cli:
    startup_timeout: 45s
    execution_timeout: 20m
";
        let c: Config = serde_yaml::from_str(yaml).expect("parse");
        let entry = &c.runtime.entries["agent-cli"];
        assert_eq!(entry.startup_timeout, Some(secs(45)));
        assert_eq!(entry.execution_timeout, Some(secs(1200)));
        assert_eq!(
            Config::default().runtime.entries["agent-cli"].startup_timeout,
            None
        );
    }

    #[test]
    fn config_round_trips_through_yaml() {
        let original = Config::default();
        let yaml = serde_yaml::to_string(&original).expect("serialize");
        let back: Config = serde_yaml::from_str(&yaml).expect("deserialize");
        assert_eq!(back, original);
    }

    #[test]
    fn on_exhausted_admits_only_escalation() {
        // ATH-CORE-030: Core Host must not take over deciding.
        assert!(serde_yaml::from_str::<OnExhausted>("escalate").is_ok());
        assert!(serde_yaml::from_str::<OnExhausted>("continue").is_err());
    }

    #[test]
    fn approval_timeout_admits_only_rejection() {
        // ATH-HITL-005: silence is never consent.
        assert!(serde_yaml::from_str::<OnApprovalTimeout>("reject").is_ok());
        assert!(serde_yaml::from_str::<OnApprovalTimeout>("approve").is_err());
    }
}
