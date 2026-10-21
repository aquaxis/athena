//! Layered configuration resolution (ATH-CFG-001, ATH-CFG-002, ATH-CFG-010).
//!
//! Resolution order is defaults, then file, then environment, then CLI. Layering happens on a
//! YAML value tree rather than on the typed struct, for two reasons: an override must be able
//! to address a single leaf without reconstructing its parents, and `${athena.home}` has to be
//! expanded *before* the typed schema is validated (ATH-CFG-010).

use crate::error::{ConfigError, Result};
use crate::home::AthenaHome;
use crate::schema::Config;
use serde_yaml::{Mapping, Value};
use std::path::Path;

/// The environment-variable prefix (ATH-CFG-002).
pub const ENV_PREFIX: &str = "ATHENA_";

/// The only permitted variable reference (ATH-CFG-010).
const HOME_VAR: &str = "${athena.home}";

/// One override, carrying where it came from so errors can name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Override {
    /// How the user expressed it, e.g. `ATHENA_API_PORT` or `--set athena.api.port`.
    pub origin: String,
    /// Dotted key path, already resolved.
    pub path: Vec<String>,
    /// Raw value.
    pub value: String,
}

/// Inputs to resolution.
#[derive(Debug, Clone, Default)]
pub struct ResolveInputs {
    /// Raw YAML of the configuration file, when one exists.
    pub file_yaml: Option<String>,
    /// Path of that file, for error messages.
    pub file_path: Option<std::path::PathBuf>,
    /// `ATHENA_*` variables as `(name, value)`.
    pub env: Vec<(String, String)>,
    /// CLI overrides as dotted `path=value` strings.
    pub cli: Vec<String>,
}

/// Key fragments that indicate a secret has been written into the file (ATH-CFG-005).
const SECRET_HINTS: &[&str] = &[
    "api_key",
    "apikey",
    "token",
    "secret",
    "password",
    "passwd",
    "private_key",
];

/// Resolve the effective configuration.
///
/// Returns the typed configuration together with the expanded value tree, the latter being what
/// `ConfigurationLoaded` reports after redaction (ATH-CFG-004).
pub fn resolve(home: &AthenaHome, inputs: &ResolveInputs) -> Result<(Config, Value)> {
    // Layer 1: defaults. Serializing the default struct guarantees the tree has every key,
    // which is what lets override paths be matched against real keys rather than guessed.
    let mut tree = serde_yaml::to_value(Config::default())
        .map_err(|source| ConfigError::Invalid { path: None, source })?;

    // Layer 2: the file.
    if let Some(yaml) = &inputs.file_yaml {
        let from_file: Value =
            serde_yaml::from_str(yaml).map_err(|source| ConfigError::Invalid {
                path: inputs.file_path.clone(),
                source,
            })?;
        reject_secrets(&from_file, &mut Vec::new())?;
        if !from_file.is_null() {
            merge(&mut tree, from_file);
        }
    }

    // Layer 3: the environment, then layer 4: the CLI. CLI is applied last so it wins.
    for over in collect_env_overrides(&tree, &inputs.env)? {
        apply_override(&mut tree, &over)?;
    }
    for over in collect_cli_overrides(&inputs.cli)? {
        apply_override(&mut tree, &over)?;
    }

    // `${athena.home}` is expanded before validation, so constraint checks and path handling
    // see real paths (ATH-CFG-010).
    expand_home_var(&mut tree, home, &mut Vec::new())?;

    let config: Config =
        serde_yaml::from_value(tree.clone()).map_err(|source| ConfigError::Invalid {
            path: inputs.file_path.clone(),
            source,
        })?;

    Ok((config, tree))
}

/// Read `ATHENA_*` variables from the real environment.
#[must_use]
pub fn env_from_process() -> Vec<(String, String)> {
    std::env::vars()
        .filter(|(k, _)| k.starts_with(ENV_PREFIX))
        .collect()
}

