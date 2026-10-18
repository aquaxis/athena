//! `ATHENA_HOME` resolution (ATH-CFG-010).
//!
//! The owning project is decided by locating `.athena/`, never by where the executable lives
//! and never by where the configuration file lives. Both of those would be ambiguous: the
//! former breaks under package-manager and global installs, the latter under `--config`, which
//! can point anywhere.

use crate::error::{ConfigError, Result};
use std::path::{Path, PathBuf};

/// The directory name ATHENA occupies inside a project.
pub const HOME_DIR_NAME: &str = ".athena";

/// Where a resolved `ATHENA_HOME` came from, in the precedence order of ATH-CFG-010.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeOrigin {
    /// The `--home` CLI option.
    CliOption,
    /// The `ATHENA_HOME` environment variable.
    Environment,
    /// An upward search from the current directory.
    UpwardSearch,
}

/// A resolved `ATHENA_HOME`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AthenaHome {
    /// Absolute path to `<PROJECT_ROOT>/.athena`.
    pub path: PathBuf,
    /// How it was found.
    pub origin: HomeOrigin,
}

impl AthenaHome {
    /// The project root, which is the parent of `.athena` (spec section 4.3.4).
    #[must_use]
    pub fn project_root(&self) -> &Path {
        // A resolved home always has a parent: resolution either joins HOME_DIR_NAME onto a
        // directory or accepts an explicit path, and `/` alone cannot be a home.
        self.path.parent().unwrap_or(&self.path)
    }

    /// The default configuration file location (ATH-CFG-011).
    #[must_use]
    pub fn config_file(&self) -> PathBuf {
        self.path.join("config.yaml")
    }

    /// The runtime data directory. The sole writable area (ATH-OPS-006).
    #[must_use]
    pub fn var_dir(&self) -> PathBuf {
        self.path.join("var")
    }
}

/// Inputs to resolution, in precedence order.
#[derive(Debug, Clone, Default)]
pub struct HomeInputs {
    /// `--home`, the highest precedence.
    pub cli_home: Option<PathBuf>,
    /// `ATHENA_HOME`.
    pub env_home: Option<PathBuf>,
    /// Where the upward search starts.
    pub current_dir: PathBuf,
}

/// Resolve `ATHENA_HOME` (ATH-CFG-010).
///
/// Order: `--home`, then `ATHENA_HOME`, then an upward search for `.athena/`, taking the first
/// directory found. When none resolves, this fails rather than creating a default: an implicit
/// creation would silently bind the process to the wrong project.
pub fn resolve_home(inputs: &HomeInputs) -> Result<AthenaHome> {
    if let Some(explicit) = &inputs.cli_home {
        return accept_explicit(explicit, HomeOrigin::CliOption);
    }
    if let Some(explicit) = &inputs.env_home {
        return accept_explicit(explicit, HomeOrigin::Environment);
    }

    let mut dir: &Path = &inputs.current_dir;
    loop {
        let candidate = dir.join(HOME_DIR_NAME);
        if candidate.is_dir() {
            return Ok(AthenaHome {
                path: absolutize(&candidate, &inputs.current_dir),
                origin: HomeOrigin::UpwardSearch,
            });
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => {
                return Err(ConfigError::HomeUnresolved {
                    searched_from: inputs.current_dir.clone(),
                });
            }
        }
    }
}

