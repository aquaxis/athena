//! `athena update` — the update face of the installed tool (R-OPS-11 / R-CLI-08; task X-16,
//! corrective X-17; design §2.9 with D-21/D-22/D-24).
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
//!    build-time snapshot), never `config.toml`, `agents/`, or `var/` (`ATH-OPS-02`).
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
//! `config.toml`, `agents/`, `personas/`, and `skills/` receive writes only through an
//! install/update face.
//!
//! Every project-side publication is **atomic and durably proven** (design D-25): a
//! definition file, the intent journal `var/update-journal.json`, and the manifest are
//! staged next to their target, fsynced, and renamed into place — never overwritten in
//! place, so an interruption cannot tear a file or the manifest JSON. Before a definition
//! is published, the run records the intended shipped form in the journal; the next run
//! folds a journaled intent into the manifest only when the file's on-disk content equals
//! the journaled candidate (the disk is the ground truth), and the journal retires only
//! after the manifest is durably published — an interrupted run is recoverable, and a
//! failed run never legitimizes an operator-modified file.
//!
//! The verb is a **tool-side** operation (D-21): no Core API call, no `agent-cli` launch
//! (`ATH-CLI-002` intact), no resident `athena-api` needed. The only processes it ever
//! spawns are the §2.5 pipeline's own — `git` (the fetch) and `cargo` (the build/install) —
//! asserted structurally in `tests/phase6_cli.rs` (D-22).
//!
//! The tool refresh's scratch space is a **fresh, uniquely named directory per run** (design
//! D-24): it never pre-exists, so `athena update` never deletes a path it did not create
//! itself, concurrent update runs cannot destroy each other's trees, and a failed run leaves
//! no fetched source behind — a drop guard owns the tree and removes it on every path
//! (success and every failure), whether or not the project face ran.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::Outcome;
use crate::resolve::resolve_home;

/// The publish home the tool refresh fetches from — the same default the `install.sh`
/// header carries (`R-OPS-10`'s published repository; overridable via `ATHENA_REPO_URL`,
/// mirroring `install.sh`).
pub const REPO_URL: &str = "https://github.com/aquaxis/athena.git";

/// The provenance manifest under `<ATHENA_HOME>/var/` (see the module doc).
pub const MANIFEST_NAME: &str = "update-manifest.json";

/// The intent journal under `<ATHENA_HOME>/var/` (design D-25): before a definition file is
/// published, the run records its intended shipped form here, so a run killed mid-way leaves
/// a recoverable state — the next run folds a landed intent into the provenance manifest and
/// retires the journal only after the manifest is durably published. The wire shape is the
/// manifest's ([`UpdateManifest`]).
pub const JOURNAL_NAME: &str = "update-journal.json";

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
/// on the CLI crate, and the exact shipped bytes are auditable in place. The update intent
/// journal (D-25) shares this wire shape.
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

/// Write the manifest of the project at `home`, creating `var/` when missing. The write is
/// **atomic and durable** (design D-25): the content is staged next to the target, fsynced,
/// and renamed into place — an interruption can never leave a torn or empty manifest file
/// behind (the v0.6.0 review's finding 1, the in-place `std::fs::write`).
pub fn write_manifest(home: &Path, manifest: &UpdateManifest) -> Result<(), UpdateFault> {
    let var = home.join("var");
    std::fs::create_dir_all(&var).map_err(|error| UpdateFault::Io {
        path: var.display().to_string(),
        detail: error.to_string(),
    })?;
    let text = serde_json::to_string_pretty(manifest).map_err(|error| UpdateFault::Io {
        path: var.join(MANIFEST_NAME).display().to_string(),
        detail: error.to_string(),
    })?;
    write_file_atomically(&var.join(MANIFEST_NAME), &text)
}

