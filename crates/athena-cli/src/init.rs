//! `athena init` (ATH-CLI-009, spec sections 24, 36; tasks P3-07/P3-08).
//!
//! What init does, in the specification's terms:
//!
//! 1. Treats the current directory as the project root and generates `.athena/` with
//!    `config.yaml` (the section-28 defaults), `agents/`, `personas/`, `skills/`, `var/`, and
//!    the template origin `<ATHENA_HOME>/var/agent-cli-template/` (`config.toml` and
//!    `commands/`, ATH-ACLI-013). Only *missing* elements are added to an existing tree; an
//!    existing one is never overwritten (ATH-AIPRJ-006's policy).
//! 2. Writes `var/` into `.athena/.gitignore` so the version-control boundary is one path
//!    (ATH-OPS-002).
//! 3. Expands the `aiprj` convention (`install.sh` expansion: `.aiprj/` **and**
//!    `.agent-cli/`, spec section 2.3) at exactly the two permitted locations —
//!    `<PROJECT_ROOT>` and `<ATHENA_HOME>/var/agent-cli-template/` (ATH-AIPRJ-005). An
//!    existing `instructions.md` is never discarded or replaced (ATH-AIPRJ-007).
//!
//! The expansion content is the vendored `aiprj` template (see `template`); the project-wide
//! `.gitignore` entries that `aiprj install.sh` manages are applied only at the project root,
//! where they have a version-control meaning.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::template;

/// One expansion target inside the project (ATH-AIPRJ-005's two permitted locations).
///
/// Both sites receive the expansion contents, laid out per their roles (spec section 2.3
/// states that an `install.sh` expansion creates `.aiprj/` **and** `.agent-cli/`; the
/// section-4.3.4 tree names what the template origin then holds):
///
/// - at `ExpansionSite::ProjectRoot` the full expansion `.aiprj/` + `.agent-cli/` lands as
///   `install.sh` would lay it out, with the root `.gitignore` block on top — this is the
///   project's own agent-cli workspace, which ATHENA never reads (ATH-ACLI-012);
/// - at `ExpansionSite::TemplateOrigin` the pieces land in the *template* shape the
///   per-Instance generation builds on (ATH-ACLI-013): `config.toml` and `commands/` at the
///   origin's top level — the `.agent-cli/` overlay's contents — plus `.aiprj/`; the
///   root-`.gitignore` block has no meaning under `var/` (ATH-OPS-002 ignores it as a whole).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpansionSite {
    /// `<PROJECT_ROOT>`.
    ProjectRoot,
    /// `<ATHENA_HOME>/var/agent-cli-template/`.
    TemplateOrigin,
}

/// Why init refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitFault {
    /// The project root is not a directory.
    NotADirectory(PathBuf),
    /// Filesystem failure, with the touched path.
    Io { path: String, detail: String },
}

impl fmt::Display for InitFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotADirectory(path) => {
                write!(
                    f,
                    "{} is not a directory; run `athena init` at the project root",
                    path.display()
                )
            }
            Self::Io { path, detail } => write!(f, "filesystem failure on {path}: {detail}"),
        }
    }
}

/// What one init run changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InitReport {
    /// Paths created by this run.
    pub created: Vec<PathBuf>,
    /// Paths already present and left untouched (never overwritten).
    pub kept: Vec<PathBuf>,
}

impl InitReport {
    /**
     * The human summary printed by the CLI.
     *
     * A *creation* is reported as created; *kept* paths are collected by the caller into
     * counts, since a re-run of a fully initialized project must report progress rather than
     * a wall of unchanged names.
     */
    #[must_use]
    pub fn summary(&self) -> String {
        if self.created.is_empty() {
            format!(
                "athena init: nothing to add ({} existing element(s) kept)",
                self.kept.len()
            )
        } else {
            format!(
                "athena init: created {} element(s), kept {} existing element(s)",
                self.created.len(),
                self.kept.len()
            )
        }
    }
}