/// Deep-merge `src` into `dst`, with `src` winning.
///
/// Mappings merge key by key; every other value replaces wholesale. Sequences replace rather
/// than append, because appending would make it impossible for a later layer to *remove* an
/// entry from an allow-list.
fn merge(dst: &mut Value, src: Value) {
    match (dst, src) {
        (Value::Mapping(dst_map), Value::Mapping(src_map)) => {
            for (key, src_val) in src_map {
                match dst_map.get_mut(&key) {
                    Some(dst_val) => merge(dst_val, src_val),
                    None => {
                        dst_map.insert(key, src_val);
                    }
                }
            }
        }
        (dst_slot, src_val) => *dst_slot = src_val,
    }
}

/// Refuse a configuration file that contains something secret-shaped (ATH-CFG-005).
///
/// `config.yaml` may be version-controlled (ATH-DEV-020), so a token written here would be
/// committed. `deny_unknown_fields` would also reject these keys, but with a message about an
/// unknown field rather than about secrets.
///
/// The match is substring-based, so counting-rule keys that legitimately carry the word
/// are carved out first: `memory.tokenizer` (the named counting rule, ATH-MEM-009) and
/// `…max_tokens` (memory ceilings) are not secrets. Discovered by the first resident
/// boot (2026-10-18): the real startup aborted on the schema's own default key.
fn reject_secrets(value: &Value, path: &mut Vec<String>) -> Result<()> {
    if let Value::Mapping(map) = value {
        for (key, val) in map {
            let key_str = key.as_str().unwrap_or_default().to_ascii_lowercase();
            let compact = key_str.replace(['-', '.'], "_");
            if is_secret_shaped(&compact) {
                path.push(key_str);
                return Err(ConfigError::SecretInConfig {
                    key: path.join("."),
                });
            }
            path.push(key_str);
            reject_secrets(val, path)?;
            path.pop();
        }
    }
    Ok(())
}

/// The substring check with the counting-rule carve-outs.
fn is_secret_shaped(compact: &str) -> bool {
    if compact == "tokenizer" || compact.ends_with("_tokenizer") || compact.ends_with("max_tokens")
    {
        return false;
    }
    SECRET_HINTS.iter().any(|hint| compact.contains(hint))
}

/// Turn `ATHENA_*` variables into resolved overrides (ATH-CFG-002).
fn collect_env_overrides(tree: &Value, env: &[(String, String)]) -> Result<Vec<Override>> {
    let mut out = Vec::new();
    for (name, value) in env {
        let Some(rest) = name.strip_prefix(ENV_PREFIX) else {
            continue;
        };
        // `--home` and `ATHENA_HOME` are consumed by home resolution, which runs first.
        if rest.eq_ignore_ascii_case("home") {
            continue;
        }
        let segments: Vec<String> = rest.split('_').map(str::to_ascii_lowercase).collect();

        // Match from the root, then retry beneath the `athena` section. The prefix is
        // ambiguous with that section's own name: the specification's example
        // `ATHENA_API_PORT -> athena.api.port` elides it, while
        // `ATHENA_SCHEDULER_MAX_CONCURRENT_TASKS` names a root section directly.
        let path = match_path(tree, &segments)
            .or_else(|| match_path_under(tree, "athena", &segments))
            .ok_or_else(|| ConfigError::UnknownKey {
                origin: name.clone(),
            })?;

        out.push(Override {
            origin: name.clone(),
            path,
            value: value.clone(),
        });
    }
    // Deterministic order, so two variables addressing the same key resolve predictably.
    out.sort_by(|a, b| a.origin.cmp(&b.origin));
    Ok(out)
}

/// Parse `path=value` CLI overrides.
fn collect_cli_overrides(cli: &[String]) -> Result<Vec<Override>> {
    let mut out = Vec::new();
    for raw in cli {
        let (path, value) = raw
            .split_once('=')
            .ok_or_else(|| ConfigError::BadOverride {
                origin: raw.clone(),
                reason: "expected path=value".into(),
            })?;
        if path.is_empty() {
            return Err(ConfigError::BadOverride {
                origin: raw.clone(),
                reason: "empty key path".into(),
            });
        }
        out.push(Override {
            origin: raw.clone(),
            path: path.split('.').map(str::to_string).collect(),
            value: value.to_string(),
        });
    }
    Ok(out)
}

