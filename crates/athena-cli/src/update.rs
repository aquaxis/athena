//! `athena update` — the update face of the installed tool (R-OPS-11 / R-CLI-08; task X-16;
//! design §2.9 with D-21/D-22).
//!
//! The instruction directs: running `athena update` performs work *equivalent to* the published
//! one-liner `curl -fsSL https://raw.githubusercontent.com/aquaxis/athena/main/install.sh | sh`,
//! and, when a `.athena/` project is present, updates **only** `personas` and `skills`.
//!
//! Two independent faces:
//!
//! 1. **Tool refresh** (always): the §2.5 installer pipeline run against the *published*
//!    source — fetch the repository publish home (the same default `install.sh` carries as
//!    `REPO_URL`, overridable with `ATHENA_REPO_URL`), build in release mode, and re-install
//!    the deliverable binaries onto PATH via `cargo install --path` (`athena`, `athena-api`).
//!    This is `install.sh`'s contract, run from inside the installed tool.
//! 2. **Project definition refresh** (only when `.athena/` resolves from the invocation
//!    directory by the upward search, `ATH-CFG-010`): refresh **only** the project's
//!    `personas/` and `skills/` from the *published* repository's shipped definitions (the
//!    freshly fetched source tree of the tool refresh — the published state, not this binary's
//!    build-time snapshot), never `config.yaml`, `agents/`, or `var/` (`ATH-OPS-02`).
//!
//! **The refresh discipline is additive-only** (D-21): a definition file the project does not
//! have is added; an already-present file is left untouched unless it is provably in shipped
//! form — either it already equals the published content (up to date) or it equals the
//! previously shipped content recorded in the provenance manifest under `var/`
//! (`var/update-manifest.json`, verbatim shipped content per file, written by `athena init`
//! for the definitions it creates and by every `athena update` run for the definitions it
//! knows). An operator-modified definition is never overwritten silently; a project with no
//! manifest (pre-manifest init) only gains missing files, and its present files stay
//! operator-owned until an update run can prove them shipped. The manifest lives under `var/`
//! because ATHENA's run-time write authority is exactly that directory (`ATH-OPS-002`);
//! `config.yaml`, `agents/`, `personas/`, and `skills/` receive writes only through an
//! install/update face.
//!
//! The verb is a **tool-side** operation (D-21): no Core API call, no `agent-cli` launch
//! (`ATH-CLI-002` intact), no resident `athena-api` needed. The only processes it ever
//! spawns are the §2.5 pipeline's own — `git` (the fetch) and `cargo` (the build/install) —
//! asserted structurally in `tests/phase6_cli.rs` (D-22).

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Outcome;
use crate::resolve::resolve_home;

/// The publish home the tool refresh fetches from — the same default the `install.sh`
/// header carries (`R-OPS-10`'s published repository; overridable via `ATHENA_REPO_URL`,
/// mirroring `install.sh`).
pub const REPO_URL: &str = "https://github.com/aquaxis/athena.git";

/// The provenance manifest under `<ATHENA_HOME>/var/` (see the module doc).
pub const MANIFEST_NAME: &str = "update-manifest.json";

/// The project definition directories `athena update` touches — exactly the two the
/// instruction names (D-21). Everything else in `.athena/` is out of its reach.
pub const DEFINITION_DIRS: &[&str] = &["personas", "skills"];

/// Why `athena update` refused (the lib maps both variants to the execution-failure
/// exit code 4, `ATH-CLI-008` / R-CLI-05).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateFault {
    /// The tool-refresh pipeline failed at a named step.
    ToolRefresh { step: &'static str, detail: String },
    /// Filesystem failure on the project-side face.
    Io { path: String, detail: String },
}

impl fmt::Display for UpdateFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ToolRefresh { step, detail } => write!(f, "{step}: {detail}"),
            Self::Io { path, detail } => write!(f, "{path}: {detail}"),
        }
    }
}

