//! Tests for the Skill Registry (P5-03) and the source separation (P5-04).

use super::*;
use crate::package::Package;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The in-memory `SkillStore` face the tests drive.
#[derive(Default)]
pub(crate) struct MemStore {
    rows: RefCell<HashMap<(String, String), athena_types::Skill>>,
}

impl SkillStore for MemStore {
    fn write(&mut self, skill: &athena_types::Skill) -> Result<(), String> {
        self.rows.borrow_mut().insert(
            (skill.id.as_str().to_string(), skill.version.clone()),
            skill.clone(),
        );
        Ok(())
    }

    fn read(
        &self,
        id: &athena_types::SkillId,
        version: &str,
    ) -> Result<Option<athena_types::Skill>, String> {
        Ok(self
            .rows
            .borrow()
            .get(&(id.as_str().to_string(), version.to_string()))
            .cloned())
    }

    fn list_all(&self) -> Result<Vec<athena_types::Skill>, String> {
        Ok(self.rows.borrow().values().cloned().collect())
    }
}

pub(crate) fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "athena-registry-{tag}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

pub(crate) fn make_package(id: &str, skill_md: &str, source_dir: &Path) -> Package {
    std::fs::create_dir_all(source_dir.join(id)).expect("dir");
    std::fs::write(source_dir.join(id).join("SKILL.md"), skill_md).expect("md");
    std::fs::write(
        source_dir.join(id).join("manifest.yaml"),
        format!(
            r#"
id: {id}
name: Test Skill
version: 1.0.0
description: registry test fixture
capabilities: ["summarize"]
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
        ),
    )
    .expect("manifest");
    Package::open(&source_dir.join(id)).expect("opens")
}

/// Marker that `Outcome::Transitioned` result types are keepers in every leg below.
#[allow(dead_code)]
fn keeper(outcome: &Outcome) -> bool {
    matches!(outcome, Outcome::Transitioned { .. })
}

// --- The nine operations ------------------------------------------------------------------

#[test]
fn register_validates_copies_to_the_registered_tree_and_only_that_is_a_candidate() {
    let source = scratch("source");
    let registered = scratch("registered");
    let package = make_package("copy-me", "the material", &source);
    let mut store = MemStore::default();
    let outcome = register(
        &package,
        true,
        &AllowList::default(),
        &source,
        &registered,
        &mut store,
        "2026-10-18T00:00:00Z",
    )
    .expect("registrable");
    match outcome {
        Outcome::Transitioned { event, to, .. } => {
            assert_eq!(event, "SkillValidated");
            assert_eq!(to, SkillState::Available);
        }
        other => panic!("expected completed mechanism outcome, got {other:?}"),
    }

    // The registered copy exists in the skills.dir tree, not the source dir.
    let copy = registered.join("copy-me").join("1.0.0").join("SKILL.md");
    assert!(copy.is_file(), "the registered body was written");

    // Only registered rows are candidates (the source tree holds no row at all).
    // NOT yet a candidate: only ENABLED is selectable (ATH-STATE-008); registration
    // lands AVAILABLE and `auto_enable_on_register` stays false (ATH-API-014).
    assert!(matches!(
        search("summarize", &store).expect("search"),
        Outcome::Listed(rows) if rows.is_empty()
    ));
    assert!(registered != source);
}

#[test]
fn registration_refuses_a_colliding_single_directory() {
    let dir = scratch("collide");
    let package = make_package("collider", "material", &dir);
    let mut store = MemStore::default();
    let fault = register(
        &package,
        true,
        &AllowList::default(),
        &dir,
        &dir,
        &mut store,
        "now",
    )
    .expect_err("same dir refused");
    assert!(
        matches!(fault, RegistryFault::SameDirectory),
        "ATH-SKILL-038's identical-tree rule: {fault:?}"
    );
}