/// Resolve env segments to a key path by greedy longest-match against real keys.
///
/// Greedy matching is what makes `MAX_CONCURRENT_TASKS` resolve to the single key
/// `max_concurrent_tasks` instead of a nested `max.concurrent.tasks`.
fn match_path(node: &Value, segments: &[String]) -> Option<Vec<String>> {
    if segments.is_empty() {
        return Some(Vec::new());
    }
    let Value::Mapping(map) = node else {
        return None;
    };

    // Longest candidate first.
    for take in (1..=segments.len()).rev() {
        let candidate = segments[..take].join("_");
        for (key, child) in map {
            let Some(key_str) = key.as_str() else {
                continue;
            };
            // Compare on a normalized form so `agent-cli` matches `AGENT_CLI`.
            if key_str.replace('-', "_").eq_ignore_ascii_case(&candidate)
                && let Some(mut rest) = match_path(child, &segments[take..])
            {
                let mut path = vec![key_str.to_string()];
                path.append(&mut rest);
                return Some(path);
            }
        }
    }
    None
}

/// Match beneath a named section, prefixing that section onto the result.
fn match_path_under(tree: &Value, section: &str, segments: &[String]) -> Option<Vec<String>> {
    let child = tree.get(section)?;
    let mut rest = match_path(child, segments)?;
    let mut path = vec![section.to_string()];
    path.append(&mut rest);
    Some(path)
}

/// Apply one override, coercing the raw string to the existing leaf's type.
///
/// The existing value's type is authoritative: the defaults layer populated it, so the schema
/// already states what shape this key takes.
fn apply_override(tree: &mut Value, over: &Override) -> Result<()> {
    let existing = path_get(tree, &over.path);
    let coerced = coerce(over, existing.as_ref())?;
    path_set(tree, &over.path, coerced).ok_or_else(|| ConfigError::UnknownKey {
        origin: over.origin.clone(),
    })
}

/// Coerce a raw override string into the leaf's type.
fn coerce(over: &Override, existing: Option<&Value>) -> Result<Value> {
    let bad = |reason: &str| ConfigError::BadOverride {
        origin: over.origin.clone(),
        reason: reason.to_string(),
    };

    Ok(match existing {
        Some(Value::Bool(_)) => Value::Bool(
            over.value
                .parse::<bool>()
                .map_err(|_| bad("expected true or false"))?,
        ),
        Some(Value::Number(n)) if n.is_u64() || n.is_i64() => Value::Number(
            over.value
                .parse::<u64>()
                .map_err(|_| bad("expected a non-negative integer"))?
                .into(),
        ),
        Some(Value::Number(_)) => Value::Number(
            over.value
                .parse::<f64>()
                .map_err(|_| bad("expected a number"))?
                .into(),
        ),
        // Sequences take a comma-separated list, the only workable spelling for an env var.
        Some(Value::Sequence(_)) => Value::Sequence(
            over.value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| Value::String(s.to_string()))
                .collect(),
        ),
        // Strings cover durations too: they stay in their `30s` spelling and are parsed by the
        // typed schema afterwards.
        Some(Value::String(_)) | Some(Value::Null) | None => Value::String(over.value.clone()),
        Some(Value::Mapping(_)) => return Err(bad("cannot replace a section with a scalar")),
        Some(other) => {
            let _ = other;
            Value::String(over.value.clone())
        }
    })
}

/// Read a value by key path.
fn path_get(tree: &Value, path: &[String]) -> Option<Value> {
    let mut node = tree;
    for key in path {
        node = node.get(key.as_str())?;
    }
    Some(node.clone())
}

