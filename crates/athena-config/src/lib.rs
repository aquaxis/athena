//! Configuration schema and resolution (spec section 28).
//!
//! The startup sequence is fixed by the specification and realized by [`load`]:
//!
//! 1. Resolve `ATHENA_HOME` by `--home`, `ATHENA_HOME`, then an upward search for `.athena/`.
//!    Failing all three aborts rather than creating a default (ATH-CFG-010).
//! 2. Read `<ATHENA_HOME>/config.yaml` unless one was named explicitly (ATH-CFG-011).
//! 3. Layer defaults, file, environment, CLI in that order (ATH-CFG-001).
//! 4. Expand `${athena.home}` — before validation, so constraints see real paths.
//! 5. Validate against the typed schema and the section 28.2 cross-constraints, aborting on
//!    any violation (ATH-CFG-003, ATH-CFG-008).
//!
//! The caller then reports the redacted effective configuration as `ConfigurationLoaded`
//! (ATH-CFG-004).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod error;
pub mod home;
pub mod layer;
pub mod policy;
pub mod schema;
pub mod validate;

pub use error::{ConfigError, Result};
pub use home::{AthenaHome, HOME_DIR_NAME, HomeInputs, HomeOrigin, resolve_home};
pub use layer::{
    ENV_PREFIX, Override, ResolveInputs, env_from_process, read_config_file, redacted_snapshot,
    resolve,
};
pub use policy::{Classification, CompiledPolicy, Operation};
pub use schema::*;
pub use validate::{effective_timeouts, validate};

use std::path::PathBuf;

/// Everything the process needs to start.
#[derive(Debug)]
pub struct Loaded {
    /// The validated configuration.
    pub config: Config,
    /// The resolved home, carrying how it was found.
    pub home: AthenaHome,
    /// The configuration file that was read, if any.
    pub config_file: Option<PathBuf>,
    /// The effective configuration as a value tree, for the `ConfigurationLoaded` payload.
    ///
    /// Pass through [`redacted_snapshot`] before emitting (ATH-CFG-004).
    pub effective: serde_yaml::Value,
    /// Policy rules with their patterns compiled, so an invalid pattern has already failed.
    pub policy: CompiledPolicy,
}

/// Command-line inputs that affect configuration loading.
#[derive(Debug, Clone, Default)]
pub struct CliInputs {
    /// `--home`.
    pub home: Option<PathBuf>,
    /// `--config`, which replaces the default file location.
    pub config: Option<PathBuf>,
    /// `--set path=value` overrides, the highest-precedence layer.
    pub set: Vec<String>,
}