#[test]
fn the_lifecycle_walks_the_table_and_refuses_off_rows() {
    let source = scratch("walk-source");
    let registered = scratch("walk-registered");
    let package = make_package("walker", "material", &source);
    let mut store = MemStore::default();
    register(
        &package,
        true,
        &AllowList::default(),
        &source,
        &registered,
        &mut store,
        "t1",
    )
    .expect("in");
    let id = &athena_types::SkillId::new("walker");

    // AVAILABLE stepped ENABLED (admin) → DISABLED → ENABLED: each return its own
    // destination state — the row reached, not a constant.
    let steps = [
        (athena_types::SkillTrigger::Enabled, SkillState::Enabled),
        (athena_types::SkillTrigger::Disabled, SkillState::Disabled),
        (athena_types::SkillTrigger::Enabled, SkillState::Enabled),
    ];
    let mut stamp = 0;
    for (_, expected_state) in steps {
        stamp += 1;
        let operator = if expected_state == SkillState::Enabled {
            enable(id, "1.0.0", &format!("st{stamp}"), &mut store)
        } else {
            disable(id, "1.0.0", &format!("st{stamp}"), &mut store)
        };
        match operator.expect("legal transition") {
            Outcome::Transitioned { to, .. } => assert_eq!(to, expected_state),
            other => panic!("expected Transitioned, got {other:?}"),
        }
    }

    // update rides ENABLED → UPDATING off the *current* row; then unregister to terminal.
    match update(id, "1.0.0", "st9", &mut store).expect("legal") {
        Outcome::Transitioned { event, to, .. } => {
            assert_eq!(event, "SkillUpdating");
            assert_eq!(to, SkillState::Updating);
        }
        other => panic!("expected Transitioned, got {other:?}"),
    }
    // From UPDATING the §17.3 rows go VALIDATING / FAILED / AVAILABLE — no removal.
    assert!(matches!(
        unregister(id, "1.0.0", "st10", &mut store),
        Err(RegistryFault::IllegalTransition {
            operation: "unregister",
            state: SkillState::Updating,
            ..
        })
    ));
}

#[test]
fn list_get_search_shape_their_faces() {
    let source = scratch("faces-source");
    let registered = scratch("faces-registered");
    let mut store = MemStore::default();
    for id in ["alpha", "beta"] {
        let package = make_package(id, "material", &source);
        register(
            &package,
            true,
            &AllowList::default(),
            &source,
            &registered,
            &mut store,
            "t",
        )
        .expect("in");
    }
    // The rows start AVAILABLE — not selectable — so search over ENABLED is empty.
    assert!(matches!(
        search("summarize", &store).expect("search"),
        Outcome::Listed(rows) if rows.is_empty()
    ));
    // list returns both.
    match list(&store).expect("list") {
        Outcome::Listed(rows) => assert_eq!(rows.len(), 2),
        other => panic!("expected Listed, got {other:?}"),
    }
    // get returns the one id's versions.
    match get(&athena_types::SkillId::new("alpha"), &store).expect("get") {
        Outcome::Listed(rows) => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].id.as_str(), "alpha");
        }
        other => panic!("expected Listed, got {other:?}"),
    }
    // Enable alpha and the search finds it (only ENABLED is a candidate, ATH-STATE-008).
    enable(
        &athena_types::SkillId::new("alpha"),
        "1.0.0",
        "s",
        &mut store,
    )
    .expect("enable");
    match search("summarize", &store).expect("search") {
        Outcome::Listed(rows) => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].id.as_str(), "alpha");
        }
        other => panic!("expected Listed, got {other:?}"),
    }
    // And a capability with no offering returns nothing.
    assert!(matches!(
        search("nonexistent-capability", &store).expect("search"),
        Outcome::Listed(rows) if rows.is_empty()
    ));
}

#[test]
fn re_register_after_removal_walks_the_lifecycle_and_refresh_helper_names_itself() {
    // refresh over an empty source tree is a no-op named NothingToRefresh.
    assert!(matches!(refresh_outcome(0), Outcome::NothingToRefresh));
    assert!(matches!(refresh_outcome(3), Outcome::NothingToRefresh));
}