/// Accept an explicitly supplied home, which must already exist as a directory.
///
/// An explicit path is not created on demand: `athena init` is the only thing that creates a
/// home (ATH-CLI-009), so a missing one here means the user named the wrong place.
fn accept_explicit(path: &Path, origin: HomeOrigin) -> Result<AthenaHome> {
    if !path.is_dir() {
        return Err(ConfigError::NotADirectory {
            what: "ATHENA_HOME",
            path: path.to_path_buf(),
        });
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    Ok(AthenaHome {
        path: absolutize(path, &cwd),
        origin,
    })
}

/// Make a path absolute without requiring it to be canonicalizable.
///
/// `canonicalize` is avoided deliberately: it resolves symlinks, and a project reached through
/// a symlinked path would then derive a different `ProjectId` (ATH-DATA-011) than the one the
/// user addressed.
fn absolutize(path: &Path, base: &Path) -> PathBuf {
    if path.is_absolute() {
        normalize(path)
    } else {
        normalize(&base.join(path))
    }
}

/// Remove `.` and resolve `..` lexically.
fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A disposable directory tree.
    struct TempTree(PathBuf);

    impl TempTree {
        fn new(tag: &str) -> Self {
            let base = std::env::temp_dir().join(format!(
                "athena-home-{tag}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or_default()
            ));
            std::fs::create_dir_all(&base).expect("create temp tree");
            Self(base)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn mkdir(&self, rel: &str) -> PathBuf {
            let p = self.0.join(rel);
            std::fs::create_dir_all(&p).expect("mkdir");
            p
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn cli_option_takes_precedence_over_environment_and_search() {
        let tree = TempTree::new("precedence");
        let cli = tree.mkdir("from-cli/.athena");
        let env = tree.mkdir("from-env/.athena");
        let cwd = tree.mkdir("deep/nested");
        tree.mkdir("deep/.athena");

        let home = resolve_home(&HomeInputs {
            cli_home: Some(cli.clone()),
            env_home: Some(env),
            current_dir: cwd,
        })
        .expect("resolves");
        assert_eq!(home.origin, HomeOrigin::CliOption);
        assert_eq!(home.path, normalize(&cli));
    }

    #[test]
    fn environment_takes_precedence_over_search() {
        let tree = TempTree::new("env");
        let env = tree.mkdir("from-env/.athena");
        let cwd = tree.mkdir("deep/nested");
        tree.mkdir("deep/.athena");

        let home = resolve_home(&HomeInputs {
            cli_home: None,
            env_home: Some(env.clone()),
            current_dir: cwd,
        })
        .expect("resolves");
        assert_eq!(home.origin, HomeOrigin::Environment);
        assert_eq!(home.path, normalize(&env));
    }

    #[test]
    fn upward_search_finds_the_nearest_home() {
        // The nearest ancestor wins, so a nested project is not captured by an outer one.
        let tree = TempTree::new("search");
        tree.mkdir(".athena");
        let inner = tree.mkdir("inner/.athena");
        let cwd = tree.mkdir("inner/a/b/c");

        let home = resolve_home(&HomeInputs {
            cli_home: None,
            env_home: None,
            current_dir: cwd,
        })
        .expect("resolves");
        assert_eq!(home.origin, HomeOrigin::UpwardSearch);
        assert_eq!(home.path, normalize(&inner));
    }

    #[test]
    fn unresolved_home_is_an_error_and_never_an_implicit_creation() {
        // ATH-CFG-010: no implicit default; the caller must be told to run `athena init`.
        let tree = TempTree::new("missing");
        let cwd = tree.mkdir("no/home/here");

        let err = resolve_home(&HomeInputs {
            cli_home: None,
            env_home: None,
            current_dir: cwd.clone(),
        })
        .expect_err("must not resolve");

        assert!(err.to_string().contains("athena init"), "{err}");
        match &err {
            ConfigError::HomeUnresolved { searched_from } => assert_eq!(searched_from, &cwd),
            other => panic!("wrong error: {other}"),
        }
        assert!(!cwd.join(HOME_DIR_NAME).exists(), "nothing may be created");
    }

    #[test]
    fn an_explicit_home_that_does_not_exist_is_refused() {
        let tree = TempTree::new("explicit-missing");
        let missing = tree.path().join("nope/.athena");
        let err = resolve_home(&HomeInputs {
            cli_home: Some(missing.clone()),
            env_home: None,
            current_dir: tree.path().to_path_buf(),
        })
        .expect_err("must be refused");
        assert!(matches!(err, ConfigError::NotADirectory { .. }), "{err}");
    }

    #[test]
    fn project_root_is_the_parent_of_the_home() {
        // Spec section 4.3.4.
        let tree = TempTree::new("root");
        let root = tree.mkdir("proj");
        let home_dir = tree.mkdir("proj/.athena");
        let home = resolve_home(&HomeInputs {
            cli_home: Some(home_dir),
            env_home: None,
            current_dir: tree.path().to_path_buf(),
        })
        .expect("resolves");
        assert_eq!(home.project_root(), normalize(&root).as_path());
    }

    #[test]
    fn derived_paths_match_the_specification() {
        let tree = TempTree::new("paths");
        let home_dir = tree.mkdir("p/.athena");
        let home = resolve_home(&HomeInputs {
            cli_home: Some(home_dir.clone()),
            env_home: None,
            current_dir: tree.path().to_path_buf(),
        })
        .expect("resolves");
        assert_eq!(home.config_file(), normalize(&home_dir).join("config.yaml"));
        assert_eq!(home.var_dir(), normalize(&home_dir).join("var"));
    }

    #[test]
    fn relative_explicit_homes_become_absolute() {
        // Downstream path joining and ProjectId derivation both need an absolute base.
        let tree = TempTree::new("relative");
        let home_dir = tree.mkdir("rel/.athena");
        let home = resolve_home(&HomeInputs {
            cli_home: Some(home_dir),
            env_home: None,
            current_dir: tree.path().to_path_buf(),
        })
        .expect("resolves");
        assert!(home.path.is_absolute(), "{:?}", home.path);
    }

    #[test]
    fn normalization_is_lexical_and_does_not_require_existence() {
        let n = normalize(Path::new("/a/b/../c/./d"));
        assert_eq!(n, PathBuf::from("/a/c/d"));
    }
}
