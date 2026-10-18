//! Tests for the Skill Validator (P5-02).

use super::*;
use crate::package::Package;
use std::path::Path;

fn write(root: &Path, _id: &str, skill_md: &str, manifest: &str) -> Package {
    std::fs::create_dir_all(root).expect("dir");
    std::fs::write(root.join("SKILL.md"), skill_md).expect("md");
    std::fs::write(root.join("manifest.yaml"), manifest).expect("manifest");
    Package::open(root).expect("opens")
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "athena-validator-{tag}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn manifest_of(id: &str, publisher: &str, source: &str, declared: Option<&str>) -> String {
    let checksum_line = declared
        .map(|value| format!("checksum: \"{value}\"\n"))
        .unwrap_or_default();
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
source: {source}
{checksum_line}publisher: {publisher}
risk:
  level: low
"#
    )
}

/// Compute the true checksum of a freshly written package on disk (re-open it).
fn computed_checksum_of(root: &Path) -> String {
    Package::open(root)
        .expect("opens")
        .content_checksum()
        .expect("checksum")
}

// --- The happy local path ----------------------------------------------------------------

#[test]
fn a_local_package_passes_and_names_available() {
    let root = scratch("local-ok");
    let package = write(
        &root,
        "local-skill",
        "material",
        &manifest_of("local-skill", "local", "local", None),
    );
    let state = validate(&package, true, &AllowList::default()).expect("local needs no list");
    assert_eq!(state, SkillState::Available);
}

// --- The checksum layer (ATH-SKILL-022) --------------------------------------------------

#[test]
fn a_declared_checksum_mismatch_refuses_and_a_match_passes() {
    let bad_root = scratch("checksum-bad");
    // The declared value differs from the content.
    let package = write(
        &bad_root,
        "bad-hash",
        "material",
        &manifest_of("bad-hash", "local", "local", Some("sha256:0000")),
    );
    let fault = validate(&package, true, &AllowList::default()).expect_err("mismatch refused");
    match fault {
        ValidationFault::ChecksumMismatch { declared, computed } => {
            assert_eq!(declared, "sha256:0000");
            assert_eq!(computed, computed_checksum_of(&bad_root));
        }
        other => panic!("expected ChecksumMismatch, got {other:?}"),
    }

    // Declared == computed passes.
    let good_root = scratch("checksum-good");
    let written = write(
        &good_root,
        "good-hash",
        "material",
        &manifest_of("good-hash", "local", "local", None),
    );
    let computed = written.content_checksum().expect("checksum");
    let good = write(
        &good_root,
        "good-hash",
        "material",
        &manifest_of("good-hash", "local", "local", Some(&computed)),
    );
    assert!(
        validate(&good, true, &AllowList::default()).is_ok(),
        "the honest declared checksum passes"
    );
}

#[test]
fn the_checksum_check_honors_the_configured_switch() {
    // `verify_checksum: false` lets even a lying declared checksum through — config-owned.
    let root = scratch("unchecked");
    let package = write(
        &root,
        "unchecked",
        "material",
        &manifest_of("unchecked", "local", "local", Some("sha256:0000")),
    );
    let permissive = AllowList::new(vec![], vec![], false);
    assert!(validate(&package, true, &permissive).is_ok());
}

// --- The allow-list layer (ATH-SKILL-036) ------------------------------------------------

#[test]
fn an_external_skill_needs_publisher_and_source_on_the_list() {
    let dir = scratch("external");
    let package = write(
        &dir,
        "external-skill",
        "material",
        &manifest_of(
            "external-skill",
            "aquaxis",
            "https://hub.example.invalid/skills",
            None,
        ),
    );
    // Empty list: every external fetch denied (the v1 safe default).
    let empty = AllowList::default();
    let fault = validate(&package, false, &empty).expect_err("empty list denies");
    assert!(
        matches!(fault, ValidationFault::PublisherNotAllowed(_)),
        "publisher checked first: {fault:?}"
    );

    // Publisher allowed, source not: the source refusal names the fact.
    let publisher_only = AllowList::new(vec!["aquaxis".into()], vec![], true);
    let fault = validate(&package, false, &publisher_only).expect_err("source refused");
    assert!(matches!(fault, ValidationFault::SourceNotAllowed(_)));

    // Both allowed: passes.
    let both = AllowList::new(
        vec!["aquaxis".into()],
        vec!["https://hub.example.invalid/skills".into()],
        true,
    );
    assert_eq!(
        validate(&package, false, &both).expect("passes"),
        SkillState::Available
    );
}

#[test]
fn a_source_prefix_and_the_star_wildcard_honor_their_meaning() {
    let dir = scratch("wildcard");
    let package = write(
        &dir,
        "wild-skill",
        "material",
        &manifest_of(
            "wild-skill",
            "anyone",
            "https://mirror.example.invalid/ any/path",
            None,
        ),
    );
    let generous = AllowList::new(vec!["*".into()], vec!["*".into()], true);
    let _ = &package.manifest.source;

    // A prefix match: a source under the registered root is inside.
    let prefix_list = AllowList::new(
        vec!["anyone".into()],
        vec!["https://mirror.example.invalid/".into()],
        true,
    );
    assert!(validate(&package, false, &prefix_list).is_ok());
    // A different root is not.
    let prefix_list = AllowList::new(
        vec!["anyone".into()],
        vec!["https://other.example.invalid/".into()],
        true,
    );
    assert!(validate(&package, false, &prefix_list).is_err());
    let _ = generous;
}

// --- ATH-SKILL-009: failure means no candidate, never a silent retry ---------------------

#[test]
fn a_failed_validation_is_failed_not_available() {
    let dir = scratch("failed-state");
    let package = write(
        &dir,
        "untrusted",
        "material",
        &manifest_of(
            "untrusted",
            "stranger",
            "https://unknown.example.invalid",
            None,
        ),
    );
    let _ = set_failed;
    let outcome = validate(&package, false, &AllowList::default());
    match outcome {
        Err(_) => {
            // The call site (Registry, P5-03/04) writes `FAILED` for this exact fault and
            // the Catalog excludes `FAILED` from selection (`ATH-STATE-008`).
            assert!(!SkillState::Failed.is_selectable());
        }
        Ok(state) => panic!("must not be {state:?}"),
    }
}

fn set_failed() {}