/// One project-side refresh outcome per definition file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionAction {
    /// What happened to the file.
    pub kind: DefinitionActionKind,
    /// The file's path under `<ATHENA_HOME>/` (for example `personas/producer.md`).
    pub rel_path: String,
}

/// What an update run did to one definition file (D-21's additive-only discipline).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefinitionActionKind {
    /// The file was absent — the published content was added.
    Added,
    /// The file is in shipped form and differs from the published content — refreshed.
    Refreshed,
    /// The file exists and differs from any shipped form this run can prove — left untouched.
    KeptOperator,
    /// The file already equals the published content — nothing to do.
    UpToDate,
}

impl DefinitionActionKind {
    fn label(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Refreshed => "updated",
            Self::KeptOperator => "kept (operator-modified)",
            Self::UpToDate => "unchanged",
        }
    }
}

/// The project-side refresh outcome.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProjectRefresh {
    /// The `.athena/` home the refresh ran against.
    pub home: PathBuf,
    /// Per-definition actions in `personas/`/`skills/` walk order.
    pub actions: Vec<DefinitionAction>,
}

/// The outcome of one `athena update` invocation.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UpdateReport {
    /// Whether the tool-refresh face completed (it runs before the project face; a failure
    /// means no report is produced at all).
    pub tool_refreshed: bool,
    /// The project face's outcome, when a `.athena/` project resolved.
    pub project: Option<ProjectRefresh>,
}

/// The provenance manifest (see the module doc): per definition file, the shipped content
/// this binary or update run knows. Verbatim content, not a digest — no hashing dependency
/// on the CLI crate, and the exact shipped bytes are auditable in place.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateManifest {
    /// Key: `<dir>/<relative path>` (forward slashes); value: the shipped content.
    pub files: BTreeMap<String, String>,
}

/// Read the manifest of the project at `home`; a missing manifest is an empty one.
///
/// # Errors
/// [`UpdateFault::Io`] when the manifest exists but cannot be read or parsed.
pub fn read_manifest(home: &Path) -> Result<UpdateManifest, UpdateFault> {
    let path = home.join("var").join(MANIFEST_NAME);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(UpdateManifest::default());
        }
        Err(error) => {
            return Err(UpdateFault::Io {
                path: path.display().to_string(),
                detail: error.to_string(),
            });
        }
    };
    serde_json::from_str(&text).map_err(|error| UpdateFault::Io {
        path: path.display().to_string(),
        detail: error.to_string(),
    })
}

/// Write the manifest of the project at `home`, creating `var/` when missing.
pub fn write_manifest(home: &Path, manifest: &UpdateManifest) -> Result<(), UpdateFault> {
    let var = home.join("var");
    std::fs::create_dir_all(&var).map_err(|error| UpdateFault::Io {
        path: var.display().to_string(),
        detail: error.to_string(),
    })?;
    let path = var.join(MANIFEST_NAME);
    let text = serde_json::to_string_pretty(manifest).map_err(|error| UpdateFault::Io {
        path: path.display().to_string(),
        detail: error.to_string(),
    })?;
    std::fs::write(&path, text).map_err(|error| UpdateFault::Io {
        path: path.display().to_string(),
        detail: error.to_string(),
    })
}

/// Record in `manifest` that `rel_key`'s shipped form is `content` — for the files this run
/// wrote or proved shipped. A kept-operator file is **not** recorded: the manifest only ever
/// carries content an ATHENA install/update face wrote, so an operator's modification stays
/// unlegitimized.
fn record_shipped(manifest: &mut UpdateManifest, rel_key: &str, content: &str, wrote: bool) {
    if wrote {
        manifest
            .files
            .insert(rel_key.to_string(), content.to_string());
    }
}