/// Write a value by key path, creating intermediate mappings as needed.
///
/// Returns `None` when an intermediate node exists but is not a mapping, which means the path
/// does not address a real key.
fn path_set(tree: &mut Value, path: &[String], value: Value) -> Option<()> {
    let (last, parents) = path.split_last()?;
    let mut node = tree;
    for key in parents {
        if !node.is_mapping() {
            return None;
        }
        let map = node.as_mapping_mut()?;
        let entry = Value::String(key.clone());
        if !map.contains_key(&entry) {
            map.insert(entry.clone(), Value::Mapping(Mapping::new()));
        }
        node = map.get_mut(&entry)?;
    }
    node.as_mapping_mut()?
        .insert(Value::String(last.clone()), value);
    Some(())
}

/// Expand `${athena.home}` in every string, rejecting any other variable reference.
fn expand_home_var(value: &mut Value, home: &AthenaHome, path: &mut Vec<String>) -> Result<()> {
    match value {
        Value::String(s) => {
            if s.contains(HOME_VAR) {
                let home_str = home.path.to_string_lossy();
                *s = s.replace(HOME_VAR, &home_str);
            }
            // Any surviving `${...}` is unsupported: a second variable would make resolution
            // order ambiguous (ATH-CFG-010).
            if let Some(start) = s.find("${") {
                let rest = &s[start + 2..];
                let name = rest.split('}').next().unwrap_or(rest).to_string();
                return Err(ConfigError::UnsupportedVariable {
                    name,
                    at: if path.is_empty() {
                        "<root>".into()
                    } else {
                        path.join(".")
                    },
                });
            }
            Ok(())
        }
        Value::Sequence(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                path.push(index.to_string());
                expand_home_var(item, home, path)?;
                path.pop();
            }
            Ok(())
        }
        Value::Mapping(map) => {
            for (key, val) in map.iter_mut() {
                path.push(key.as_str().unwrap_or("?").to_string());
                expand_home_var(val, home, path)?;
                path.pop();
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Produce a snapshot suitable for the `ConfigurationLoaded` Event (ATH-CFG-004).
///
/// The schema holds no secret-bearing key, and `reject_secrets` refuses any attempt to add one,
/// so this is defence in depth rather than the primary control.
#[must_use]
pub fn redacted_snapshot(tree: &Value) -> Value {
    fn walk(value: &Value) -> Value {
        match value {
            Value::Mapping(map) => {
                let mut out = Mapping::new();
                for (key, val) in map {
                    let name = key
                        .as_str()
                        .unwrap_or_default()
                        .to_ascii_lowercase()
                        .replace(['-', '.'], "_");
                    let redact = SECRET_HINTS.iter().any(|hint| name.contains(hint));
                    out.insert(
                        key.clone(),
                        if redact {
                            Value::String("[redacted]".into())
                        } else {
                            walk(val)
                        },
                    );
                }
                Value::Mapping(out)
            }
            Value::Sequence(items) => Value::Sequence(items.iter().map(walk).collect()),
            other => other.clone(),
        }
    }
    walk(tree)
}

/// Load the configuration file, returning `None` when absent.
///
/// A missing file is not an error: the defaults layer stands alone, and `athena init` writes a
/// file with the defaults anyway (ATH-CLI-009).
pub fn read_config_file(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ConfigError::Unreadable {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::home::HomeOrigin;
    use std::path::PathBuf;
    use std::time::Duration;

    fn home() -> AthenaHome {
        AthenaHome {
            path: PathBuf::from("/work/proj/.athena"),
            origin: HomeOrigin::UpwardSearch,
        }
    }

    fn resolve_with(inputs: ResolveInputs) -> Result<Config> {
        resolve(&home(), &inputs).map(|(c, _)| c)
    }

    #[test]
    fn defaults_alone_resolve_when_nothing_else_is_supplied() {
        let c = resolve_with(ResolveInputs::default()).expect("resolves");
        assert_eq!(c.scheduler.max_concurrent_tasks, 8);
        assert_eq!(c.athena.api.port, 8080);
    }

    #[test]
    fn the_file_layer_overrides_defaults() {
        let c = resolve_with(ResolveInputs {
            file_yaml: Some("scheduler:\n  max_concurrent_tasks: 3\n".into()),
            ..Default::default()
        })
        .expect("resolves");
        assert_eq!(c.scheduler.max_concurrent_tasks, 3);
        // Unmentioned keys keep their defaults.
        assert_eq!(c.scheduler.max_concurrent_instances, 16);
    }

    #[test]
    fn the_environment_overrides_the_file() {
        // ATH-CFG-001 precedence, and ATH-CFG-002 name mapping.
        let c = resolve_with(ResolveInputs {
            file_yaml: Some("athena:\n  api:\n    port: 9000\n".into()),
            env: vec![("ATHENA_API_PORT".into(), "9999".into())],
            ..Default::default()
        })
        .expect("resolves");
        assert_eq!(c.athena.api.port, 9999);
    }

    #[test]
    fn the_cli_overrides_the_environment() {
        let c = resolve_with(ResolveInputs {
            env: vec![("ATHENA_API_PORT".into(), "9999".into())],
            cli: vec!["athena.api.port=7777".into()],
            ..Default::default()
        })
        .expect("resolves");
        assert_eq!(c.athena.api.port, 7777);
    }

    #[test]
    fn full_precedence_chain_holds_for_one_key() {
        // defaults < file < env < CLI, verified on a single key at each step.
        let base = resolve_with(ResolveInputs::default()).expect("resolves");
        assert_eq!(base.scheduler.max_concurrent_tasks, 8);

        let with_file = resolve_with(ResolveInputs {
            file_yaml: Some("scheduler:\n  max_concurrent_tasks: 2\n".into()),
            ..Default::default()
        })
        .expect("resolves");
        assert_eq!(with_file.scheduler.max_concurrent_tasks, 2);

        let with_env = resolve_with(ResolveInputs {
            file_yaml: Some("scheduler:\n  max_concurrent_tasks: 2\n".into()),
            env: vec![("ATHENA_SCHEDULER_MAX_CONCURRENT_TASKS".into(), "4".into())],
            ..Default::default()
        })
        .expect("resolves");
        assert_eq!(with_env.scheduler.max_concurrent_tasks, 4);

        let with_cli = resolve_with(ResolveInputs {
            file_yaml: Some("scheduler:\n  max_concurrent_tasks: 2\n".into()),
            env: vec![("ATHENA_SCHEDULER_MAX_CONCURRENT_TASKS".into(), "4".into())],
            cli: vec!["scheduler.max_concurrent_tasks=6".into()],
            ..Default::default()
        })
        .expect("resolves");
        assert_eq!(with_cli.scheduler.max_concurrent_tasks, 6);
    }

    #[test]
    fn multi_word_env_keys_resolve_by_greedy_match() {
        // The whole point of greedy matching: MAX_CONCURRENT_INSTANCES is one key, not three.
        let c = resolve_with(ResolveInputs {
            env: vec![(
                "ATHENA_SCHEDULER_MAX_CONCURRENT_INSTANCES".into(),
                "32".into(),
            )],
            ..Default::default()
        })
        .expect("resolves");
        assert_eq!(c.scheduler.max_concurrent_instances, 32);
    }

    #[test]
    fn nested_env_keys_resolve_through_sections() {
        let c = resolve_with(ResolveInputs {
            env: vec![
                ("ATHENA_MEMORY_LONG_TERM_MAX_RECORDS".into(), "5".into()),
                (
                    "ATHENA_CORE_EVALUATION_REQUIRE_VERIFICATION".into(),
                    "false".into(),
                ),
            ],
            ..Default::default()
        })
        .expect("resolves");
        assert_eq!(c.memory.long_term.max_records, 5);
        assert!(!c.core.evaluation.require_verification);
    }

    #[test]
    fn duration_values_survive_as_their_spelling() {
        let c = resolve_with(ResolveInputs {
            env: vec![("ATHENA_TIMEOUT_STARTUP".into(), "45s".into())],
            ..Default::default()
        })
        .expect("resolves");
        assert_eq!(c.timeout.startup, Duration::from_secs(45));
    }

    #[test]
    fn an_unknown_environment_key_is_refused() {
        // Silently ignoring it would leave the operator believing a setting took effect.
        let err = resolve_with(ResolveInputs {
            env: vec![("ATHENA_SCHEDULER_NO_SUCH_KEY".into(), "1".into())],
            ..Default::default()
        })
        .expect_err("must be refused");
        assert!(matches!(err, ConfigError::UnknownKey { .. }), "{err}");
    }

    #[test]
    fn a_mistyped_value_is_refused_rather_than_coerced() {
        let err = resolve_with(ResolveInputs {
            env: vec![("ATHENA_API_PORT".into(), "not-a-number".into())],
            ..Default::default()
        })
        .expect_err("must be refused");
        assert!(matches!(err, ConfigError::BadOverride { .. }), "{err}");
    }

    #[test]
    fn athena_home_itself_is_not_treated_as_an_override() {
        // Home resolution already consumed it (ATH-CFG-010); re-applying it would let the
        // value disagree with the resolved home.
        let c = resolve_with(ResolveInputs {
            env: vec![("ATHENA_HOME".into(), "/elsewhere/.athena".into())],
            ..Default::default()
        })
        .expect("resolves");
        assert_eq!(c.athena.home, "./.athena");
    }

    #[test]
    fn home_variable_is_expanded_before_validation() {
        // ATH-CFG-010: the typed config must see a real path, not the placeholder.
        let c = resolve_with(ResolveInputs::default()).expect("resolves");
        assert_eq!(c.athena.data.database, "/work/proj/.athena/var/athena.db");
        assert_eq!(c.skills.local_dir, "/work/proj/.athena/skills");
        assert_eq!(c.skills.dir, "/work/proj/.athena/var/skills");
        assert_eq!(c.core.agent.workspace, "/work/proj/.athena/var/core-agent");
        assert!(
            !serde_yaml::to_string(&c).expect("serialize").contains("${"),
            "no placeholder may survive expansion"
        );
    }

    #[test]
    fn any_other_variable_reference_is_refused() {
        let err = resolve_with(ResolveInputs {
            file_yaml: Some("athena:\n  data:\n    dir: ${HOME}/athena\n".into()),
            ..Default::default()
        })
        .expect_err("must be refused");
        match err {
            ConfigError::UnsupportedVariable { name, at } => {
                assert_eq!(name, "HOME");
                assert_eq!(at, "athena.data.dir");
            }
            other => panic!("wrong error: {other}"),
        }
    }

    #[test]
    fn a_secret_in_the_configuration_file_is_refused() {
        // ATH-CFG-005 / ATH-DEV-021: config.yaml may be version-controlled.
        for key in ["api_key", "token", "provider_secret", "password"] {
            let yaml = format!("athena:\n  api:\n    {key}: abc123\n");
            let err = resolve_with(ResolveInputs {
                file_yaml: Some(yaml),
                ..Default::default()
            })
            .expect_err("must be refused");
            assert!(
                matches!(err, ConfigError::SecretInConfig { .. }),
                "{key}: {err}"
            );
        }
    }

    #[test]
    fn the_counting_rule_keys_are_not_secrets() {
        // The real resident boot aborted on the schema's own `memory.tokenizer` default
        // (2026-10-18): the substring "token" is not automatically a secret. The full
        // default file itself must resolve, and a `token*` *secret* still refuses.
        let yaml =
            "memory:\n  tokenizer: provider-default-claude\n  short_term_max_tokens: 32000\n";
        let config = resolve_with(ResolveInputs {
            file_yaml: Some(yaml.into()),
            ..Default::default()
        })
        .expect("counting-rule keys resolve");
        assert_eq!(config.memory.tokenizer, "provider-default-claude");

        let err = resolve_with(ResolveInputs {
            file_yaml: Some("api:\n  auth_token: abc123\n".into()),
            ..Default::default()
        })
        .expect_err("an auth token is still a secret");
        assert!(matches!(err, ConfigError::SecretInConfig { .. }), "{err}");
    }

    #[test]
    fn an_unknown_file_key_is_refused_by_the_typed_schema() {
        let err = resolve_with(ResolveInputs {
            file_yaml: Some("scheduler:\n  max_concurent_tasks: 2\n".into()),
            ..Default::default()
        })
        .expect_err("must be refused");
        assert!(matches!(err, ConfigError::Invalid { .. }), "{err}");
    }

    #[test]
    fn sequences_replace_rather_than_append() {
        // Replacement is what allows a later layer to shrink an allow-list.
        let c = resolve_with(ResolveInputs {
            file_yaml: Some("skills:\n  allowed_publishers: [aquaxis, other]\n".into()),
            cli: vec!["skills.allowed_publishers=aquaxis".into()],
            ..Default::default()
        })
        .expect("resolves");
        assert_eq!(c.skills.allowed_publishers, vec!["aquaxis".to_string()]);
    }

    #[test]
    fn comma_separated_overrides_populate_a_sequence() {
        let c = resolve_with(ResolveInputs {
            env: vec![(
                "ATHENA_CORE_AGENT_DELEGABLE_TOOLS".into(),
                "fs_read,shell,git".into(),
            )],
            ..Default::default()
        })
        .expect("resolves");
        assert_eq!(
            c.core.agent.delegable_tools,
            vec![
                athena_types::ToolName::FsRead,
                athena_types::ToolName::Shell,
                athena_types::ToolName::Git
            ]
        );
    }

    #[test]
    fn a_cli_override_without_a_value_is_refused() {
        let err = resolve_with(ResolveInputs {
            cli: vec!["athena.api.port".into()],
            ..Default::default()
        })
        .expect_err("must be refused");
        assert!(matches!(err, ConfigError::BadOverride { .. }), "{err}");
    }

    #[test]
    fn a_cli_override_naming_no_key_is_refused() {
        let err = resolve_with(ResolveInputs {
            cli: vec!["scheduler.nonexistent.deep=1".into()],
            ..Default::default()
        })
        .expect_err("must be refused");
        assert!(
            matches!(
                err,
                ConfigError::UnknownKey { .. } | ConfigError::Invalid { .. }
            ),
            "{err}"
        );
    }

    #[test]
    fn redaction_masks_secret_shaped_keys() {
        let tree: Value =
            serde_yaml::from_str("a:\n  api_key: s3cret\n  port: 1\nb:\n  - token: t\n")
                .expect("parse");
        let out = serde_yaml::to_string(&redacted_snapshot(&tree)).expect("serialize");
        assert!(!out.contains("s3cret"), "{out}");
        assert!(!out.contains("token: t"), "{out}");
        assert!(out.contains("[redacted]"), "{out}");
        assert!(out.contains("port: 1"), "{out}");
    }

    #[test]
    fn a_missing_configuration_file_is_not_an_error() {
        // The defaults layer stands alone.
        let got = read_config_file(Path::new("/nonexistent/athena/config.yaml"))
            .expect("missing file is fine");
        assert!(got.is_none());
    }

    #[test]
    fn the_returned_tree_reflects_every_layer() {
        let (config, tree) = resolve(
            &home(),
            &ResolveInputs {
                file_yaml: Some("scheduler:\n  max_queue_length: 10\n".into()),
                cli: vec!["athena.api.port=1234".into()],
                ..Default::default()
            },
        )
        .expect("resolves");
        assert_eq!(config.scheduler.max_queue_length, 10);
        assert_eq!(tree["scheduler"]["max_queue_length"].as_u64(), Some(10));
        assert_eq!(tree["athena"]["api"]["port"].as_u64(), Some(1234));
    }
}
