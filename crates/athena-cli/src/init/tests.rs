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
        home.join("config.yaml"),
        home.join("agents").join("producer").join("loop.md"),
        home.join("agents")
            .join("producer")
            .join("core-requests.md"),
        home.join("personas").join("producer.md"),
        home.join("personas").join("director.md"),
        home.join("personas").join("lead.md"),
        home.join("personas").join("worker.md"),
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
    let config = root.join(".athena").join("config.yaml");
    fs::write(&config, "operator's configuration").expect("mutate");
    let instructions = root.join(".aiprj").join("instructions.md");
    fs::write(&instructions, "the project's own instructions").expect("mutate");
    let persona = root.join(".athena").join("personas").join("producer.md");
    fs::write(&persona, "operator's persona").expect("mutate");

    let report = run_init(&root).expect("second");
    assert_eq!(
        read(&config),
        "operator's configuration",
        "an existing config.yaml is never overwritten (ATH-CLI-009)"
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
        read(&home.join("personas").join("producer.md")),
        template::PERSONA_PRODUCER,
        "the persona default is the repository's producer persona"
    );
    assert_eq!(
        read(&home.join("personas").join("director.md")),
        template::PERSONA_DIRECTOR,
        "the persona default is the repository's director persona (P2-19)"
    );
    assert_eq!(
        read(&home.join("personas").join("lead.md")),
        template::PERSONA_LEAD,
        "the persona default is the repository's lead persona (P2-19)"
    );
    assert_eq!(
        read(&home.join("personas").join("worker.md")),
        template::PERSONA_WORKER,
        "the persona default is the repository's worker persona (P2-19)"
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