/// Run one `athena update` invocation (R-CLI-08): the tool-refresh face always; the
/// project-definition refresh only when `.athena/` resolves from `cwd`. The returned
/// [`UpdateReport`] carries both faces' outcomes.
///
/// # Errors
/// [`UpdateFault`] naming the failed step or path (the lib maps it to exit code 4).
pub fn run_update(cwd: &Path) -> Result<UpdateReport, UpdateFault> {
    // --- Face 1: the tool refresh (the §2.5 pipeline against the publish home, D-22) ---
    let source = refresh_tool()?;

    // --- Face 2: the project definitions (only when `.athena/` resolves, D-21) ---
    let project = match resolve_home(cwd) {
        Some(home) => Some(refresh_project(&home, &source)?),
        None => None,
    };

    // The fetched source tree was the tool-refresh scratch space; the installed binaries
    // never hold a path back into it, so the best-effort cleanup leaves nothing behind.
    let _ = std::fs::remove_dir_all(&source);
    Ok(UpdateReport {
        tool_refreshed: true,
        project,
    })
}

/// The `update` verb's CLI outcome (the lib composes it, `--json` per R-CLI-04).
#[must_use]
pub fn update_outcome(report: &UpdateReport, json_wanted: bool) -> Outcome {
    let mut lines = vec![
        "athena update: tool refreshed from the publish home (fetch + release build + \
         cargo install of athena, athena-api)"
            .to_string(),
    ];
    match &report.project {
        Some(project) => {
            lines.push(format!(
                "athena update: project definitions refreshed under {} (personas/, skills/ only)",
                project.home.display()
            ));
            for action in &project.actions {
                lines.push(format!("  {} — {}", action.kind.label(), action.rel_path));
            }
        }
        None => lines.push(
            "athena update: no .athena/ resolved from the invocation directory — the project \
             definitions face did not run (run `athena init` to introduce a project)"
                .to_string(),
        ),
    }
    if json_wanted {
        let payload = serde_json::json!({
            "tool_refreshed": report.tool_refreshed,
            "project": report.project.as_ref().map(|project| {
                serde_json::json!({
                    "home": project.home.display().to_string(),
                    "actions": project.actions.iter().map(|action| serde_json::json!({
                        "action": action.kind.label(),
                        "path": action.rel_path,
                    })).collect::<Vec<_>>(),
                })
            }),
        });
        return Outcome::ok_json(lines.join("\n"), payload.to_string());
    }
    Outcome::ok(lines.join("\n"))
}

/// The §2.5 installer pipeline's steps against the publish home; returns the fetched source
/// tree (the definitions the project face refreshes from). Each step runs exactly one of the
/// two programs `install.sh` itself runs — `git`, then `cargo` (D-22; the structural test
/// asserts precisely these).
fn refresh_tool() -> Result<PathBuf, UpdateFault> {
    let repo_url = std::env::var("ATHENA_REPO_URL").unwrap_or_else(|_| REPO_URL.to_string());
    let source = std::env::temp_dir().join("athena-update-source");
    if source.exists() {
        std::fs::remove_dir_all(&source).map_err(|error| UpdateFault::Io {
            path: source.display().to_string(),
            detail: error.to_string(),
        })?;
    }
    tool_step(
        "fetch",
        run_git(&[
            "clone",
            "--depth",
            "1",
            repo_url.as_str(),
            source.to_string_lossy().as_ref(),
        ]),
    )?;
    tool_step("build", run_cargo(&["build", "--release"], &source))?;
    tool_step(
        "install-athena",
        run_cargo(&["install", "--path", "crates/athena-cli"], &source),
    )?;
    tool_step(
        "install-athena-api",
        run_cargo(&["install", "--path", "crates/athena-api"], &source),
    )?;
    Ok(source)
}

fn tool_step(step: &'static str, work: Result<(), UpdateFault>) -> Result<(), UpdateFault> {
    work.map_err(|fault| UpdateFault::ToolRefresh {
        step,
        detail: fault.to_string(),
    })
}

