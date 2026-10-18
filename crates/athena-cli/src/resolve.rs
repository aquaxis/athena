//! `ATHENA_HOME` resolution for the CLI (ATH-CFG-010).
//!
//! The owning project is decided by locating `.athena/`, never by the executable's location
//! and never by a configuration file's location. The same rule lives in `athena-config` for
//! the API process; this is the CLI's own copy because the dependency graph does not permit a
//! `athena-cli -> athena-config` edge (spec section 32.2: the UI reaches Core only through
//! the Core API).

use std::path::{Path, PathBuf};

/// The directory name ATHENA occupies inside a project.
pub const HOME_DIR_NAME: &str = ".athena";

/// Search `start` and its ancestors for an existing `.athena/` directory.
///
/// Returns the `<PROJECT_ROOT>/.athena` path, or [`None`] when nothing resolves — the caller
/// then prompts for `athena init` and exits 2 (ATH-CLI-010). Nothing is ever created here.
#[must_use]
pub fn resolve_home(start: &Path) -> Option<PathBuf> {
    let start = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(start)
    };
    let mut probe = start.as_path();
    loop {
        let candidate = probe.join(HOME_DIR_NAME);
        if candidate.is_dir() {
            return Some(candidate);
        }
        probe = probe.parent()?;
    }
}
