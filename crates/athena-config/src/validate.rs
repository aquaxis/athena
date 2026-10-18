//! Fail-fast configuration validation (ATH-CFG-003, ATH-CFG-006, ATH-CFG-008).
//!
//! Validation runs after `${athena.home}` expansion, so constraints compare real values. Every
//! failure aborts startup: continuing on a partially sensible configuration is forbidden,
//! because the operator would then be running something they did not describe.

use crate::error::{ConfigError, Result};
use crate::schema::{AuthMode, Config};
use athena_types::ToolName;
use std::time::Duration;

/// Tools the Core Agent must never hold in its own `allowed_tools` (ATH-SEC-025).
const CORE_AGENT_FORBIDDEN_TOOLS: &[ToolName] =
    &[ToolName::Shell, ToolName::Process, ToolName::Network];

/// The scope every single-operation approval uses (spec section 15.3).
const OPERATION_ONLY_SCOPE: &str = "operation_only";

/// Validate the resolved configuration.
pub fn validate(config: &Config) -> Result<()> {
    let fail =
        |constraint: String| -> ConfigError { ConfigError::ConstraintViolated { constraint } };

    // --- Spec section 28.2 cross-constraints (ATH-CFG-008) ---

    let runtime_capacity: u64 = config
        .runtime
        .entries
        .values()
        .map(|entry| entry.max_instances)
        .sum();
    if runtime_capacity < config.scheduler.max_concurrent_instances {
        return Err(fail(format!(
            "sum of runtime.*.max_instances ({runtime_capacity}) must be at least \
             scheduler.max_concurrent_instances ({}); otherwise the Scheduler would admit \
             instances no Runtime can host",
            config.scheduler.max_concurrent_instances
        )));
    }

    let startup_plus_execution = config.timeout.startup + config.timeout.execution;
    if config.timeout.message_reply <= startup_plus_execution {
        return Err(fail(format!(
            "timeout.message_reply ({:?}) must exceed timeout.startup + timeout.execution \
             ({startup_plus_execution:?}); a shorter reply deadline would time out a delegation \
             that is still legitimately starting or running",
            config.timeout.message_reply
        )));
    }

    if config.timeout.stop.is_zero() {
        return Err(fail(
            "timeout.stop must be greater than zero; a zero stop budget would force-kill every \
             instance immediately and leave side effects undetermined"
                .into(),
        ));
    }

    if config.policy.max_approval_ttl < config.timeout.approval {
        return Err(fail(format!(
            "policy.max_approval_ttl ({:?}) must be at least timeout.approval ({:?}); otherwise \
             an approval could expire by scope before its response deadline arrives",
            config.policy.max_approval_ttl, config.timeout.approval
        )));
    }

    if config.scheduler.reserved_core_agent_slots < 1 {
        return Err(fail(
            "scheduler.reserved_core_agent_slots must be at least 1; without a reserved slot the \
             Core Agent could be unable to start and ATHENA would have no decision-maker"
                .into(),
        ));
    }

    // The same reply-deadline inequality must hold against any per-Runtime override.
    for (name, entry) in &config.runtime.entries {
        let startup = entry.startup_timeout.unwrap_or(config.timeout.startup);
        let execution = entry.execution_timeout.unwrap_or(config.timeout.execution);
        let sum = startup + execution;
        if config.timeout.message_reply <= sum {
            return Err(fail(format!(
                "timeout.message_reply ({:?}) must exceed runtime.{name}.startup_timeout + \
                 runtime.{name}.execution_timeout ({sum:?})",
                config.timeout.message_reply
            )));
        }
    }

    // --- Authentication (ATH-CFG-006) ---

    if config.athena.api.auth.mode == AuthMode::None && config.athena.api.host != "127.0.0.1" {
        return Err(fail(format!(
            "athena.api.auth.mode: none is permitted only when athena.api.host is 127.0.0.1, \
             not {}; unauthenticated access must never be reachable off-host",
            config.athena.api.host
        )));
    }

    // --- Core Agent definition ---

    // ATH-AGENT-008: no ATHENA-side default exists, so an unset value is a validation error
    // rather than something to fill in.
    if config.core.agent.provider.trim().is_empty() {
        return Err(fail(
            "core.agent.provider must be set explicitly; ATHENA holds no provider default so \
             that cost and quality stay judgeable from configuration alone"
                .into(),
        ));
    }
    if config.core.agent.model.trim().is_empty() {
        return Err(fail(
            "core.agent.model must be set explicitly; ATHENA holds no model default".into(),
        ));
    }

    for forbidden in CORE_AGENT_FORBIDDEN_TOOLS {
        if config.core.agent.allowed_tools.contains(forbidden) {
            return Err(fail(format!(
                "core.agent.allowed_tools must not contain {forbidden}; the Core Agent is \
                 long-lived and externally reachable, so it keeps minimal execution rights and \
                 grants this through delegable_tools instead"
            )));
        }
    }

    // ATH-SEC-009: a subordinate's allowed_tools must be a subset of the parent's
    // delegable_tools. With an empty ceiling no subordinate could run any tool, so the
    // hierarchy could never do work.
    if config.core.agent.delegable_tools.is_empty() {
        return Err(fail(
            "core.agent.delegable_tools must be stated explicitly and non-empty; it is ATHENA's \
             overall permission ceiling, and an empty set leaves every subordinate unable to act"
                .into(),
        ));
    }

    // --- Runtime selection ---

    if !config.runtime.entries.contains_key(&config.runtime.default) {
        let known: Vec<&str> = config.runtime.entries.keys().map(String::as_str).collect();
        return Err(fail(format!(
            "runtime.default names {:?}, which has no settings block; known runtimes: {known:?}",
            config.runtime.default
        )));
    }

    // --- Skills ---

    // ATH-SKILL-038: source and registered must be distinct directories.
    if config.skills.local_dir == config.skills.dir {
        return Err(fail(format!(
            "skills.local_dir and skills.dir must differ (both are {:?}); the former holds \
             Package sources and the latter registered skills, and only registered skills are \
             selection candidates",
            config.skills.dir
        )));
    }

    // --- Approval scopes (ATH-HITL-012) ---

    if !config
        .policy
        .approval_scopes
        .iter()
        .any(|s| s == OPERATION_ONLY_SCOPE)
    {
        return Err(fail(format!(
            "policy.approval_scopes must include {OPERATION_ONLY_SCOPE:?}; every single-operation \
             approval uses it, and an agent may not invent a scope pattern"
        )));
    }

    // --- Ratios, counts and intervals ---

    let ratio = config.scheduler.max_waiting_approval_ratio;
    if !(ratio.is_finite() && ratio > 0.0 && ratio <= 1.0) {
        return Err(fail(format!(
            "scheduler.max_waiting_approval_ratio must be within (0, 1], not {ratio}"
        )));
    }

    if config.scheduler.max_queue_length == 0 {
        return Err(fail(
            "scheduler.max_queue_length must be greater than zero".into(),
        ));
    }

    if config.scheduler.max_concurrent_tasks == 0 || config.scheduler.max_concurrent_instances == 0
    {
        return Err(fail(
            "scheduler.max_concurrent_tasks and max_concurrent_instances must be greater than \
             zero"
                .into(),
        ));
    }

    if config.scheduler.stall_detection_factor == 0 {
        return Err(fail(
            "scheduler.stall_detection_factor must be greater than zero; zero would report every \
             non-terminal Task as stalled"
                .into(),
        ));
    }

    if config.core.max_iterations == 0 {
        return Err(fail("core.max_iterations must be greater than zero".into()));
    }

    if config.events.retention_days == 0 {
        return Err(fail(
            "events.retention_days must be greater than zero; Events are the only record from \
             which a Task's progress can be reconstructed"
                .into(),
        ));
    }

    if config.retry.max_attempts == 0 {
        return Err(fail("retry.max_attempts must be greater than zero".into()));
    }

    if config.retry.backoff.multiplier < 1.0 || !config.retry.backoff.multiplier.is_finite() {
        return Err(fail(format!(
            "retry.backoff.multiplier must be at least 1.0, not {}; a shrinking backoff would \
             retry faster after each failure",
            config.retry.backoff.multiplier
        )));
    }

    if config.retry.backoff.max_ms < config.retry.backoff.initial_ms {
        return Err(fail(format!(
            "retry.backoff.max_ms ({}) must be at least initial_ms ({})",
            config.retry.backoff.max_ms, config.retry.backoff.initial_ms
        )));
    }

    if config.core_agent_restart.max_attempts == 0 {
        return Err(fail(
            "core_agent_restart.max_attempts must be greater than zero".into(),
        ));
    }

    // Not a spec-stated inequality, but an incoherent pairing: a lease shorter than the
    // emission interval would make every instance self-terminate between heartbeats
    // (ATH-CORE-047).
    if config.core_agent_restart.heartbeat_timeout <= config.core_agent_restart.heartbeat_interval {
        return Err(fail(format!(
            "core_agent_restart.heartbeat_timeout ({:?}) must exceed heartbeat_interval ({:?}); \
             otherwise the lease lapses between two heartbeats and every instance self-terminates",
            config.core_agent_restart.heartbeat_timeout,
            config.core_agent_restart.heartbeat_interval
        )));
    }

    // --- Memory ---

    // ATH-MEM-009: implicit reliance on a provider default is forbidden.
    if config.memory.tokenizer.trim().is_empty() {
        return Err(fail(
            "memory.tokenizer must be set; token counting must not depend implicitly on a \
             provider default"
                .into(),
        ));
    }
    // ATH-MEM-008: summarizing is inference, so Core Host delegates it and needs a target.
    if config.memory.summarizer.trim().is_empty() {
        return Err(fail(
            "memory.summarizer must name an Agent definition; Core Host delegates summarization \
             rather than performing it"
                .into(),
        ));
    }
    if config.memory.short_term_max_tokens == 0 || config.memory.working_max_records == 0 {
        return Err(fail(
            "memory.short_term_max_tokens and memory.working_max_records must be greater than \
             zero"
                .into(),
        ));
    }

    Ok(())
}