/// Run `athena init` with `project_root` as the current directory (ATH-CLI-009).
///
/// Idempotent by construction: every element is added only when missing, so a re-run adds
/// nothing and reports the keeps.
///
/// # Errors
/// [`InitFault`] naming the touched path.
pub fn run_init(project_root: &Path) -> Result<InitReport, InitFault> {
    if !project_root.is_dir() {
        return Err(InitFault::NotADirectory(project_root.to_path_buf()));
    }
    let mut report = InitReport::default();
    let home = project_root.join(crate::resolve::HOME_DIR_NAME);

    // --- The `.athena/` tree (ATH-CLI-009) ---
    for dir in [
        home.clone(),
        home.join("agents"),
        home.join("agents").join("producer"),
        home.join("personas"),
        home.join("skills"),
        home.join("var"),
        home.join("var").join("agent-cli-template"),
    ] {
        ensure_dir(&dir, &mut report)?;
    }
    ensure_file(
        &home.join("config.yaml"),
        template::DEFAULT_CONFIG_YAML,
        &mut report,
    )?;

    // The distributed default definitions (spec section 32.1: the repository's
    // `agents/` / `personas/` defaults are copied into the target project).
    ensure_file(
        &home.join("agents").join("producer").join("loop.md"),
        template::AGENT_PRODUCER_LOOP,
        &mut report,
    )?;
    ensure_file(
        &home
            .join("agents")
            .join("producer")
            .join("core-requests.md"),
        template::AGENT_PRODUCER_CORE_REQUESTS,
        &mut report,
    )?;
    ensure_file(
        &home.join("personas").join("producer.md"),
        template::PERSONA_PRODUCER,
        &mut report,
    )?;
    // The level personas (spec section 8.2): one default per hierarchy level below the
    // Core Agent (P2-19). Their `allowed_tools` stay at the minimal working set
    // (`fs_read`/`fs_write`/`send_to` — ATH-SEC-008's minimal-default policy); any
    // broader grant remains the operator's agent-definition decision.
    ensure_file(
        &home.join("personas").join("director.md"),
        template::PERSONA_DIRECTOR,
        &mut report,
    )?;
    ensure_file(
        &home.join("personas").join("lead.md"),
        template::PERSONA_LEAD,
        &mut report,
    )?;
    ensure_file(
        &home.join("personas").join("worker.md"),
        template::PERSONA_WORKER,
        &mut report,
    )?;

    // The version-control boundary is one path (ATH-OPS-002): `var/` ignored inside the
    // `.athena/.gitignore` ATH-CLI-009 writes.
    ensure_gitignore_entries(
        &home.join(".gitignore"),
        &["var/".to_string()],
        "# runtime data is not version-controlled (ATH-OPS-002)\n",
        &mut report,
    )?;

    // The provenance manifest for `athena update` (R-CLI-08, design §2.9/D-21): the shipped
    // forms of the definitions this run wrote — so a later `athena update` can prove a
    // project file unmodified and refresh only those. The manifest lives under `var/`
    // (ATH-OPS-002's write boundary; ignored with it) and is written only when absent:
    // init adds only missing elements, and a re-run must not regenerate a manifest a newer
    // `athena update` run has since built. Only files this run itself wrote are recorded —
    // an existing file could be an operator modification, and the never-overwrite
    // discipline does not let init legitimize it.
    let shipped: &[(&str, &str)] = &[
        ("personas/producer.md", template::PERSONA_PRODUCER),
        ("personas/director.md", template::PERSONA_DIRECTOR),
        ("personas/lead.md", template::PERSONA_LEAD),
        ("personas/worker.md", template::PERSONA_WORKER),
    ];
    let mut manifest = crate::update::read_manifest(&home).map_err(|fault| InitFault::Io {
        path: fault.to_string(),
        detail: "the update manifest read failed".to_string(),
    })?;
    for (key, content) in shipped {
        let target = home.join(key);
        if !target.exists() {
            manifest
                .files
                .insert((*key).to_string(), (*content).to_string());
        }
    }
    let manifest_path = home.join("var").join(crate::update::MANIFEST_NAME);
    if manifest_path.exists() {
        report.kept.push(manifest_path);
    } else {
        crate::update::write_manifest(&home, &manifest).map_err(|fault| InitFault::Io {
            path: fault.to_string(),
            detail: "the update manifest write failed".to_string(),
        })?;
        report.created.push(manifest_path);
    }

    // --- The two `aiprj` expansions (ATH-AIPRJ-005) ---
    expand_aiprj(
        project_root,
        project_root,
        ExpansionSite::ProjectRoot,
        &mut report,
    )?;
    expand_aiprj(
        &home.join("var").join("agent-cli-template"),
        project_root,
        ExpansionSite::TemplateOrigin,
        &mut report,
    )?;

    Ok(report)
}