/// The pipeline's fetch step: exactly `git clone --depth 1` (D-22).
fn run_git(args: &[&str]) -> Result<(), UpdateFault> {
    let status = std::process::Command::new("git")
        .args(args)
        .status()
        .map_err(|error| UpdateFault::Io {
            path: "git".to_string(),
            detail: error.to_string(),
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(UpdateFault::Io {
            path: "git".to_string(),
            detail: format!("exited with {status}"),
        })
    }
}

/// The pipeline's build/install step: exactly `cargo build --release` / `cargo install --path`
/// (D-22) inside the fetched source tree.
fn run_cargo(args: &[&str], cwd: &Path) -> Result<(), UpdateFault> {
    let status = std::process::Command::new("cargo")
        .args(args)
        .current_dir(cwd)
        .status()
        .map_err(|error| UpdateFault::Io {
            path: "cargo".to_string(),
            detail: error.to_string(),
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(UpdateFault::Io {
            path: "cargo".to_string(),
            detail: format!("exited with {status}"),
        })
    }
}

/// Refresh the project at `home` from the published source tree: walk `personas/` and
/// `skills/` (D-21's exactly-two set), apply the additive-only discipline per file, and
/// rewrite the provenance manifest with the shipped forms this run wrote or proved.
///
/// # Errors
/// [`UpdateFault::Io`] on any filesystem failure (the run aborts; a partial run's manifest
/// records what it did prove, on the next successful write).
pub fn refresh_project(home: &Path, source: &Path) -> Result<ProjectRefresh, UpdateFault> {
    let io = |path: &Path, error: std::io::Error| UpdateFault::Io {
        path: path.display().to_string(),
        detail: error.to_string(),
    };

    let mut actions = Vec::new();
    let mut manifest = read_manifest(home)?;
    for dir_name in DEFINITION_DIRS {
        let src_root = source.join(dir_name);
        if !src_root.is_dir() {
            // No shipped definitions of this kind in the published source: nothing to do.
            continue;
        }
        for (rel, src_path) in collect_rel_files(&src_root, &src_root)? {
            let content =
                std::fs::read_to_string(&src_path).map_err(|error| io(&src_path, error))?;
            let rel_key = format!("{dir_name}/{rel}");
            let target = home.join(dir_name).join(&rel);
            let (kind, wrote) = if !target.exists() {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).map_err(|error| io(parent, error))?;
                }
                std::fs::write(&target, &content).map_err(|error| io(&target, error))?;
                (DefinitionActionKind::Added, true)
            } else {
                let existing =
                    std::fs::read_to_string(&target).map_err(|error| io(&target, error))?;
                if existing == content {
                    // Already the published shipped form — nothing to do (and the manifest
                    // may now carry the proof it lacked).
                    (DefinitionActionKind::UpToDate, true)
                } else if manifest.files.get(&rel_key) == Some(&existing) {
                    // Provably in previously shipped form (the manifest's record): a
                    // definition the operator never modified — refresh it.
                    std::fs::write(&target, &content).map_err(|error| io(&target, error))?;
                    (DefinitionActionKind::Refreshed, true)
                } else {
                    // Operator-modified (or unprovable): never overwritten silently.
                    (DefinitionActionKind::KeptOperator, false)
                }
            };
            record_shipped(&mut manifest, &rel_key, &content, wrote);
            actions.push(DefinitionAction {
                kind,
                rel_path: rel_key.clone(),
            });
        }
    }
    write_manifest(home, &manifest)?;
    Ok(ProjectRefresh {
        home: home.to_path_buf(),
        actions,
    })
}

