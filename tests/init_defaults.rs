//! Drift test for `athena init`'s default `config.yaml` (tasks P3-07; ATH-CLI-009).
//!
//! The CLI crate generates `.athena/config.yaml` but, by the section-32.2 dependency graph,
//! must not depend on `athena-config`. Nothing stops the embedded default and the schema from
//! drifting apart, so this conformance test holds the pairing: the generated file must parse
//! through the real schema and validate except for the two values the operator must supply
//! (`core.agent.provider` / `core.agent.model` — ATH-AGENT-008 forbids an ATHENA-side
//! default, so a default file cannot be a fully valid startup configuration by design).

use athena_cli::template;
use athena_config::Config;
use athena_types::ToolName;

#[test]
fn generated_default_config_parses_through_the_real_schema() {
    let parsed: Config = serde_yaml::from_str(template::DEFAULT_CONFIG_YAML)
        .expect("the generated config.yaml must be schema-valid YAML (deny_unknown_fields holds)");
    // A spot check against the section-28 defaults the file claims to carry.
    assert_eq!(parsed.athena.api.port, 8080);
    assert_eq!(parsed.scheduler.max_concurrent_instances, 16);
    assert_eq!(parsed.events.retention_days, 90);
    assert_eq!(parsed.core.agent.allowed_tools.len(), 3);
    assert!(parsed.core.agent.delegable_tools.is_empty());
    assert_eq!(parsed.policy.rules.len(), 6);
}

#[test]
fn generated_default_config_passes_validation_once_operator_values_are_set() {
    let mut parsed: Config = serde_yaml::from_str(template::DEFAULT_CONFIG_YAML)
        .expect("parse the generated config.yaml");
    // The values the operator must supply before the first start. ATH-AGENT-008 forbids an
    // ATHENA-side provider/model default, and ATH-SEC-024 requires the permission ceiling to
    // be stated explicitly, so a default file cannot be a fully valid startup configuration
    // by design — the empty values are the fail-fast reminders.
    parsed.core.agent.provider = "claude".into();
    parsed.core.agent.model = "claude-opus-4-7".into();
    parsed.core.agent.delegable_tools = vec![ToolName::FsRead, ToolName::FsWrite, ToolName::Shell];
    athena_config::validate(&parsed)
        .expect("the default file must validate apart from the operator-supplied values");

    // With them empty, validation refuses on the coordination constraints alone.
    let plain: Config = serde_yaml::from_str(template::DEFAULT_CONFIG_YAML).expect("parse");
    let error = athena_config::validate(&plain)
        .expect_err("the unset provider must fail validation")
        .to_string();
    assert!(
        error.contains("provider"),
        "the first failure must name the provider constraint, got: {error}"
    );
}

#[test]
fn generated_default_config_carries_only_specification_sections() {
    // Section set per spec section 28.1: exactly the schema's top-level keys.
    let value: serde_yaml::Value = serde_yaml::from_str(template::DEFAULT_CONFIG_YAML)
        .expect("parse the generated config.yaml");
    let mapping = value.as_mapping().expect("a top-level mapping");
    let mut keys: Vec<String> = mapping
        .keys()
        .filter_map(|key| key.as_str().map(str::to_string))
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "agents",
            "approval",
            "athena",
            "core",
            "core_agent_restart",
            "events",
            "memory",
            "messaging",
            "observability",
            "policy",
            "retry",
            "runtime",
            "scheduler",
            "skills",
            "timeout",
        ]
    );
}
