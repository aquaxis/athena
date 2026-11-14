//! `athena init` behaviour tests (tasks P3-07/P3-08).

use std::fs;
use std::path::{Path, PathBuf};

use super::run_init;
use crate::template;

fn tmp_dir(name: &str) -> PathBuf {
    let base = std::env::temp_dir().join("athena-cli-init-tests");
    let dir = base.join(format!("{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn assert_all_present(paths: &[PathBuf], label: &str) {
    for path in paths {
        assert!(
            path.is_dir() || path.is_file(),
            "expected {label} {}",
            path.display()
        );
    }
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).expect("read file")
}

#[test]
fn a_fresh_init_creates_the_full_athena_and_aiprj_tree() {
    let root = tmp_dir("fresh");
    let report = run_init(&root).expect("fresh init");
    let home = root.join(".athena");
    let template_origin = home.join("var").join("agent-cli-template");

    let dirs = vec![
        home.join("agents"),
        home.join("agents").join("producer"),
        home.join("personas"),
        home.join("skills"),
        home.join("var"),
        template_origin.join(".aiprj"),
        template_origin.join("commands"),
        // The two aiprj expansion locations (ATH-AIPRJ-005).
        root.join(".aiprj"),
        root.join(".aiprj").join("rules"),
        root.join(".agent-cli"),
        root.join(".agent-cli").join("commands"),
        template_origin.join(".aiprj").join("rules"),
    ];
    assert_all_present(&dirs, "directory");

    let files = vec![
        home.join("config.toml"),
        home.join("agents").join("producer").join("loop.md"),
        home.join("agents")
            .join("producer")
            .join("core-requests.md"),
        home.join("personas").join("producer_ja.md"),
        home.join("personas").join("director_ja.md"),
        home.join("personas").join("lead_ja.md"),
        home.join("personas").join("worker_ja.md"),
        home.join(".gitignore"),
        root.join(".aiprj").join("instructions.md"),
        root.join(".aiprj").join("rules").join("exec_job.md"),
        root.join(".aiprj").join("rules").join("close_ai.md"),
        root.join(".agent-cli").join("config.toml"),
        root.join(".agent-cli").join("commands").join("ai.md"),
        root.join(".agent-cli").join("commands").join("next_ai.md"),
        template_origin.join("config.toml"),
        template_origin.join("commands").join("setup_ai.md"),
    ];
    assert_all_present(&files, "file");

    // The `.gitignore` split: the aiprj block at the project root, `var/` inside
    // `.athena/.gitignore` (ATH-OPS-002).
    let root_gitignore = read(&root.join(".gitignore"));
    assert!(root_gitignore.lines().any(|l| l == ".aiprj"));
    assert!(root_gitignore.lines().any(|l| l == "AI_LOG/"));
    let home_gitignore = read(&home.join(".gitignore"));
    assert!(home_gitignore.lines().any(|l| l == "var/"));
    // The aiprj block never leaks into the ATHENA home's file.
    assert!(!home_gitignore.lines().any(|l| l == ".aiprj"));

    // The summary reports creations.
    assert!(!report.created.is_empty());
    assert!(report.summary().contains("created"));
}

#[test]
fn a_re_run_adds_only_missing_elements_and_overwrites_nothing() {
    let root = tmp_dir("rerun");
    run_init(&root).expect("first");

    // Mutate files an init must never overwrite.
    let config = root.join(".athena").join("config.toml");
    fs::write(&config, "operator's configuration").expect("mutate");
    let instructions = root.join(".aiprj").join("instructions.md");
    fs::write(&instructions, "the project's own instructions").expect("mutate");
    let persona = root.join(".athena").join("personas").join("producer_ja.md");
    fs::write(&persona, "operator's persona").expect("mutate");

    let report = run_init(&root).expect("second");
    assert_eq!(
        read(&config),
        "operator's configuration",
        "an existing config.toml is never overwritten (ATH-CLI-009)"
    );
    assert_eq!(
        read(&instructions),
        "the project's own instructions",
        "an existing instructions.md is never discarded (ATH-AIPRJ-007)"
    );
    assert_eq!(read(&persona), "operator's persona");

    // Everything is already present: nothing created, all kept.
    assert!(report.created.is_empty(), "a re-run must create nothing");
    assert!(!report.kept.is_empty());
    assert!(report.summary().contains("nothing to add"));
}

#[test]
fn a_seeded_org_file_is_promoted_but_an_existing_instructions_md_wins() {
    let root = tmp_dir("promote");
    fs::create_dir_all(root.join(".aiprj")).unwrap();
    fs::write(
        root.join(".aiprj").join("instructions.md.org"),
        "seeded project policy",
    )
    .expect("seed");

    run_init(&root).expect("promote");
    // `install.sh` promotes `.org` into place; the template origin is untouched by it.
    assert!(!root.join(".aiprj").join("instructions.md.org").exists());
    assert_eq!(
        read(&root.join(".aiprj").join("instructions.md")),
        "seeded project policy"
    );

    // The template origin independently seeds its own instructions.
    let template_instructions = root
        .join(".athena")
        .join("var")
        .join("agent-cli-template")
        .join(".aiprj")
        .join("instructions.md");
    assert_eq!(read(&template_instructions), template::AIPRJ_INSTRUCTIONS);
}

#[test]
fn aiprj_is_expanded_at_exactly_the_two_permitted_locations() {
    let root = tmp_dir("two-locations");
    run_init(&root).expect("init");
    let home = root.join(".athena");

    // The two locations (ATH-AIPRJ-005).
    assert!(
        root.join(".agent-cli")
            .join("commands")
            .join("ai.md")
            .is_file()
    );
    assert!(
        home.join("var")
            .join("agent-cli-template")
            .join("config.toml")
            .is_file()
    );
    // No instance-shaped location is ever created by init.
    assert!(
        !home.join("var").join("instances").exists(),
        "init must not create instance workspaces"
    );
}

#[test]
fn template_and_default_definitions_carry_the_repository_defaults() {
    let root = tmp_dir("defaults");
    run_init(&root).expect("init");
    let home = root.join(".athena");
    assert_eq!(
        read(&home.join("personas").join("producer_ja.md")),
        template::PERSONA_PRODUCER,
        "the persona default is the repository's Japanese producer persona (X-59)"
    );
    assert_eq!(
        read(&home.join("personas").join("director_ja.md")),
        template::PERSONA_DIRECTOR,
        "the persona default is the repository's Japanese director persona (P2-19)"
    );
    assert_eq!(
        read(&home.join("personas").join("lead_ja.md")),
        template::PERSONA_LEAD,
        "the persona default is the repository's Japanese lead persona (P2-19)"
    );
    assert_eq!(
        read(&home.join("personas").join("worker_ja.md")),
        template::PERSONA_WORKER,
        "the persona default is the repository's Japanese worker persona (P2-19)"
    );
    assert_eq!(
        read(
            &home
                .join("agents")
                .join("producer")
                .join("core-requests.md")
        ),
        template::AGENT_PRODUCER_CORE_REQUESTS
    );
    // The `.agent-cli` common part is the vendored aiprj overlay, unmodified.
    let template_config = home
        .join("var")
        .join("agent-cli-template")
        .join("config.toml");
    assert_eq!(read(&template_config), template::AIPRJ_AGENT_CLI_CONFIG);
}

#[test]
fn procedure_reads_are_absolute_at_the_template_and_cwd_relative_at_the_project_root() {
    // ATH-AIPRJ-028 / R-AIP-06: the Instance-read procedure files reference the conventions
    // by absolute path, while the project's own `.agent-cli/` (ATHENA never reads it,
    // ATH-ACLI-012) keeps upstream's cwd-relative form for a user running at the project
    // root. Each copy is verbatim from its origin (ATH-ACLI-013, design §21.2).
    let root = tmp_dir("reads");
    run_init(&root).expect("init");
    let home = root.join(".athena");

    let project_ai = read(&root.join(".agent-cli").join("commands").join("ai.md"));
    assert_eq!(
        project_ai.trim(),
        "@.aiprj/rules/exec_job.md",
        "the project copy keeps the cwd-relative read a user at the project root needs"
    );

    let template_ai = read(
        &home
            .join("var")
            .join("agent-cli-template")
            .join("commands")
            .join("ai.md"),
    );
    assert_eq!(
        template_ai.trim(),
        format!("@{}/.aiprj/rules/exec_job.md", root.display()),
        "the template origin carries the absolute read every Instance cwd needs"
    );

    let template_setup = read(
        &home
            .join("var")
            .join("agent-cli-template")
            .join("commands")
            .join("setup_ai.md"),
    );
    assert_eq!(
        template_setup.trim(),
        format!("@{}/.aiprj/rules/setup_project.md", root.display())
    );

    // A wrapper without a read directive is carried verbatim.
    let next = read(
        &home
            .join("var")
            .join("agent-cli-template")
            .join("commands")
            .join("next_ai.md"),
    );
    assert_eq!(next.trim(), "Go next job");
}

/// A fresh `athena init` installs **every** shipped definition, not just the four level
/// personas (spec section 32.1: the repository's `personas/` and `skills/` are the defaults
/// init copies into the target project).
///
/// This is the regression the operator reported: a fresh project used to receive the four
/// personas and nothing else, so every Skill Package and every non-level persona arrived
/// only on a later `athena update` — which fetches the *published* repository, i.e. requires
/// network access and a release that already carries them. Init must stand alone.
#[test]
fn a_fresh_init_installs_every_shipped_persona_and_skill_package() {
    let root = tmp_dir("all-definitions");
    run_init(&root).expect("init");
    let home = root.join(".athena");

    // The embedded table is the repository's two directories, so the assertion is
    // structural: every entry must land, byte-identical. The local binding keeps the
    // read through the table's own face (clippy's `const_is_empty` folds the constant
    // into a false positive on the toolchain's face — the build script emits the 31
    // entries in every build tree, and the assertion holds at run time).
    let embedded = template::EMBEDDED_DEFINITIONS;
    assert!(
        !embedded.is_empty(),
        "the build script must embed the repository's definitions"
    );
    for (rel_key, content) in template::EMBEDDED_DEFINITIONS {
        let target = home.join(rel_key);
        assert!(
            target.is_file(),
            "a fresh init must install {rel_key} ({} embedded)",
            template::EMBEDDED_DEFINITIONS.len()
        );
        assert_eq!(
            read(&target),
            *content,
            "{rel_key} must be byte-identical to the shipped definition"
        );
    }

    // The concrete shape the operator asked for: every Skill Package is a complete package
    // (`SKILL.md` + `manifest.yaml`, ATH-SKILL-003), and the PCB family's nine personas are
    // present.
    let mut packages = 0;
    for entry in fs::read_dir(home.join("skills")).expect("skills dir") {
        let path = entry.expect("entry").path();
        if !path.is_dir() {
            continue;
        }
        packages += 1;
        assert!(
            path.join("SKILL.md").is_file(),
            "{}: no SKILL.md",
            path.display()
        );
        assert!(
            path.join("manifest.yaml").is_file(),
            "{}: no manifest.yaml",
            path.display()
        );
    }
    assert_eq!(
        packages, 9,
        "the nine shipped Skill packages install at init"
    );

    for slug in [
        "pcb-orchestrator",
        "pcb-grand-design",
        "pcb-system-design",
        "pcb-schematic-design",
        "pcb-artwork-design",
        "pcb-si-analysis",
        "pcb-pi-analysis",
        "pcb-parts-research",
        "pcb-common-operator",
    ] {
        assert!(
            home.join("personas")
                .join(format!("{slug}_ja.md"))
                .is_file(),
            "the PCB persona {slug} installs at init"
        );
    }

    // Idempotency is unchanged: a re-run installs nothing new and keeps operator edits.
    let report = run_init(&root).expect("re-run");
    assert!(report.created.is_empty(), "a re-run creates nothing");
}

/// The shipped configuration's commented examples must parse when an operator follows them.
///
/// The `agents` example used to span two lines inside an inline table — a form TOML forbids
/// — so uncommenting it exactly as shipped failed the loader with `invalid inline table`
/// before the resident ever reached its first read. An example is documentation that an
/// operator executes, so it is held to the loader's own grammar here rather than by review.
#[test]
fn every_commented_example_in_the_shipped_config_parses_as_toml() {
    /// Does the uncommented line open a TOML example — a table header, or an assignment
    /// whose key is a bare identifier? The field *documentation* lines share the same
    /// comment indent ("id    the slug …"), so a key followed by prose must not qualify.
    fn opens_an_example(text: &str) -> bool {
        if text.starts_with('[') {
            return true;
        }
        let Some((key, value)) = text.split_once('=') else {
            return false;
        };
        let key = key.trim();
        !key.is_empty()
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            && !value.trim().is_empty()
    }

    let mut blocks: Vec<String> = Vec::new();
    let mut current: Option<Vec<String>> = None;
    for line in crate::template::DEFAULT_CONFIG_TOML.lines() {
        match line.strip_prefix("#   ") {
            Some(text) if current.is_some() => current.as_mut().expect("open").push(text.into()),
            Some(text) if opens_an_example(text) => current = Some(vec![text.into()]),
            _ => {
                if let Some(block) = current.take() {
                    blocks.push(block.join("\n"));
                }
            }
        }
    }
    if let Some(block) = current.take() {
        blocks.push(block.join("\n"));
    }

    assert!(
        blocks.len() >= 2,
        "the shipped config documents at least the inline-table and the table-array agent \
         example; found {}: {blocks:?}",
        blocks.len()
    );
    for block in &blocks {
        assert!(
            toml::from_str::<toml::Value>(block).is_ok(),
            "a commented example does not parse as TOML, so following it breaks the loader:\n{block}"
        );
    }
}