/// Collect `(relative path, absolute path)` of every file under `root`, recursively and in
/// sorted order (deterministic report and manifest ordering).
fn collect_rel_files(root: &Path, base: &Path) -> Result<Vec<(String, PathBuf)>, UpdateFault> {
    let io = |path: &Path, error: std::io::Error| UpdateFault::Io {
        path: path.display().to_string(),
        detail: error.to_string(),
    };
    let mut out = Vec::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(root)
        .map_err(|error| io(root, error))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            out.extend(collect_rel_files(&path, base)?);
        } else {
            let rel = path
                .strip_prefix(base)
                .map_err(std::io::Error::other)
                .map_err(|error| io(&path, error))?;
            out.push((rel.to_string_lossy().replace('\\', "/"), path));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    /// A temp project home with `var/` (the shape `athena init` leaves).
    struct Fixture {
        home: PathBuf,
        source: PathBuf,
        _keep: tempfile_guard::Guard,
    }

    // The tests need temp directories without a new dependency; a minimal drop guard on a
    // scratch directory under the system temp is enough.
    mod tempfile_guard {
        use std::path::PathBuf;

        pub struct Guard(pub PathBuf);

        impl Drop for Guard {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    fn fixture(label: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!(
            "athena-update-tests-{}-{label}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("home").join("var")).unwrap();
        fs::create_dir_all(root.join("source")).unwrap();
        Fixture {
            home: root.join("home"),
            source: root.join("source"),
            _keep: tempfile_guard::Guard(root),
        }
    }

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    fn action_of(report: &ProjectRefresh, rel: &str) -> DefinitionActionKind {
        report
            .actions
            .iter()
            .find(|action| action.rel_path == rel)
            .unwrap_or_else(|| panic!("no action for {rel}"))
            .kind
    }

    #[test]
    fn an_empty_project_gains_every_shipped_definition() {
        let fx = fixture("empty-gains");
        write(&fx.source.join("personas").join("producer.md"), "V1\n");
        write(&fx.source.join("personas").join("worker.md"), "W1\n");

        let report = refresh_project(&fx.home, &fx.source).unwrap();
        assert_eq!(report.home, fx.home);
        assert_eq!(
            action_of(&report, "personas/producer.md"),
            DefinitionActionKind::Added
        );
        assert_eq!(
            action_of(&report, "personas/worker.md"),
            DefinitionActionKind::Added
        );
        assert_eq!(read(&fx.home.join("personas").join("producer.md")), "V1\n");

        // The manifest records the shipped forms the run wrote — and nothing for skills
        // (the published source carries none).
        let manifest = read_manifest(&fx.home).unwrap();
        assert_eq!(
            manifest
                .files
                .get("personas/producer.md")
                .map(String::as_str),
            Some("V1\n")
        );
        assert!(!manifest.files.contains_key("personas/other.md"));
    }

    #[test]
    fn a_unchanged_project_reports_up_to_date_and_a_changed_shipped_file_refreshes() {
        let fx = fixture("up-to-date-refreshed");
        write(&fx.source.join("personas").join("producer.md"), "V1\n");
        refresh_project(&fx.home, &fx.source).unwrap();

        // A no-change rerun: nothing to do, and the file is untouched.
        let report = refresh_project(&fx.home, &fx.source).unwrap();
        assert_eq!(
            action_of(&report, "personas/producer.md"),
            DefinitionActionKind::UpToDate
        );
        assert_eq!(read(&fx.home.join("personas").join("producer.md")), "V1\n");

        // The shipped content changes: the untouched project file refreshes.
        write(&fx.source.join("personas").join("producer.md"), "V2\n");
        let report = refresh_project(&fx.home, &fx.source).unwrap();
        assert_eq!(
            action_of(&report, "personas/producer.md"),
            DefinitionActionKind::Refreshed
        );
        assert_eq!(read(&fx.home.join("personas").join("producer.md")), "V2\n");
    }

    #[test]
    fn an_operator_modified_definition_is_never_overwritten() {
        let fx = fixture("kept-operator");
        write(&fx.source.join("personas").join("producer.md"), "SHIPPED\n");
        refresh_project(&fx.home, &fx.source).unwrap();

        // The operator changes the project copy…
        write(&fx.home.join("personas").join("producer.md"), "OPERATOR\n");
        // …and the shipped form changes too.
        write(
            &fx.source.join("personas").join("producer.md"),
            "SHIPPED-V2\n",
        );

        let report = refresh_project(&fx.home, &fx.source).unwrap();
        assert_eq!(
            action_of(&report, "personas/producer.md"),
            DefinitionActionKind::KeptOperator,
            "an operator-modified definition must be kept, not overwritten"
        );
        assert_eq!(
            read(&fx.home.join("personas").join("producer.md")),
            "OPERATOR\n"
        );

        // The manifest must not legitimize the operator's content: it still records only
        // the shipped forms, so a later run keeps protecting the file.
        let manifest = read_manifest(&fx.home).unwrap();
        assert_eq!(
            manifest
                .files
                .get("personas/producer.md")
                .map(String::as_str),
            Some("SHIPPED\n")
        );
    }

    #[test]
    fn a_project_without_a_manifest_only_gains_missing_files() {
        let fx = fixture("no-manifest");
        write(&fx.home.join("personas").join("producer.md"), "OPERATOR\n");
        write(&fx.source.join("personas").join("producer.md"), "SHIPPED\n");
        write(&fx.source.join("personas").join("worker.md"), "W\n");

        let report = refresh_project(&fx.home, &fx.source).unwrap();
        assert_eq!(
            action_of(&report, "personas/producer.md"),
            DefinitionActionKind::KeptOperator,
            "without a manifest the present file cannot be proven shipped: kept"
        );
        assert_eq!(
            action_of(&report, "personas/worker.md"),
            DefinitionActionKind::Added
        );
        assert_eq!(
            read(&fx.home.join("personas").join("producer.md")),
            "OPERATOR\n"
        );

        // From here on the manifest carries the proved forms; a later run with unchanged
        // content reports UpToDate for the added file and still keeps the operator's.
        let report = refresh_project(&fx.home, &fx.source).unwrap();
        assert_eq!(
            action_of(&report, "personas/worker.md"),
            DefinitionActionKind::UpToDate
        );
        assert_eq!(
            action_of(&report, "personas/producer.md"),
            DefinitionActionKind::KeptOperator
        );
    }

    #[test]
    fn skills_walking_is_recursive_and_the_empty_skill_set_touches_nothing() {
        let fx = fixture("skills-walk");
        write(&fx.source.join("personas").join("producer.md"), "V1\n");

        let report = refresh_project(&fx.home, &fx.source).unwrap();
        // The shipped skills/ is empty in-tree: no skill action may appear (X-16's note).
        assert!(
            report
                .actions
                .iter()
                .all(|action| action.rel_path.starts_with("personas/"))
        );

        // A nested skill tree lands with its relative path preserved.
        write(
            &fx.source.join("skills").join("demo").join("SKILL.md"),
            "# demo\n",
        );
        let report = refresh_project(&fx.home, &fx.source).unwrap();
        assert_eq!(
            action_of(&report, "skills/demo/SKILL.md"),
            DefinitionActionKind::Added
        );
        assert!(
            fx.home
                .join("skills")
                .join("demo")
                .join("SKILL.md")
                .exists()
        );
    }

    #[test]
    fn the_only_processes_the_verb_spawns_are_the_pipeline_s_git_and_cargo() {
        // D-22's affirmative face: a structural scan of update.rs — every Command::new
        // must name exactly `git` or `cargo` (the §2.5 pipeline's own programs), and never
        // `agent-cli` or anything else. Only the implementation part is scanned (the scan
        // stops at the `#[cfg(test)]` marker, whose own text mentions the needle).
        let whole = include_str!("update.rs");
        let this_file = whole.split("#[cfg(test)]").next().unwrap_or(whole);
        const NEEDLE: &str = "Command::new(";
        let mut spawns = Vec::new();
        let mut rest = this_file;
        while let Some(at) = rest.find(NEEDLE) {
            let after = &rest[at + NEEDLE.len()..];
            let literal = after
                .split_once('"')
                .map(|(_, tail)| tail.split('"').next().unwrap_or("").to_string());
            spawns.push(literal.unwrap_or_default());
            rest = after;
        }
        assert_eq!(
            spawns.len(),
            2,
            "the pipeline's two spawns are the only ones"
        );
        assert!(spawns.contains(&"git".to_string()));
        assert!(spawns.contains(&"cargo".to_string()));
        assert!(
            !this_file.contains("Command::new(\"agent-cli\""),
            "the ATH-CLI-002 ban holds: update never launches agent-cli"
        );
    }
}