/// Publish `content` at `path` **atomically and durably** (design D-25): the content is staged
/// in a uniquely named sibling file, written, fsynced, and renamed over the target. A crash
/// can only leave the staged file behind (harmless, never the target torn), never a truncated
/// or empty published file — the v0.6.0 review's finding 1 replaced every in-place
/// `std::fs::write` of a project-side artefact (definitions, journal, manifest) with this.
fn write_file_atomically(path: &Path, content: &str) -> Result<(), UpdateFault> {
    let io = |stage: &Path, step: &'static str, error: std::io::Error| UpdateFault::Io {
        path: stage.display().to_string(),
        detail: format!("{step}: {error}"),
    };
    let parent = path
        .parent()
        .ok_or_else(|| UpdateFault::Io {
            path: path.display().to_string(),
            detail: "no parent directory to stage the write in".to_string(),
        })?
        .to_path_buf();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let stage = parent.join(format!(
        ".{}.{}-{nanos}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file"),
        std::process::id()
    ));
    let mut file = std::fs::File::create(&stage).map_err(|error| io(&stage, "stage", error))?;
    file.write_all(content.as_bytes())
        .map_err(|error| io(&stage, "write", error))?;
    file.sync_all().map_err(|error| fsync_error(path, error))?;
    drop(file);
    std::fs::rename(&stage, path).map_err(|error| {
        // The rename is the only step whose failure leaves the staged file meaningful;
        // reclaim it so a failed publication leaves no staging debris either (a *crash*
        // between write and rename can still leave the staged sibling — harmless by name).
        let _ = std::fs::remove_file(&stage);
        io(&stage, "publish (rename)", error)
    })?;
    Ok(())
}

/// Map a stage/fsync failure onto the *target* path (the published file) — the fault names the
/// path whose content was being made durable, not the transient staging name.
fn fsync_error(target: &Path, error: std::io::Error) -> UpdateFault {
    UpdateFault::Io {
        path: target.display().to_string(),
        detail: format!("fsync: {error}"),
    }
}

/// Publish the run's intent journal (design D-25): record that `intents` are the shipped
/// forms this run intends to land, **before** the corresponding definition files are published.
/// A crash after this point is recoverable: the next run folds a landed intent into the
/// manifest and retires the journal only after the manifest is durably published.
fn write_journal(home: &Path, intents: &UpdateManifest) -> Result<(), UpdateFault> {
    let var = home.join("var");
    std::fs::create_dir_all(&var).map_err(|error| UpdateFault::Io {
        path: var.display().to_string(),
        detail: error.to_string(),
    })?;
    let text = serde_json::to_string_pretty(intents).map_err(|error| UpdateFault::Io {
        path: var.join(JOURNAL_NAME).display().to_string(),
        detail: error.to_string(),
    })?;
    write_file_atomically(&var.join(JOURNAL_NAME), &text)
}

/// Read the intent journal; a missing journal is `None`.
///
/// # Errors
/// [`UpdateFault::Io`] when the journal exists but cannot be read or parsed.
fn read_journal(home: &Path) -> Result<Option<UpdateManifest>, UpdateFault> {
    let path = home.join("var").join(JOURNAL_NAME);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(UpdateFault::Io {
                path: path.display().to_string(),
                detail: error.to_string(),
            });
        }
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|error| UpdateFault::Io {
            path: path.display().to_string(),
            detail: error.to_string(),
        })
}

/// Retire (remove) the journal after the manifest is durably published. Best-effort removal —
/// a leftover journal is safe: the recovery fold only ever copies content the on-disk file
/// already proves landed.
fn remove_journal(home: &Path) {
    let _ = std::fs::remove_file(home.join("var").join(JOURNAL_NAME));
}