/// Install the `aiprj` expansion contents at one of the two permitted locations:
/// `.aiprj/` (`instructions.md` + `rules/`) and `.agent-cli/` (`config.toml` +
/// `commands/`), both per the `install.sh` expansion shape (spec section 2.3). An existing
/// `instructions.md` is never discarded or replaced (ATH-AIPRJ-007).
///
/// # Errors
/// [`InitFault`] naming the touched path.
pub fn expand_aiprj(
    target_root: &Path,
    project_root: &Path,
    site: ExpansionSite,
    report: &mut InitReport,
) -> Result<(), InitFault> {
    let io = |path: &Path, error: std::io::Error| -> InitFault {
        InitFault::Io {
            path: path.display().to_string(),
            detail: error.to_string(),
        }
    };

    let aiprj = target_root.join(".aiprj");
    let agent_cli = target_root.join(".agent-cli");
    ensure_dir(&aiprj, report)?;
    ensure_dir(&aiprj.join("rules"), report)?;
    // The `.agent-cli/` pieces land per the site's role (see `ExpansionSite`): as a
    // `.agent-cli/` directory at the project root, as the template origin's top-level
    // `config.toml` + `commands/` (ATH-ACLI-013).
    let (config_target, commands_target) = match site {
        ExpansionSite::ProjectRoot => {
            ensure_dir(&agent_cli, report)?;
            (agent_cli.join("config.toml"), agent_cli.join("commands"))
        }
        ExpansionSite::TemplateOrigin => (
            target_root.join("config.toml"),
            target_root.join("commands"),
        ),
    };

    // `instructions.md`: keep an existing one; promote a seeded `.org` only into the empty
    // case; otherwise write the seed (ATH-AIPRJ-007: the project's own instructions win).
    let instructions = aiprj.join("instructions.md");
    let seed = aiprj.join("instructions.md.org");
    if instructions.exists() {
        report.kept.push(instructions);
    } else if seed.exists() {
        std::fs::rename(&seed, &instructions).map_err(|e| io(&seed, e))?;
        report.created.push(instructions);
    } else {
        std::fs::write(&instructions, template::AIPRJ_INSTRUCTIONS)
            .map_err(|e| io(&instructions, e))?;
        report.created.push(instructions);
    }

    for (name, content) in template::AIPRJ_RULES {
        ensure_file(&aiprj.join("rules").join(name), content, report)?;
    }

    // The `.agent-cli/` overlay: the project's own agent-cli workspace for direct user use
    // at the root (ATHENA never reads it, ATH-ACLI-012); the definition-independent common
    // part of the generation origin at the template (ATH-ACLI-013).
    ensure_file(&config_target, template::AIPRJ_AGENT_CLI_CONFIG, report)?;
    for (name, content) in template::AIPRJ_COMMANDS {
        // ExpandSite reciprocity: the project's own `.agent-cli/` serves a user whose cwd is
        // the project root, so its upstream `@.aiprj/` read directives stay cwd-relative;
        // the template origin serves Instances whose cwd differs every time, so the reads
        // are rewritten to the absolute `<PROJECT_ROOT>/.aiprj/` form (ATH-AIPRJ-028,
        // R-AIP-06). The write targets are untouched — they stay cwd-relative, which is the
        // read/write split that lets the origin be copied to every Instance unmodified
        // (ATH-ACLI-013, design §21.2).
        let content = match site {
            ExpansionSite::ProjectRoot => (*content).to_string(),
            ExpansionSite::TemplateOrigin => rewrite_aiprj_reads(content, project_root),
        };
        ensure_file(&commands_target.join(name), &content, report)?;
    }

    // The project-root `.gitignore` block that `aiprj install.sh` manages (prepend the
    // entries, never duplicating them). The template origin is `var/`-ignored as a whole, so
    // the block is applied only at the project root.
    if site == ExpansionSite::ProjectRoot {
        // `install.sh` keys on the literal `.aiprj` line being present, so the marker joins
        // the entries it manages.
        let entries: Vec<String> = template::AIPRJ_GITIGNORE_ENTRIES
            .lines()
            .map(str::to_string)
            .chain(std::iter::once(".aiprj".to_string()))
            .collect();
        ensure_gitignore_entries(
            &target_root.join(".gitignore"),
            &entries,
            "# aiprj (managed block; entries below are ignored by the aiprj convention)\n",
            report,
        )?;
    }

    Ok(())
}

