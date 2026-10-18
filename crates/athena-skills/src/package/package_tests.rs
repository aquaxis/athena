//! Tests for the Skill Package model (P5-01).

use super::*;
use athena_types::RiskLevel;

fn manifest_yaml(id: &str) -> String {
    format!(
        r#"
id: {id}
name: Test Skill
version: 0.1.0
description: a test skill
capabilities: []
required_tools:
  - fs_read
required_permissions:
  - fs_read
supported_agents: []
runtime:
  provider: agent-cli
source: local
publisher: local
risk:
  level: low
"#
    )
}

fn make_package(root: &std::path::Path, id: &str, skill_md: &str) -> Package {
    std::fs::create_dir_all(root).expect("dir");
    std::fs::write(root.join("SKILL.md"), skill_md).expect("SKILL.md");
    std::fs::write(root.join("manifest.yaml"), manifest_yaml(id)).expect("manifest");
    Package::open(root).expect("package opens")
}

// --- ATH-SKILL-003: both files, or no package at all --------------------------------------

#[test]
fn a_package_requires_both_files() {
    let root = tempfile_dir();
    let full = make_package(
        &root.join("full"),
        "both-files",
        "the material the agent reads",
    );
    assert!(
        !full.skill_md.is_empty(),
        "SKILL.md's body is carried verbatim"
    );

    // `manifest.yaml` only → not a package.
    std::fs::create_dir_all(root.join("md-only")).expect("dir");
    std::fs::write(root.join("md-only").join("SKILL.md"), "x").expect("md");
    assert!(matches!(
        Package::open(&root.join("md-only")),
        Err(PackageFault::MissingManifest)
    ));

    // Manifest only → not a package.
    std::fs::create_dir_all(root.join("manifest-only")).expect("dir");
    std::fs::write(
        root.join("manifest-only").join("manifest.yaml"),
        manifest_yaml("manifest-only"),
    )
    .expect("manifest");
    assert!(matches!(
        Package::open(&root.join("manifest-only")),
        Err(PackageFault::MissingSkillMd)
    ));

    // Not a directory at all.
    assert!(matches!(
        Package::open(&root.join("absent-directory")),
        Err(PackageFault::MissingRoot(_))
    ));
}

fn tempfile_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "athena-skills-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

// --- The checksum is content-derived and stable -------------------------------------------

#[test]
fn the_content_checksum_binds_both_files() {
    let root = tempfile_dir();
    let package = make_package(&root.join("checksum"), "checksum", "material v1");
    let first = package.content_checksum().expect("checksum");
    assert!(first.starts_with("sha256:"));

    // Same content, dir changed → same checksum; content changed → checksum changes.
    let twin = make_package(&root.join("twin"), "checksum", "material v1");
    assert_eq!(twin.content_checksum().expect("checksum"), first);

    let changed = make_package(&root.join("changed"), "checksum", "material v2");
    assert_ne!(changed.content_checksum().expect("checksum"), first);

    // A manifest-declared checksum wins (it is the Validator's comparison basis).
    let record_fresh = package
        .to_catalog_record(
            SkillState::Available,
            "2026-10-18T00:00:00Z".to_string(),
            "2026-10-18T00:00:00Z".to_string(),
        )
        .expect("record");
    assert_eq!(
        record_fresh.checksum, first,
        "no declared checksum ⇒ the computed one"
    );
}

// --- All fifteen §12.3 fields land on the Catalog record ----------------------------------

#[test]
fn the_catalog_record_carries_all_15_fields() {
    let root = tempfile_dir();
    let dir = root.join("fifteen");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(dir.join("SKILL.md"), "the material the agent reads").expect("md");
    std::fs::write(
        dir.join("manifest.yaml"),
        r#"
id: fpga-timing-analysis
name: FPGA Timing Analysis
version: 1.2.0
description: Analyze FPGA timing violations and identify root causes.
capabilities:
  - fpga
  - sta
required_tools:
  - shell
  - fs_read
required_permissions:
  - fs_read
  - shell
supported_agents:
  - timing-lead
dependencies:
  - id: sdc-analysis
    version: "^1.0.0"
runtime:
  provider: agent-cli
source: https://example.invalid/skills/fpga-timing-analysis
publisher: aquaxis
risk:
  level: medium
"#,
    )
    .expect("manifest");
    let package = Package::open(&dir).expect("opens");
    let record = package
        .to_catalog_record(
            SkillState::Enabled,
            "2026-10-18T00:00:00Z".to_string(),
            "2026-10-18T00:00:01Z".to_string(),
        )
        .expect("record");

    // ATH-SKILL-005: id, name, version, description, capabilities, required_tools,
    // required_permissions, supported_agents, dependencies, runtime, source, checksum,
    // publisher, risk, status — fifteen, all carrying content.
    assert_eq!(record.id.as_str(), "fpga-timing-analysis");
    assert_eq!(record.name, "FPGA Timing Analysis");
    assert_eq!(record.version, "1.2.0");
    assert!(!record.description.is_empty());
    assert_eq!(record.capabilities.len(), 2);
    assert_eq!(record.required_tools.len(), 2);
    assert_eq!(record.required_permissions.len(), 2);
    assert_eq!(record.supported_agents.len(), 1);
    assert_eq!(record.dependencies.len(), 1);
    assert_eq!(record.runtime_provider, "agent-cli");
    assert!(!record.source.is_empty());
    assert!(record.checksum.starts_with("sha256:"));
    assert_eq!(record.publisher, "aquaxis");
    assert_eq!(record.risk_level, RiskLevel::Medium);
    assert_eq!(record.state, SkillState::Enabled);
    assert!(record.state.is_selectable()); // ATH-STATE-008 face
    assert!(!record.hot_reload_safe);
    assert_eq!(record.registered_at, "2026-10-18T00:00:00Z");
}

// --- A declared checksum mismatch is the Validator's (P5-02) comparison basis ------------

#[test]
fn the_record_carries_the_declared_checksum_when_manifest_declares_one() {
    let root = tempfile_dir();
    let dir = root.join("declared");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(dir.join("SKILL.md"), "material").expect("md");
    std::fs::write(
        dir.join("manifest.yaml"),
        r#"
id: declared
name: Declared
version: 1.0.0
description: declares its checksum
capabilities: []
required_tools: []
required_permissions: []
supported_agents: []
runtime:
  provider: agent-cli
source: https://example.invalid/skills/declared
checksum: "sha256:deadbeef"
publisher: aquaxis
risk:
  level: low
"#,
    )
    .expect("manifest");
    let package = Package::open(&dir).expect("opens");
    // The computed checksum must be *different* from the declared one — the Validator
    // refuses a mismatch in P5-02; here the record carries the declared value verbatim.
    assert_ne!(
        package.content_checksum().expect("computed"),
        "sha256:deadbeef"
    );
    let record = package
        .to_catalog_record(SkillState::Available, "t".to_string(), "t".to_string())
        .expect("record");
    assert_eq!(record.checksum, "sha256:deadbeef");
}