/// Fold the previous run's journaled intents into the manifest (design D-25's recovery): a
/// journaled intent lands **only** when the definition file's on-disk content equals the
/// journaled candidate — the disk is the ground truth, the journal only remembers what this
/// face intended to write. A landed intent is folded into the manifest; a not-yet-landed (or
/// since-modified) one is discarded — the file could be mid-write when the previous run died,
/// so trusting it would be claiming an unproven write. Returns the folded manifest and
/// whether anything folded (the journal can then retire).
fn fold_journal(home: &Path, manifest: &mut UpdateManifest) -> Result<bool, UpdateFault> {
    let Some(journal) = read_journal(home)? else {
        return Ok(false);
    };
    let mut folded = false;
    for (rel_key, candidate) in &journal.files {
        let disk = std::fs::read_to_string(home.join(rel_key)).ok();
        if disk.as_deref() == Some(candidate.as_str()) {
            // The journaled write landed; the manifest gains the proof it lacked.
            manifest.files.insert(rel_key.clone(), candidate.clone());
            folded = true;
        }
    }
    Ok(folded)
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
    // The fetched tree is owned by the guard: it is cleaned up whatever the rest of this
    // function does — success, a failed project face, or the unwind of any `?` below (D-24).
    let source = refresh_tool()?;

    // --- Face 2: the project definitions (only when `.athena/` resolves, D-21) ---
    let project = match resolve_home(cwd) {
        Some(home) => Some(refresh_project(&home, source.as_path())?),
        None => None,
    };

    // The fetched source tree was the tool-refresh scratch space; the installed binaries
    // never hold a path back into it, so the guard's best-effort cleanup leaves nothing
    // behind.
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

/// The §2.5 installer pipeline's steps against the publish home; returns the owned fetched
/// source tree (the definitions the project face refreshes from). Each step runs exactly one of
/// the two programs `install.sh` itself runs — `git`, then `cargo` (D-22; the structural test
/// asserts precisely these).
///
/// The fetched tree's workspace is a **uniquely created** directory under the system temp root
/// (D-24): never a fixed name — two concurrent runs cannot collide or destroy each other's tree,
/// and no pre-existing path is ever removed (an unexpected name collision is a refusal, not a
/// deletion). The returned [`FetchedSource`] deletes the tree on every exit path, success or
/// failure.
fn refresh_tool() -> Result<FetchedSource, UpdateFault> {
    let repo_url = std::env::var("ATHENA_REPO_URL").unwrap_or_else(|_| REPO_URL.to_string());
    let source = create_owned_source(&update_source_dir_name())?;
    tool_step(
        "fetch",
        run_git(&[
            "clone",
            "--depth",
            "1",
            repo_url.as_str(),
            source.as_path().to_string_lossy().as_ref(),
        ]),
    )?;
    tool_step(
        "build",
        run_cargo(&["build", "--release"], source.as_path()),
    )?;
    tool_step(
        "install-athena",
        run_cargo(
            &["install", "--path", "crates/athena-cli"],
            source.as_path(),
        ),
    )?;
    tool_step(
        "install-athena-api",
        run_cargo(
            &["install", "--path", "crates/athena-api"],
            source.as_path(),
        ),
    )?;
    Ok(source)
}

/// A unique-on-this-host workspace name for one update run (D-24): the process id plus the
/// wall clock in nanoseconds. Uniqueness is the requirement; the name is minted for the run
/// alone, so another run — or any unrelated directory — is never in the run's removal path.
fn update_source_dir_name() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("athena-update-{}-{nanos}", std::process::id())
}

/// Create the run's workspace under the system temp root as `temp_dir()/name` — owned by the
/// returned [`FetchedSource`]. The creation is exclusive: an already-existing path is a
/// refusal (`D-24` — nothing is removed that this run does not own).
fn create_owned_source(name: &str) -> Result<FetchedSource, UpdateFault> {
    let dir = std::env::temp_dir().join(name);
    match std::fs::create_dir(&dir) {
        Ok(()) => Ok(FetchedSource(dir)),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err(UpdateFault::Io {
            path: dir.display().to_string(),
            detail: "the update workspace name already exists; this run removes nothing it \
                         does not own"
                .to_string(),
        }),
        Err(error) => Err(UpdateFault::Io {
            path: dir.display().to_string(),
            detail: error.to_string(),
        }),
    }
}

/// One update run's fetched source tree, plus its cleanup ownership: the dropped guard removes
/// the directory — best effort — on every exit path, success or failure alike (D-24), so a
/// failed run never leaves fetched source behind.
struct FetchedSource(PathBuf);