/// Rewrite the upstream `@.aiprj/` **read directives** into the absolute
/// `<PROJECT_ROOT>/.aiprj/` form (ATH-AIPRJ-028: procedure files reference the conventions
/// and documents by absolute path). Only the `@`-prefixed reads move; cwd-relative write
/// targets stay where upstream carries them.
///
/// `expand_aiprj` applies this to the template origin's `commands/` (see [`ExpansionSite`]).
#[must_use]
pub fn rewrite_aiprj_reads(content: &str, project_root: &Path) -> String {
    let absolute_root = project_root.join(".aiprj");
    content.replace("@.aiprj/", &format!("@{}/", absolute_root.display()))
}

fn io_fault(path: &Path, error: std::io::Error) -> InitFault {
    InitFault::Io {
        path: path.display().to_string(),
        detail: error.to_string(),
    }
}

fn ensure_dir(dir: &Path, report: &mut InitReport) -> Result<(), InitFault> {
    if dir.exists() {
        report.kept.push(dir.to_path_buf());
        return Ok(());
    }
    std::fs::create_dir_all(dir).map_err(|e| io_fault(dir, e))?;
    report.created.push(dir.to_path_buf());
    Ok(())
}

fn ensure_file(file: &Path, content: &str, report: &mut InitReport) -> Result<(), InitFault> {
    if file.exists() {
        // Only missing elements are added; an existing file is never overwritten
        // (ATH-CLI-009 / ATH-AIPRJ-006's policy).
        report.kept.push(file.to_path_buf());
        return Ok(());
    }
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io_fault(parent, e))?;
    }
    std::fs::write(file, content).map_err(|e| io_fault(file, e))?;
    report.created.push(file.to_path_buf());
    Ok(())
}

/// Add the missing `entries` to an existing-or-new `.gitignore` without duplicating them; an
/// existing file's content is preserved (never discarded). `header` annotates an update of an
/// existing file, mirroring `install.sh`'s prepend-then-keep shape.
fn ensure_gitignore_entries(
    file: &Path,
    entries: &[String],
    header: &str,
    report: &mut InitReport,
) -> Result<(), InitFault> {
    let existing = match std::fs::read_to_string(file) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(io_fault(file, error)),
    };
    let missing: Vec<String> = entries
        .iter()
        .filter(|entry| {
            existing
                .as_ref()
                .is_none_or(|text| !text.lines().any(|l| l == *entry))
        })
        .cloned()
        .collect();
    if missing.is_empty() {
        // Nothing missing: the file (if present) is kept untouched.
        return Ok(());
    }
    let mut text = String::new();
    match &existing {
        // `install.sh` prepends its block to an existing `.gitignore`.
        Some(current) => {
            text.push_str(header);
            for entry in &missing {
                text.push_str(entry);
                text.push('\n');
            }
            text.push('\n');
            text.push_str(current);
        }
        None => {
            for entry in entries {
                text.push_str(entry);
                text.push('\n');
            }
        }
    }
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io_fault(parent, e))?;
    }
    std::fs::write(file, &text).map_err(|e| io_fault(file, e))?;
    report.created.push(file.to_path_buf());
    Ok(())
}

#[cfg(test)]
mod tests;
