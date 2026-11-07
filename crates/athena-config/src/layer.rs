//! Layered configuration resolution (ATH-CFG-001, ATH-CFG-002, ATH-CFG-010).
//!
//! Resolution order is defaults, then file, then environment, then CLI. Layering happens on a
//! TOML value tree rather than on the typed struct, for two reasons: an override must be able
//! to address a single leaf without reconstructing its parents, and `${athena.home}` has to be
//! expanded *before* the typed schema is validated (ATH-CFG-010).

use crate::error::{ConfigError, Result};
use crate::home::AthenaHome;
use crate::schema::Config;
use std::path::Path;
use toml::{Table, Value};

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
    /// Raw TOML of the configuration file, when one exists.
    pub file_toml: Option<String>,
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
    let mut tree = default_tree()?;

    // Layer 2: the file.
    if let Some(toml_text) = &inputs.file_toml {
        let from_file: Value =
            toml::from_str(toml_text).map_err(|source| ConfigError::Invalid {
                path: inputs.file_path.clone(),
                source: Box::new(source),
            })?;
        reject_secrets(&from_file, &mut Vec::new())?;
        merge(&mut tree, from_file);
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

    let config: Config = tree
        .clone()
        .try_into()
        .map_err(|source| ConfigError::Invalid {
            path: inputs.file_path.clone(),
            source: Box::new(source),
        })?;

    Ok((config, tree))
}

/// The defaults layer as a TOML value tree.
///
/// Every key the schema knows appears here, because override paths are matched against this
/// tree rather than against a guessed shape.
fn default_tree() -> Result<Value> {
    Value::try_from(Config::default()).map_err(|source| ConfigError::Invalid {
        path: None,
        source: Box::new(source),
    })
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
/// Tables merge key by key; every other value replaces wholesale. Arrays replace rather
/// than append, because appending would make it impossible for a later layer to *remove* an
/// entry from an allow-list.
fn merge(dst: &mut Value, src: Value) {
    match (dst, src) {
        (Value::Table(dst_map), Value::Table(src_map)) => {
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
/// `config.toml` may be version-controlled (ATH-DEV-020), so a token written here would be
/// committed. `deny_unknown_fields` would also reject these keys, but with a message about an
/// unknown field rather than about secrets.
///
/// The match is substring-based, so counting-rule keys that legitimately carry the word
/// are carved out first: `memory.tokenizer` (the named counting rule, ATH-MEM-009) and
/// `…max_tokens` (memory ceilings) are not secrets. Discovered by the first resident
/// boot (2026-10-18): the real startup aborted on the schema's own default key.
fn reject_secrets(value: &Value, path: &mut Vec<String>) -> Result<()> {
    if let Value::Table(map) = value {
        for (key, val) in map {
            let key_str = key.to_ascii_lowercase();
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
    let Value::Table(map) = node else {
        return None;
    };

    // Longest candidate first.
    for take in (1..=segments.len()).rev() {
        let candidate = segments[..take].join("_");
        for (key, child) in map {
            // Compare on a normalized form so `agent-cli` matches `AGENT_CLI`.
            if key.replace('-', "_").eq_ignore_ascii_case(&candidate)
                && let Some(mut rest) = match_path(child, &segments[take..])
            {
                let mut path = vec![key.to_string()];
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
        Some(Value::Boolean(_)) => Value::Boolean(
            over.value
                .parse::<bool>()
                .map_err(|_| bad("expected true or false"))?,
        ),
        Some(Value::Integer(_)) => Value::Integer(
            over.value
                .parse::<i64>()
                .map_err(|_| bad("expected a non-negative integer"))?,
        ),
        Some(Value::Float(_)) => Value::Float(
            over.value
                .parse::<f64>()
                .map_err(|_| bad("expected a number"))?,
        ),
        // Arrays take a comma-separated list, the only workable spelling for an env var.
        Some(Value::Array(_)) => Value::Array(
            over.value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| Value::String(s.to_string()))
                .collect(),
        ),
        // Strings cover durations too: they stay in their `30s` spelling and are parsed by the
        // typed schema afterwards.
        Some(Value::String(_)) | None => Value::String(over.value.clone()),
        Some(Value::Table(_)) => return Err(bad("cannot replace a section with a scalar")),
        Some(Value::Datetime(_)) => return Err(bad("cannot override a datetime value")),
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

/// Write a value by key path, creating intermediate tables as needed.
///
/// Returns `None` when an intermediate node exists but is not a table, which means the path
/// does not address a real key.
fn path_set(tree: &mut Value, path: &[String], value: Value) -> Option<()> {
    let (last, parents) = path.split_last()?;
    let mut node = tree;
    for key in parents {
        if !node.is_table() {
            return None;
        }
        let map = node.as_table_mut()?;
        if !map.contains_key(key.as_str()) {
            map.insert(key.clone(), Value::Table(Table::new()));
        }
        node = map.get_mut(key.as_str())?;
    }
    node.as_table_mut()?.insert(last.clone(), value);
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
        Value::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                path.push(index.to_string());
                expand_home_var(item, home, path)?;
                path.pop();
            }
            Ok(())
        }
        Value::Table(map) => {
            for (key, val) in map.iter_mut() {
                path.push(key.clone());
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
            Value::Table(map) => {
                let mut out = Table::new();
                for (key, val) in map {
                    let name = key.to_ascii_lowercase().replace(['-', '.'], "_");
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
                Value::Table(out)
            }
            Value::Array(items) => Value::Array(items.iter().map(walk).collect()),
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