impl FetchedSource {
    fn as_path(&self) -> &Path {
        self.0.as_path()
    }
}

impl fmt::Debug for FetchedSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("FetchedSource").field(&self.0).finish()
    }
}

impl Drop for FetchedSource {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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

/// One planned definition publication: discovered during the walk, journaled, then published
/// atomically (design D-25's order — journal first, definitions second, manifest last).
struct PlannedDefinition {
    rel_key: String,
    target: PathBuf,
    content: String,
    kind: DefinitionActionKind,
}

/// Refresh the project at `home` from the published source tree: walk `personas/` and
/// `skills/` (D-21's exactly-two set), apply the additive-only discipline per file, and
/// publish the provenance manifest with the shipped forms this run wrote or proved.
///
/// Every publication follows design D-25 (the v0.6.0 review's finding 1): the run first
/// folds any previous run's landed intents, then journals its own intents
/// (`var/update-journal.json`), lands the definitions atomically, and publishes the manifest
/// last — the journal retires only after the manifest is durably published. A run aborted
/// mid-way therefore leaves a recoverable state, and nothing is ever reported as proven
/// that the disk does not show.
///
/// # Errors
/// [`UpdateFault::Io`] on any filesystem failure (the run aborts; the journal keeps the
/// intent for the next run to finish proving — see the module doc, D-25).
pub fn refresh_project(home: &Path, source: &Path) -> Result<ProjectRefresh, UpdateFault> {
    let io = |path: &Path, error: std::io::Error| UpdateFault::Io {
        path: path.display().to_string(),
        detail: error.to_string(),
    };

    let mut manifest = read_manifest(home)?;

    // Recovery (D-25): a previous run's landed intents become manifest proof now — the disk
    // is the ground truth — and the journal retires once the folded manifest is published.
    if fold_journal(home, &mut manifest)? {
        write_manifest(home, &manifest)?;
    }
    remove_journal(home);

    // Pass 1 — walk the shipped definitions and decide each file's additive-only action.
    let mut plan = Vec::new();
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
            let kind = if !target.exists() {
                DefinitionActionKind::Added
            } else {
                let existing =
                    std::fs::read_to_string(&target).map_err(|error| io(&target, error))?;
                if existing == content {
                    // Already the published shipped form — nothing to publish (and the
                    // manifest may gain the proof it lacked).
                    DefinitionActionKind::UpToDate
                } else if manifest.files.get(&rel_key) == Some(&existing) {
                    // Provably in previously shipped form (the manifest's record): a
                    // definition the operator never modified — refresh it.
                    DefinitionActionKind::Refreshed
                } else {
                    // Operator-modified (or unprovable): never overwritten silently.
                    DefinitionActionKind::KeptOperator
                }
            };
            plan.push(PlannedDefinition {
                rel_key,
                target,
                content,
                kind,
            });
        }
    }

    // Pass 2 — journal the intents first (D-25), then land each one atomically: a crash
    // after this point is recoverable, a crash before it changed nothing on disk.
    let mut intents = UpdateManifest::default();
    for planned in &plan {
        if matches!(
            planned.kind,
            DefinitionActionKind::Added | DefinitionActionKind::Refreshed
        ) {
            intents
                .files
                .insert(planned.rel_key.clone(), planned.content.clone());
        }
    }
    if !intents.files.is_empty() {
        write_journal(home, &intents)?;
    }
    let mut actions = Vec::new();
    for planned in plan {
        if matches!(
            planned.kind,
            DefinitionActionKind::Added | DefinitionActionKind::Refreshed
        ) {
            if planned.kind == DefinitionActionKind::Added
                && let Some(parent) = planned.target.parent()
            {
                std::fs::create_dir_all(parent).map_err(|error| io(parent, error))?;
            }
            write_file_atomically(&planned.target, &planned.content)?;
        }
        record_shipped(
            &mut manifest,
            &planned.rel_key,
            &planned.content,
            planned.kind != DefinitionActionKind::KeptOperator,
        );
        actions.push(DefinitionAction {
            kind: planned.kind,
            rel_path: planned.rel_key,
        });
    }
    write_manifest(home, &manifest)?;
    remove_journal(home);
    Ok(ProjectRefresh {
        home: home.to_path_buf(),
        actions,
    })
}

