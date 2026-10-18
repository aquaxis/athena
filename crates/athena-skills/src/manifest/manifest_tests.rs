//! Tests for the manifest parser (P5-01).

use super::*;

const SPEC_EXAMPLE: &str = r#"
id: fpga-timing-analysis
name: FPGA Timing Analysis
version: 1.2.0
description: Analyze FPGA timing violations and identify root causes.

capabilities:
  - fpga
  - timing-analysis
  - sta
  - sdc

required_tools:
  - shell
  - fs_read
  - fs_write

required_permissions:
  - fs_read
  - fs_write
  - shell

supported_agents:
  - timing-lead
  - verification-agent

dependencies:
  - id: sdc-analysis
    version: "^1.0.0"

runtime:
  provider: agent-cli

source: https://example.invalid/skills/fpga-timing-analysis
checksum: "sha256:..."
publisher: aquaxis
risk:
  level: medium
"#;

// --- The §12.3 example parses end to end -------------------------------------------------

#[test]
fn the_12_3_example_manifest_parses_whole() {
    let manifest = Manifest::parse(SPEC_EXAMPLE).expect("the §12.3 example parses");
    assert_eq!(manifest.id, "fpga-timing-analysis");
    assert_eq!(manifest.version, "1.2.0");
    assert_eq!(manifest.capabilities.len(), 4);
    assert_eq!(manifest.required_tools.len(), 3);
    assert_eq!(manifest.supported_agents.len(), 2);
    assert_eq!(manifest.runtime.provider, "agent-cli");
    assert_eq!(manifest.publisher, "aquaxis");
    assert!(manifest.dependencies.contains(&Dependency {
        id: "sdc-analysis".into(),
        version: "^1.0.0".into(),
    }));
    assert!(
        !manifest.hot_reload_safe,
        "undeclared means not hot-reload-safe"
    );
}

#[test]
fn the_tool_and_permission_shapes_resolve() {
    let manifest = Manifest::parse(SPEC_EXAMPLE).expect("parses");
    let tools = manifest.tool_names().expect("tools");
    assert_eq!(
        tools,
        vec![
            athena_types::ToolName::Shell,
            athena_types::ToolName::FsRead,
            athena_types::ToolName::FsWrite
        ]
    );
    let permissions = manifest.permissions().expect("permissions");
    for permission in permissions {
        assert!(matches!(permission, Permission::Tool { .. }));
    }
}

// --- Structured permission forms ---------------------------------------------------------

#[test]
fn structured_permission_forms_deserialize() {
    let from = "required_permissions:\n  - fs_read\n  - fs_write\n  - shell";
    let to = "required_permissions:\n  - fs:\n      mode: read\n      scope: /workspace\n  - network:\n      mode: allowlist\n      hosts: [\"example.invalid\"]\n  - process:";
    let yaml = SPEC_EXAMPLE.replace(from, to);
    let manifest = Manifest::parse(&yaml).expect("structured manifest parses");
    let permissions = manifest.permissions().expect("resolve");
    assert!(matches!(
        &permissions[0],
        Permission::Fs { mode: athena_types::FsMode::Read, scope } if scope == "/workspace"
    ));
    assert!(matches!(
        &permissions[1],
        Permission::Network { mode: athena_types::NetworkMode::Allowlist, hosts }
            if hosts.as_ref().is_some_and(|hosts| hosts.len() == 1)
    ));
    assert!(matches!(permissions[2], Permission::Process));
}

// --- The refusals ------------------------------------------------------------------------

#[test]
fn every_blank_required_field_is_refused_by_name() {
    let cases: [(String, &str); 6] = [
        (
            SPEC_EXAMPLE.replace("id: fpga-timing-analysis", "id: \"\""),
            "id",
        ),
        (
            SPEC_EXAMPLE.replace("name: FPGA Timing Analysis", "name: \"\""),
            "name",
        ),
        (
            SPEC_EXAMPLE.replace("version: 1.2.0", "version: \"\""),
            "version",
        ),
        (
            SPEC_EXAMPLE.replace(
                "description: Analyze FPGA timing violations and identify root causes.",
                "description: \"\"",
            ),
            "description",
        ),
        (
            SPEC_EXAMPLE.replace(
                "source: https://example.invalid/skills/fpga-timing-analysis",
                "source: \"\"",
            ),
            "source",
        ),
        (
            SPEC_EXAMPLE.replace("publisher: aquaxis", "publisher: \"\""),
            "publisher",
        ),
    ];
    for (yaml, field) in cases {
        let fault = Manifest::parse(&yaml).expect_err("blank refused");
        assert!(format!("{fault:?}").contains(field), "{field}: {fault:?}");
    }
}

#[test]
fn a_non_semver_version_is_refused() {
    let yaml = SPEC_EXAMPLE.replace("version: 1.2.0", "version: beta");
    assert!(matches!(
        Manifest::parse(&yaml),
        Err(ManifestFault::BadVersion(_))
    ));
    // And the shape test itself:
    assert!(Manifest::is_semver("1.2.0"));
    assert!(Manifest::is_semver("1.2.0-rc.1"));
    assert!(Manifest::is_semver("1.2.0+build.3"));
    assert!(!Manifest::is_semver("1.2"));
    assert!(!Manifest::is_semver("1.x.0"));
    assert!(Manifest::is_range("^1.0.0"));
    assert!(Manifest::is_range("~2.3"));
    assert!(Manifest::is_range(">=0.9.0"));
}

#[test]
fn an_unknown_tool_is_refused() {
    let yaml = SPEC_EXAMPLE.replace(
        "required_tools:\n  - shell\n  - fs_read\n  - fs_write",
        "required_tools:\n  - telepath",
    );
    let fault = Manifest::parse(&yaml).expect_err("parse-time validation refuses");
    assert!(matches!(fault, ManifestFault::UnknownTool(ref tool) if tool == "telepath"));
}

#[test]
fn an_unknown_risk_level_is_refused() {
    let yaml = SPEC_EXAMPLE.replace("level: medium", "level: whatever");
    assert!(Manifest::parse(&yaml).is_err());
}

#[test]
fn a_malformed_dependency_is_refused() {
    for bad in ["", "latest", "1"] {
        let yaml = SPEC_EXAMPLE.replace("version: \"^1.0.0\"", &format!("version: \"{bad}\""));
        assert!(
            matches!(Manifest::parse(&yaml), Err(ManifestFault::BadDependency(_))),
            "dependency {bad:?} refused"
        );
    }
}

#[test]
fn an_unknown_manifest_key_is_refused_silently_never() {
    let yaml = format!("{SPEC_EXAMPLE}\nhot_relaod_safe: true");
    assert!(
        Manifest::parse(&yaml).is_err(),
        "a typo'd key must not silently lose a metadata field"
    );
}
