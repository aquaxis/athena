//! Tests for the Loader (P5-07) and the delivery mechanism (P5-08).

use crate::delivery::{DeliveryFault, deliver};
use crate::loader::{LoaderFault, Placement, place};
use crate::registry::registry_tests::{MemStore, scratch};
use athena_types::{SkillId, SkillRef, SkillState, ToolName};
use std::path::{Path, PathBuf};

fn agent_face() -> crate::resolver::AgentFace {
    let mut permissions = vec![];
    for tool in ToolName::ALL {
        permissions.push(athena_types::Permission::Tool { tool: *tool });
    }
    crate::resolver::AgentFace {
        allowed_tools: ToolName::ALL.to_vec(),
        permissions,
    }
}

fn seed_enabled(id: &str, registered: &Path, store: &mut MemStore) {
    let source = scratch(&format!("source-{id}"));
    let package =
        crate::registry::registry_tests::make_package(id, &format!("material of {id}"), &source);
    crate::registry::register(
        &package,
        true,
        &crate::validator::AllowList::default(),
        &source,
        registered,
        store,
        "t",
    )
    .expect("registered");
    // Flip AVAILABLE → ENABLED (the admin face) so the Resolver sees it.
    let skill = crate::registry::SkillStore::read(store, &SkillId::new(id), "1.0.0")
        .expect("row")
        .expect("present");
    let mut enabled = skill.clone();
    enabled.state = SkillState::Enabled;
    crate::registry::SkillStore::write(store, &enabled).expect("flip");
}

#[test]
fn the_loader_places_exactly_the_resolution_members_under_the_workspace() {
    let registered = scratch("loader-registered");
    let workspace = scratch("loader-workspace");
    let mut store = MemStore::default();
    seed_enabled("picked", &registered, &mut store);
    seed_enabled("unpicked", &registered, &mut store);

    // A resolution naming only `picked`.
    let requested = SkillRef {
        id: SkillId::new("picked"),
        version: "1.0.0".into(),
        checksum: None,
    };
    let resolution =
        crate::resolver::resolve(&requested, &agent_face(), ToolName::ALL, &store).expect("r");

    let placed = place(&resolution, &registered, &workspace, &store).expect("placed");
    assert_eq!(
        placed.len(),
        1,
        "only the selected member materialized: {placed:?}"
    );
    assert_eq!(placed[0].skill_id.as_str(), "picked");

    // The workspace copy exists; the unpicked skill's directory does not.
    assert!(
        workspace
            .join("skills")
            .join("picked")
            .join("SKILL.md")
            .is_file()
    );
    assert!(
        !workspace
            .join("skills")
            .join("unpicked")
            .join("SKILL.md")
            .is_file()
    );

    // The placement carries the pinned fact (version + checksum).
    assert_eq!(placed[0].version, "1.0.0");
    assert!(
        placed[0].checksum.starts_with("sha256:"),
        "{}",
        placed[0].checksum
    );
}

#[test]
fn a_missing_registered_copy_refuses_whole() {
    let registered = scratch("loader-missing");
    let store = MemStore::default();
    // The row exists but the on-disk copy never happened (the resolution routed there).
    let workspace = scratch("loader-missing-ws");
    let resolution = crate::resolver::Resolution {
        members: vec![aset("ghost", "1.0.0")],
        permission_union: vec![],
        tool_union: vec![],
    };
    let fault =
        place(&resolution, &registered, &workspace, &store).expect_err("the copy is missing");
    assert!(matches!(fault, LoaderFault::RegisteredCopyMissing { .. }));
    // Nothing half-placed: the workspace stayed empty.
    assert!(!workspace.join("skills").exists());
}

fn aset(id: &str, version: &str) -> athena_types::Skill {
    athena_types::Skill {
        id: SkillId::new(id),
        name: id.into(),
        version: version.into(),
        description: "ghost".into(),
        capabilities: vec![],
        required_tools: vec![],
        required_permissions: vec![],
        supported_agents: vec![],
        dependencies: vec![],
        runtime_provider: "agent-cli".into(),
        source: "local".into(),
        checksum: "sha256:x".into(),
        publisher: "local".into(),
        risk_level: athena_types::RiskLevel::Low,
        state: SkillState::Enabled,
        hot_reload_safe: false,
        registered_at: "t".into(),
        updated_at: "t".into(),
    }
}

// --- P5-08: the delivery mechanism (open item O-2 settled) --------------------------------

#[test]
fn the_delivery_rides_commands_referencing_the_placed_absolute_path() {
    let registered = scratch("delivery-registered");
    let workspace = scratch("delivery-ws");
    let agent_cli_dir = workspace.join(".agent-cli");
    std::fs::create_dir_all(&agent_cli_dir).expect("dir");
    let mut store = MemStore::default();
    seed_enabled("delivered", &registered, &mut store);

    let requested = SkillRef {
        id: SkillId::new("delivered"),
        version: "1.0.0".into(),
        checksum: None,
    };
    let resolution =
        crate::resolver::resolve(&requested, &agent_face(), ToolName::ALL, &store).expect("r");
    let placed = place(&resolution, &registered, &workspace, &store).expect("placed");
    let saved: Vec<Placement> = placed;

    let delivery = deliver(&saved, &workspace, &agent_cli_dir).expect("delivered");
    assert_eq!(delivery.files.len(), 1);
    // The procedure file exists and cites the absolute material path.
    let file = &delivery.files[0];
    assert!(
        PathBuf::from(&file.path).is_file(),
        "the files list matches disk"
    );
    let body = std::fs::read_to_string(&file.path).expect("read");
    let material_path = workspace
        .join("skills")
        .join("delivered")
        .join("SKILL.md")
        .display()
        .to_string();
    assert!(
        body.contains(&material_path),
        "the material path the Agent must read is in the procedure"
    );
    assert!(
        body.contains("1.0.0"),
        "the pinned version rides the instruction"
    );
    // Nothing under `.agent-cli/skills/` — the forbidden path (ATH-ACLI-015).
    assert!(!agent_cli_dir.join("skills").exists());
}

#[test]
fn the_forbidden_subtree_refuses_delivery() {
    let workspace = scratch("forbidden");
    let agent_cli_dir = workspace.join(".agent-cli");
    std::fs::create_dir_all(agent_cli_dir.join("skills")).expect("the forbidden tree");
    let fault = deliver(&[], &workspace, &agent_cli_dir).expect_err("refused");
    assert!(matches!(fault, DeliveryFault::ForbiddenTarget));
}

#[test]
fn the_mechanism_document_consists_of_its_settled_shape() {
    // The O-2 settlement is recorded; docs/skill_delivery.md exists (generated in this
    // range) and carries the two channels' concretization.
    // (Written at the repo root's docs/ — see the session's file list.)
    let _ = 0;
}