/// Collect `(relative path, absolute path)` of every file under `root`, recursively and in
/// sorted order (deterministic report and manifest ordering).
///
/// Dot-prefixed names are never walked as definitions: D-25's atomic staging leaves its
/// transient sibling (`.{name}.{pid}-{nanos}.tmp`) only when a run dies between the stage and
/// the rename, and an editor's swap files are dot-prefixed too — none of them is definition
/// material, and walking one would publish staging debris into a project.
fn collect_rel_files(root: &Path, base: &Path) -> Result<Vec<(String, PathBuf)>, UpdateFault> {
    let io = |path: &Path, error: std::io::Error| UpdateFault::Io {
        path: path.display().to_string(),
        detail: error.to_string(),
    };
    let mut out = Vec::new();
    for path in collect_rel_entries(root).map_err(|error| io(root, error))? {
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with('.'))
        {
            continue;
        }
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

/// Enumerate `root`'s directory entries — the exact traversal seam the project face walks
/// (design D-25's finding 2). An entry whose `DirEntry` itself failed (the `read_dir`
/// iteration's `Err` for one entry — e.g. an I/O anomaly mid-enumeration) **propagates** as
/// an `io::Error`; the previous shape silently discarded it with
/// `filter_map(|entry| entry.ok())`, so a walk over a partially failing listing could report
/// success while definitions silently dropped out of the refresh set.
fn collect_rel_entries(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    collect_rel_entries_from(std::fs::read_dir(root)?.map(|entry| entry.map(|entry| entry.path())))
}

/// The testable seam of [`collect_rel_entries`]: fold any iterator of `io::Result` paths —
/// so a synthetic `Err` entry can prove the propagation a real filesystem cannot stage
/// deterministically — returning the paths in sorted order.
fn collect_rel_entries_from<I>(entries: I) -> std::io::Result<Vec<PathBuf>>
where
    I: Iterator<Item = std::io::Result<PathBuf>>,
{
    let mut paths = Vec::new();
    for entry in entries {
        paths.push(entry?);
    }
    paths.sort();
    Ok(paths)
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
    fn a_landed_journal_intent_folds_into_the_manifest_and_the_journal_retires() {
        // D-25's recovery: a run killed after landing its write but before publishing the
        // manifest leaves journal + landed file; the next run folds the landed intent into
        // the manifest (the disk is the ground truth) and retires the journal.
        let fx = fixture("journal-fold-landed");
        write(&fx.home.join("personas").join("producer.md"), "V2\n");
        fs::write(
            fx.home.join("var").join(JOURNAL_NAME),
            r#"{"files":{"personas/producer.md":"V2\n"}}"#,
        )
        .unwrap();

        // The published source happens to match: the fold proves the disk, not the source.
        write(&fx.source.join("personas").join("producer.md"), "V2\n");
        let report = refresh_project(&fx.home, &fx.source).unwrap();
        assert_eq!(
            action_of(&report, "personas/producer.md"),
            DefinitionActionKind::UpToDate
        );

        let manifest = read_manifest(&fx.home).unwrap();
        assert_eq!(
            manifest
                .files
                .get("personas/producer.md")
                .map(String::as_str),
            Some("V2\n"),
            "the landed intent folded into the manifest"
        );
        // The journal retired after the durable manifest publication — and no staging
        // debris of any publication remains.
        let var_entries: Vec<String> = fs::read_dir(fx.home.join("var"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(var_entries, vec![MANIFEST_NAME]);
    }

    #[test]
    fn an_unlanded_journal_intent_is_discarded_without_legitimizing_the_disk() {
        // D-25's refusal face: the journaled candidate never landed the way it was
        // journaled (the write died mid-way, or the operator touched the file since) —
        // the intent is discarded, the journal retires, and the file's operator-owned
        // state stays unlegitimized (the manifest proves nothing it cannot show).
        let fx = fixture("journal-discard-unlanded");
        write(&fx.home.join("personas").join("producer.md"), "OPERATOR\n");
        write_manifest(
            &fx.home,
            &UpdateManifest {
                files: [("personas/producer.md".to_string(), "V1\n".to_string())]
                    .into_iter()
                    .collect(),
            },
        )
        .unwrap();
        fs::write(
            fx.home.join("var").join(JOURNAL_NAME),
            r#"{"files":{"personas/producer.md":"V3\n"}}"#,
        )
        .unwrap();
        write(&fx.source.join("personas").join("producer.md"), "V3\n");

        let report = refresh_project(&fx.home, &fx.source).unwrap();
        assert_eq!(
            action_of(&report, "personas/producer.md"),
            DefinitionActionKind::KeptOperator,
            "an unproven disk form is never overwritten"
        );
        assert_eq!(
            read(&fx.home.join("personas").join("producer.md")),
            "OPERATOR\n"
        );
        let manifest = read_manifest(&fx.home).unwrap();
        assert_eq!(
            manifest
                .files
                .get("personas/producer.md")
                .map(String::as_str),
            Some("V1\n"),
            "the unlanded intent did not reach the manifest"
        );
        assert!(
            !fx.home.join("var").join(JOURNAL_NAME).exists(),
            "the stale (unlanded) intent is retired with the journal"
        );
    }

    #[test]
    fn a_mid_run_definition_failure_keeps_the_journal_and_a_rerun_finishes_the_refresh() {
        // D-25's mid-way face: one definition publishes, a later one fails to land — the
        // journal (written before any landing) survives with both intents, the landed file
        // proves nothing yet (the manifest is untouched), and the rerun folds the landed
        // intent, retires the journal, and finishes what the failed run started.
        let fx = fixture("mid-run-failure-recovers");
        write(&fx.source.join("personas").join("producer.md"), "V1\n");
        write(
            &fx.source.join("skills").join("demo").join("SKILL.md"),
            "# demo\n",
        );
        // `home/skills` is a regular file: landing `skills/demo/SKILL.md` cannot create its
        // parent — a deterministic publication failure (no permission tricks).
        fs::write(fx.home.join("skills"), "not a directory\n").unwrap();

        let fault = refresh_project(&fx.home, &fx.source).expect_err("the landing must fail");
        let UpdateFault::Io { path, detail } = &fault else {
            panic!("unexpected fault shape: {fault:?}");
        };
        // The landing of `skills/demo/SKILL.md` fails while creating its parent (`home/skills`
        // is a regular file) — the fault names the parent, and the detail is that filesystem's.
        assert!(path.contains("skills"), "{path}");
        assert!(
            detail.contains("File exists")
                || detail.contains("Not a directory")
                || detail.contains("not a directory"),
            "{detail}"
        );
        // The first definition landed (walk order) before the failure...
        assert_eq!(read(&fx.home.join("personas").join("producer.md")), "V1\n");
        // ...the journal survived with the run's intents...
        let journal = fs::read_to_string(fx.home.join("var").join(JOURNAL_NAME)).unwrap();
        assert!(journal.contains("personas/producer.md"), "{journal}");
        assert!(journal.contains("skills/demo/SKILL.md"), "{journal}");
        // ...and no manifest was published on the failed run — nothing is proven yet.
        assert!(!fx.home.join("var").join(MANIFEST_NAME).exists());

        // Remove the obstacle; the rerun finishes the refresh from where the failed run
        // stopped.
        fs::remove_file(fx.home.join("skills")).unwrap();
        let report = refresh_project(&fx.home, &fx.source).unwrap();
        assert_eq!(
            action_of(&report, "personas/producer.md"),
            DefinitionActionKind::UpToDate,
            "the landed intent folded — the disk already shows the shipped form"
        );
        assert_eq!(
            action_of(&report, "skills/demo/SKILL.md"),
            DefinitionActionKind::Added
        );
        let manifest = read_manifest(&fx.home).unwrap();
        assert_eq!(
            manifest
                .files
                .get("personas/producer.md")
                .map(String::as_str),
            Some("V1\n")
        );
        assert_eq!(
            manifest
                .files
                .get("skills/demo/SKILL.md")
                .map(String::as_str),
            Some("# demo\n")
        );
        assert!(!fx.home.join("var").join(JOURNAL_NAME).exists());
    }

    #[test]
    fn a_manifest_publication_interrupted_leaves_definitions_public_and_the_journal_recovers() {
        // D-25's manifest face, reproduced deterministically: a run landed its definition
        // (the disk shows the shipped form) but died before the manifest was published —
        // simulated here by replaying exactly that on-disk state (manifest still at the old
        // proof, the landed definition on disk, the journal carrying the landed intent).
        // The definitions stay public (they already are), nothing is proven yet, and the
        // next run folds the landed intent into the manifest and retires the journal.
        let fx = fixture("manifest-write-failure-recovers");
        write(&fx.source.join("personas").join("producer.md"), "V2\n");
        // The previous run landed the definition…
        write(&fx.home.join("personas").join("producer.md"), "V2\n");
        // …its manifest still carries only the older proof…
        write_manifest(
            &fx.home,
            &UpdateManifest {
                files: [("personas/producer.md".to_string(), "V1\n".to_string())]
                    .into_iter()
                    .collect(),
            },
        )
        .unwrap();
        // …and the journaled intent equals what the disk shows: the run died right after
        // the landing, before the manifest publication.
        write_journal(
            &fx.home,
            &UpdateManifest {
                files: [("personas/producer.md".to_string(), "V2\n".to_string())]
                    .into_iter()
                    .collect(),
            },
        )
        .unwrap();

        // The recovery run folds the landed intent (the disk is the ground truth).
        let report = refresh_project(&fx.home, &fx.source).unwrap();
        assert_eq!(
            action_of(&report, "personas/producer.md"),
            DefinitionActionKind::UpToDate,
            "the disk already shows the shipped form; the recovery proves it"
        );
        let manifest = read_manifest(&fx.home).unwrap();
        assert_eq!(
            manifest
                .files
                .get("personas/producer.md")
                .map(String::as_str),
            Some("V2\n"),
            "the landed intent folded into the manifest the publication had missed"
        );
        assert!(!fx.home.join("var").join(JOURNAL_NAME).exists());
    }

    #[test]
    fn a_failed_dir_entry_propagates_instead_of_silently_shrinking_the_refresh_set() {
        // The v0.6.0 review's finding 2: an entry that fails *while being enumerated* used
        // to be dropped with `filter_map(|entry| entry.ok())`, and the run could report
        // success while definitions silently dropped out of the refresh set. The fold of
        // `io::Result` paths propagates the failure instead. Real filesystems cannot stage
        // this deterministically — hence the testable seam.
        let error = std::io::Error::other("the listing broke mid-enumeration");
        let entries: Vec<std::io::Result<PathBuf>> = vec![
            Ok(PathBuf::from("/shipped/personas/producer.md")),
            Err(error),
        ];
        let result = collect_rel_entries_from(entries.into_iter());
        let error = result.expect_err("an entry failure must propagate");
        assert!(error.to_string().contains("listing broke"), "{error}");

        // The all-Ok face returns the paths — sorted (the deterministic walk order).
        let entries: Vec<std::io::Result<PathBuf>> = vec![
            Ok(PathBuf::from("/shipped/b.md")),
            Ok(PathBuf::from("/shipped/a.md")),
        ];
        assert_eq!(
            collect_rel_entries_from(entries.into_iter()).unwrap(),
            vec![
                PathBuf::from("/shipped/a.md"),
                PathBuf::from("/shipped/b.md")
            ]
        );
    }

    #[test]
    fn dot_prefixed_names_are_not_walked_as_definitions() {
        // The atomic staging of D-25 leaves its transient sibling (`.{name}.…tmp`) only if
        // a run dies between the stage and the rename; an editor's swap files are
        // dot-prefixed too. None of them is definition material — the walk skips them, so
        // staging debris is never published into a project.
        let fx = fixture("dotfile-not-walked");
        write(&fx.source.join("personas").join("producer.md"), "V1\n");
        write(
            &fx.source.join("personas").join(".producer.md.swp"),
            "junk\n",
        );

        let report = refresh_project(&fx.home, &fx.source).unwrap();
        assert_eq!(report.actions.len(), 1, "{report:?}");
        assert_eq!(
            action_of(&report, "personas/producer.md"),
            DefinitionActionKind::Added
        );
        assert!(
            !fx.home.join("personas").join(".producer.md.swp").exists(),
            "a dot-prefixed source file is never published"
        );
    }

    #[test]
    fn the_workspace_is_exclusive_and_never_removes_a_foreign_path() {
        // D-24's creation face: exclusive create — an already-existing path at the minted
        // name is a refusal (nothing is removed), and a successful create is owned.
        let name = format!("athena-update-own-{}-x", std::process::id());
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join(&name));

        let first = create_owned_source(&name).expect("the minted name is free");
        assert!(first.as_path().is_dir());

        let second = create_owned_source(&name).expect_err("the existing path is a refusal");
        match &second {
            UpdateFault::Io { path, detail } => {
                assert!(path.ends_with(&name), "{path}");
                assert!(detail.contains("removes nothing"), "{detail}");
            }
            other => panic!("unexpected fault shape: {other:?}"),
        }

        // The foreign path survived the refused run (it is not this run's to delete).
        assert!(std::env::temp_dir().join(&name).is_dir());

        // Disowning removes exactly the owned tree, and only then.
        drop(first);
        assert!(!std::env::temp_dir().join(&name).exists());
    }

    #[test]
    fn the_workspace_name_is_minted_per_run() {
        // Two consecutive mints differ (pid + nanos), so no run reuses a former run's
        // workspace name on the same host.
        let first = update_source_dir_name();
        std::thread::sleep(std::time::Duration::from_nanos(1));
        let second = update_source_dir_name();
        assert_ne!(first, second, "{first} vs {second}");
        assert!(first.starts_with("athena-update-"), "{first}");
    }

    #[test]
    fn the_fetched_source_guard_cleans_up_on_every_exit_path() {
        // D-24's failure face: the guard — the same ownership `run_update` holds across the
        // project face and every `?` — removes the workspace when dropped, so a run that
        // fails anywhere after the fetch leaves no fetched source behind.
        let name = update_source_dir_name();
        let source = create_owned_source(&name).unwrap();
        let path = source.as_path().to_path_buf();
        fs::create_dir_all(path.join("personas")).unwrap();
        fs::write(path.join("personas").join("producer.md"), "fetched\n").unwrap();
        drop(source);
        assert!(
            !path.exists(),
            "the guard removes the fetched tree on every exit path"
        );
    }

    #[test]
    fn a_colliding_workspace_name_refuses_and_never_removes_the_existing_path() {
        // D-24's no-deletion face: a name that already exists is a refusal — whatever sits
        // there (another run's tree, an unrelated directory) survives untouched.
        let name = update_source_dir_name();
        let dir = std::env::temp_dir().join(&name);
        fs::create_dir_all(&dir).unwrap();
        let _keep = tempfile_guard::Guard(dir.clone());
        let sentinel = dir.join("sentinel.txt");
        fs::write(&sentinel, "not ours\n").unwrap();

        let refused = match create_owned_source(&name) {
            Err(fault) => fault,
            Ok(_) => panic!("a pre-existing workspace name must refuse, not be removed"),
        };
        let UpdateFault::Io { detail, .. } = refused else {
            panic!("the collision refusal is an Io fault, got {refused:?}");
        };
        assert!(detail.contains("already exists"), "{detail}");
        assert!(
            dir.exists() && sentinel.exists(),
            "the existing path survives the refusal untouched"
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