/// Load, resolve and validate the configuration.
///
/// `current_dir` is where the upward `.athena/` search begins.
pub fn load(current_dir: PathBuf, cli: &CliInputs) -> Result<Loaded> {
    let env: Vec<(String, String)> = env_from_process();

    let home = resolve_home(&HomeInputs {
        cli_home: cli.home.clone(),
        env_home: env
            .iter()
            .find(|(k, _)| k == "ATHENA_HOME")
            .map(|(_, v)| PathBuf::from(v)),
        current_dir,
    })?;

    let config_file = cli.config.clone().unwrap_or_else(|| home.config_file());
    let file_yaml = read_config_file(&config_file)?;
    // Report the path only when a file was actually read, so an error message never points at
    // a file that does not exist.
    let read_from = file_yaml.as_ref().map(|_| config_file.clone());

    let (config, effective) = resolve(
        &home,
        &ResolveInputs {
            file_yaml,
            file_path: read_from.clone(),
            env,
            cli: cli.set.clone(),
        },
    )?;

    validate(&config)?;
    let policy = CompiledPolicy::compile(&config.policy)?;

    Ok(Loaded {
        config,
        home,
        config_file: read_from,
        effective,
        policy,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project tree with a real `.athena/` directory.
    struct Project(PathBuf);

    impl Project {
        fn new(tag: &str) -> Self {
            let base = std::env::temp_dir().join(format!(
                "athena-load-{tag}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or_default()
            ));
            std::fs::create_dir_all(base.join(".athena")).expect("create project");
            Self(base)
        }

        fn write_config(&self, yaml: &str) {
            std::fs::write(self.0.join(".athena/config.yaml"), yaml).expect("write config");
        }

        fn path(&self) -> PathBuf {
            self.0.clone()
        }
    }

    impl Drop for Project {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The minimum a file must state, since ATHENA supplies no provider, model or ceiling.
    const MINIMAL: &str = r"
core:
  agent:
    provider: claude
    model: claude-opus-5
    delegable_tools: [fs_read, fs_write, shell]
";

    #[test]
    fn a_minimal_project_loads_and_validates() {
        let project = Project::new("ok");
        project.write_config(MINIMAL);

        let loaded = load(project.path(), &CliInputs::default()).expect("loads");
        assert_eq!(loaded.home.origin, HomeOrigin::UpwardSearch);
        assert_eq!(loaded.config.core.agent.provider, "claude");
        assert_eq!(loaded.config.athena.api.port, 8080);
        assert!(loaded.config_file.is_some());
        assert_eq!(loaded.policy.len(), 6);
    }

    #[test]
    fn home_is_found_from_a_nested_directory() {
        let project = Project::new("nested");
        project.write_config(MINIMAL);
        let nested = project.path().join("src/deep");
        std::fs::create_dir_all(&nested).expect("mkdir");

        let loaded = load(nested, &CliInputs::default()).expect("loads");
        assert_eq!(loaded.home.path, project.path().join(".athena"));
    }

    #[test]
    fn home_variable_is_expanded_against_the_resolved_home() {
        let project = Project::new("expand");
        project.write_config(MINIMAL);

        let loaded = load(project.path(), &CliInputs::default()).expect("loads");
        let expected = project.path().join(".athena/var/athena.db");
        assert_eq!(
            loaded.config.athena.data.database,
            expected.to_string_lossy()
        );
    }

    #[test]
    fn cli_overrides_win_over_the_file() {
        let project = Project::new("cli");
        project.write_config(&format!(
            "{MINIMAL}\nscheduler:\n  max_concurrent_tasks: 2\n"
        ));

        let loaded = load(
            project.path(),
            &CliInputs {
                set: vec!["scheduler.max_concurrent_tasks=5".into()],
                ..Default::default()
            },
        )
        .expect("loads");
        assert_eq!(loaded.config.scheduler.max_concurrent_tasks, 5);
    }

    #[test]
    fn an_explicit_config_path_replaces_the_default_location() {
        let project = Project::new("explicit");
        project.write_config("scheduler:\n  max_concurrent_tasks: 99\n");
        let other = project.path().join("other.yaml");
        std::fs::write(&other, MINIMAL).expect("write");

        let loaded = load(
            project.path(),
            &CliInputs {
                config: Some(other.clone()),
                ..Default::default()
            },
        )
        .expect("loads");
        // The default-location file was not read, so its value did not apply.
        assert_eq!(loaded.config.scheduler.max_concurrent_tasks, 8);
        assert_eq!(loaded.config_file, Some(other));
    }

    #[test]
    fn a_missing_home_aborts_with_a_pointer_to_athena_init() {
        // ATH-CFG-010 / ATH-CLI-010.
        let base = std::env::temp_dir().join(format!("athena-nohome-{}", std::process::id()));
        std::fs::create_dir_all(&base).expect("mkdir");
        // Guard against an ancestor `.athena/` making this vacuous.
        let has_ancestor_home = base.ancestors().any(|a| a.join(".athena").is_dir());
        if !has_ancestor_home {
            let err = load(base.clone(), &CliInputs::default()).expect_err("must abort");
            assert!(err.to_string().contains("athena init"), "{err}");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn an_invalid_configuration_aborts_rather_than_falling_back() {
        // ATH-CFG-003: no partial-default continuation.
        let project = Project::new("invalid");
        project.write_config(&format!(
            "{MINIMAL}\ntimeout:\n  startup: 30s\n  execution: 30m\n  message_reply: 1m\n"
        ));
        let err = load(project.path(), &CliInputs::default()).expect_err("must abort");
        assert!(err.to_string().contains("message_reply"), "{err}");
    }

    #[test]
    fn a_configuration_missing_the_provider_aborts() {
        // ATH-AGENT-008: ATHENA supplies no default, so this cannot be papered over.
        let project = Project::new("noprovider");
        project.write_config("scheduler:\n  max_concurrent_tasks: 4\n");
        let err = load(project.path(), &CliInputs::default()).expect_err("must abort");
        assert!(err.to_string().contains("core.agent.provider"), "{err}");
    }

    #[test]
    fn the_effective_tree_is_redactable_for_the_event_payload() {
        // ATH-CFG-004: the payload carries the effective configuration without secrets.
        let project = Project::new("effective");
        project.write_config(MINIMAL);

        let loaded = load(project.path(), &CliInputs::default()).expect("loads");
        let redacted = redacted_snapshot(&loaded.effective);
        let text = serde_yaml::to_string(&redacted).expect("serialize");
        assert!(text.contains("max_concurrent_tasks"), "{text}");
        assert!(
            !text.contains("${"),
            "placeholders must be expanded: {text}"
        );
    }
}