/// Convenience: the effective startup and execution budgets for a named Runtime
/// (ATH-CFG-008 per-Runtime overrides).
#[must_use]
pub fn effective_timeouts(config: &Config, runtime: &str) -> (Duration, Duration) {
    match config.runtime.entries.get(runtime) {
        Some(entry) => (
            entry.startup_timeout.unwrap_or(config.timeout.startup),
            entry.execution_timeout.unwrap_or(config.timeout.execution),
        ),
        None => (config.timeout.startup, config.timeout.execution),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::RuntimeEntry;

    /// A configuration that passes validation, as the baseline for single-field mutation.
    fn valid() -> Config {
        let mut c = Config::default();
        // The defaults deliberately leave these unset (ATH-AGENT-008, ATH-SEC-024).
        c.core.agent.provider = "claude".into();
        c.core.agent.model = "claude-opus-5".into();
        c.core.agent.delegable_tools = vec![ToolName::FsRead, ToolName::FsWrite, ToolName::Shell];
        c
    }

    #[test]
    fn the_baseline_configuration_validates() {
        validate(&valid()).expect("baseline must be valid");
    }

    #[test]
    fn stock_defaults_do_not_validate_because_provider_is_unset() {
        // ATH-AGENT-008: ATHENA must not supply a provider, so a bare default config is
        // incomplete by design rather than runnable.
        let err = validate(&Config::default()).expect_err("must be refused");
        assert!(err.to_string().contains("core.agent.provider"), "{err}");
    }

    #[test]
    fn runtime_capacity_must_cover_the_scheduler_ceiling() {
        let mut c = valid();
        c.scheduler.max_concurrent_instances = 32;
        c.runtime.entries.insert(
            "agent-cli".into(),
            RuntimeEntry {
                max_instances: 16,
                ..RuntimeEntry::default()
            },
        );
        let err = validate(&c).expect_err("must be refused");
        assert!(err.to_string().contains("max_instances"), "{err}");

        // Raising Runtime capacity, or adding a second Runtime, satisfies it.
        c.runtime.entries.insert(
            "local-process".into(),
            RuntimeEntry {
                max_instances: 16,
                command: "athena-local".into(),
                ..RuntimeEntry::default()
            },
        );
        validate(&c).expect("two runtimes totalling 32 must satisfy the ceiling");
    }

    #[test]
    fn reply_deadline_must_exceed_startup_plus_execution() {
        let mut c = valid();
        c.timeout.startup = Duration::from_secs(30);
        c.timeout.execution = Duration::from_secs(1800);
        c.timeout.message_reply = Duration::from_secs(1800);
        let err = validate(&c).expect_err("must be refused");
        assert!(err.to_string().contains("message_reply"), "{err}");

        c.timeout.message_reply = Duration::from_secs(1831);
        validate(&c).expect("just above the sum is acceptable");
    }

    #[test]
    fn reply_deadline_is_checked_against_per_runtime_overrides_too() {
        // ATH-CFG-008's second inequality: a Runtime override can breach it even when the
        // global values are fine.
        let mut c = valid();
        c.runtime.entries.insert(
            "agent-cli".into(),
            RuntimeEntry {
                max_instances: 16,
                command: "agent-cli".into(),
                startup_timeout: Some(Duration::from_secs(60)),
                execution_timeout: Some(Duration::from_secs(3600)),
            },
        );
        let err = validate(&c).expect_err("must be refused");
        assert!(err.to_string().contains("runtime.agent-cli"), "{err}");
    }

    #[test]
    fn stop_timeout_must_be_positive() {
        let mut c = valid();
        c.timeout.stop = Duration::ZERO;
        let err = validate(&c).expect_err("must be refused");
        assert!(err.to_string().contains("timeout.stop"), "{err}");
    }

    #[test]
    fn approval_ttl_must_cover_the_approval_deadline() {
        let mut c = valid();
        c.timeout.approval = Duration::from_secs(7200);
        c.policy.max_approval_ttl = Duration::from_secs(3600);
        let err = validate(&c).expect_err("must be refused");
        assert!(err.to_string().contains("max_approval_ttl"), "{err}");
    }

    #[test]
    fn a_core_agent_slot_must_always_be_reserved() {
        let mut c = valid();
        c.scheduler.reserved_core_agent_slots = 0;
        let err = validate(&c).expect_err("must be refused");
        assert!(
            err.to_string().contains("reserved_core_agent_slots"),
            "{err}"
        );
    }

    #[test]
    fn unauthenticated_access_is_confined_to_loopback() {
        // ATH-CFG-006.
        let mut c = valid();
        c.athena.api.auth.mode = AuthMode::None;
        c.athena.api.host = "0.0.0.0".into();
        let err = validate(&c).expect_err("must be refused");
        assert!(err.to_string().contains("127.0.0.1"), "{err}");

        c.athena.api.host = "127.0.0.1".into();
        validate(&c).expect("loopback with auth none is permitted");

        // Token mode is unconstrained by host.
        c.athena.api.auth.mode = AuthMode::Token;
        c.athena.api.host = "0.0.0.0".into();
        validate(&c).expect("token mode may bind anywhere");
    }

    #[test]
    fn the_core_agent_may_not_hold_dangerous_tools_itself() {
        // ATH-SEC-025: these are granted downward, not held.
        for tool in CORE_AGENT_FORBIDDEN_TOOLS {
            let mut c = valid();
            c.core.agent.allowed_tools.push(*tool);
            let err = validate(&c).expect_err("must be refused");
            assert!(err.to_string().contains(tool.as_str()), "{tool}: {err}");
        }
    }

    #[test]
    fn the_permission_ceiling_must_be_stated() {
        // ATH-SEC-023 / ATH-SEC-024: explicit, and non-empty or nothing can be delegated.
        let mut c = valid();
        c.core.agent.delegable_tools.clear();
        let err = validate(&c).expect_err("must be refused");
        assert!(err.to_string().contains("delegable_tools"), "{err}");
    }

    #[test]
    fn the_default_runtime_must_have_a_settings_block() {
        // This is what recovers the typo protection that `flatten` costs the runtime section.
        let mut c = valid();
        c.runtime.default = "agnet-cli".into();
        let err = validate(&c).expect_err("must be refused");
        assert!(err.to_string().contains("runtime.default"), "{err}");
    }

    #[test]
    fn skill_source_and_registry_directories_must_differ() {
        // ATH-SKILL-038.
        let mut c = valid();
        c.skills.dir = c.skills.local_dir.clone();
        let err = validate(&c).expect_err("must be refused");
        assert!(err.to_string().contains("skills.local_dir"), "{err}");
    }

    #[test]
    fn operation_only_must_remain_a_selectable_scope() {
        // ATH-HITL-012: scopes come from this enumeration, and single-use approvals need it.
        let mut c = valid();
        c.policy.approval_scopes = vec!["tool:shell:same_argv".into()];
        let err = validate(&c).expect_err("must be refused");
        assert!(err.to_string().contains("operation_only"), "{err}");
    }

    #[test]
    fn waiting_approval_ratio_must_be_a_proper_fraction() {
        for bad in [0.0, -0.1, 1.5, f64::NAN, f64::INFINITY] {
            let mut c = valid();
            c.scheduler.max_waiting_approval_ratio = bad;
            let err = validate(&c).expect_err("must be refused");
            assert!(
                err.to_string().contains("max_waiting_approval_ratio"),
                "{bad}: {err}"
            );
        }
        let mut c = valid();
        c.scheduler.max_waiting_approval_ratio = 1.0;
        validate(&c).expect("a ratio of 1.0 is permitted");
    }

    #[test]
    fn zero_valued_limits_are_refused() {
        /// A labelled mutation that should make validation fail.
        type Case = (&'static str, Box<dyn Fn(&mut Config)>);
        let cases: Vec<Case> = vec![
            (
                "max_queue_length",
                Box::new(|c: &mut Config| c.scheduler.max_queue_length = 0),
            ),
            (
                "max_concurrent_tasks",
                Box::new(|c: &mut Config| c.scheduler.max_concurrent_tasks = 0),
            ),
            (
                "stall_detection_factor",
                Box::new(|c: &mut Config| c.scheduler.stall_detection_factor = 0),
            ),
            (
                "max_iterations",
                Box::new(|c: &mut Config| c.core.max_iterations = 0),
            ),
            (
                "retention_days",
                Box::new(|c: &mut Config| c.events.retention_days = 0),
            ),
            (
                "retry.max_attempts",
                Box::new(|c: &mut Config| c.retry.max_attempts = 0),
            ),
            (
                "core_agent_restart.max_attempts",
                Box::new(|c: &mut Config| c.core_agent_restart.max_attempts = 0),
            ),
            (
                "short_term_max_tokens",
                Box::new(|c: &mut Config| c.memory.short_term_max_tokens = 0),
            ),
        ];
        for (label, mutate) in cases {
            let mut c = valid();
            mutate(&mut c);
            validate(&c).expect_err(label);
        }
    }

    #[test]
    fn backoff_must_not_shrink() {
        let mut c = valid();
        c.retry.backoff.multiplier = 0.5;
        let err = validate(&c).expect_err("must be refused");
        assert!(err.to_string().contains("multiplier"), "{err}");

        let mut c = valid();
        c.retry.backoff.initial_ms = 10_000;
        c.retry.backoff.max_ms = 1_000;
        let err = validate(&c).expect_err("must be refused");
        assert!(err.to_string().contains("max_ms"), "{err}");
    }

    #[test]
    fn a_heartbeat_lease_shorter_than_its_interval_is_refused() {
        // ATH-CORE-047: otherwise every instance self-terminates between heartbeats.
        let mut c = valid();
        c.core_agent_restart.heartbeat_interval = Duration::from_secs(60);
        c.core_agent_restart.heartbeat_timeout = Duration::from_secs(20);
        let err = validate(&c).expect_err("must be refused");
        assert!(err.to_string().contains("heartbeat_timeout"), "{err}");
    }

    #[test]
    fn memory_tokenizer_and_summarizer_must_be_named() {
        // ATH-MEM-008 and ATH-MEM-009.
        let mut c = valid();
        c.memory.tokenizer = "  ".into();
        assert!(validate(&c).is_err());

        let mut c = valid();
        c.memory.summarizer = String::new();
        let err = validate(&c).expect_err("must be refused");
        assert!(err.to_string().contains("summarizer"), "{err}");
    }

    #[test]
    fn effective_timeouts_prefer_the_runtime_override() {
        let mut c = valid();
        c.runtime.entries.insert(
            "agent-cli".into(),
            RuntimeEntry {
                max_instances: 16,
                command: "agent-cli".into(),
                startup_timeout: Some(Duration::from_secs(45)),
                execution_timeout: None,
            },
        );
        let (startup, execution) = effective_timeouts(&c, "agent-cli");
        assert_eq!(startup, Duration::from_secs(45));
        assert_eq!(execution, c.timeout.execution);

        // An unknown Runtime falls back to the global values rather than failing.
        let (startup, execution) = effective_timeouts(&c, "nonexistent");
        assert_eq!(startup, c.timeout.startup);
        assert_eq!(execution, c.timeout.execution);
    }
}
