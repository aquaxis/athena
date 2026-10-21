//! Tests for the `SkillRepository` on `AthenaStore` (P5-13 supporting layer).

use crate::AthenaStore;
use crate::repository::SkillRepository;
use athena_types::{
    AgentId, Permission, RiskLevel, Skill, SkillId, SkillRef, SkillState, ToolName,
};

fn fixture(id: &str, version: &str) -> Skill {
    Skill {
        id: SkillId::new(id),
        name: id.into(),
        version: version.into(),
        description: "store fixture".into(),
        capabilities: vec!["summarize".into(), format!("{id}-extra")],
        required_tools: vec![ToolName::FsRead, ToolName::Shell],
        required_permissions: vec![Permission::Tool {
            tool: ToolName::FsRead,
        }],
        supported_agents: vec![AgentId::new("timing-lead")],
        dependencies: vec![SkillRef {
            id: SkillId::new("dep-skill"),
            version: "^1.0.0".into(),
            checksum: None,
        }],
        runtime_provider: "agent-cli".into(),
        source: "local".into(),
        checksum: "sha256:fixture".into(),
        publisher: "aquaxis".into(),
        risk_level: RiskLevel::Medium,
        state: SkillState::Available,
        hot_reload_safe: false,
        registered_at: "2026-10-18T00:00:00Z".into(),
        updated_at: "2026-10-18T00:00:00Z".into(),
    }
}

#[test]
fn a_registered_skill_round_trips_through_the_store() {
    let mut store = AthenaStore::in_memory().expect("store");
    let fixture = fixture("round-trip", "1.0.0");
    SkillRepository::upsert(&mut store, &fixture).expect("upserted");

    let row = SkillRepository::get(&store, &fixture.id, "1.0.0")
        .expect("fetch")
        .expect("present");
    assert_eq!(row.name, "round-trip");
    assert_eq!(row.risk_level, RiskLevel::Medium);
    assert_eq!(row.required_tools, fixture.required_tools);
    assert_eq!(row.required_permissions, fixture.required_permissions);
    assert_eq!(row.supported_agents, fixture.supported_agents);
    assert_eq!(row.dependencies, fixture.dependencies);
    assert_eq!(row.capabilities, fixture.capabilities);
    assert_eq!(row.state, SkillState::Available);
    assert!(!row.hot_reload_safe);
}

#[test]
fn versions_list_per_id_and_states_list_per_state() {
    let mut store = AthenaStore::in_memory().expect("store");
    let mut enabled = fixture("multi-version", "1.0.0");
    enabled.state = SkillState::Enabled;
    SkillRepository::upsert(&mut store, &enabled).expect("upverted");
    SkillRepository::upsert(&mut store, &fixture("multi-version", "1.1.0")).expect("upserted 2");
    SkillRepository::upsert(&mut store, &fixture("other-skill", "1.0.0")).expect("upserted 3");

    let versions =
        SkillRepository::list_versions(&store, &SkillId::new("multi-version")).expect("versions");
    assert_eq!(versions.len(), 2);

    let selectable = SkillRepository::list_by_state(&store, SkillState::Enabled).expect("metric");
    assert_eq!(selectable.len(), 1);
    assert!(selectable[0].state.is_selectable());
}

#[test]
fn the_capability_search_honors_the_enabled_state_exact_text() {
    let mut store = AthenaStore::in_memory().expect("store");
    let mut helper = fixture("helper", "1.0.0");
    helper.capabilities = vec!["summarize".into()];
    helper.state = SkillState::Enabled;
    SkillRepository::upsert(&mut store, &helper).expect("helper");
    // Not enabled:
    SkillRepository::upsert(&mut store, &fixture("asleep", "1.0.0")).expect("asleep");

    let found = SkillRepository::search_by_capability(&store, "summarize").expect("search");
    assert_eq!(found.len(), 1, "only the ENABLED helper is a candidate");
    assert_eq!(found[0].id.as_str(), "helper");
    // An exact-text miss is an empty set, not a fuzzy one.
    assert!(
        SkillRepository::search_by_capability(&store, "summarize-")
            .expect("search")
            .is_empty(),
        "the exact-text miss"
    );
}

#[test]
fn the_delete_refuses_a_missing_version() {
    let mut store = AthenaStore::in_memory().expect("store");
    assert!(SkillRepository::delete(&mut store, &SkillId::new("absent"), "1.0.0").is_err());
    SkillRepository::upsert(&mut store, &fixture("doomed", "1.0.0")).expect("in");
    SkillRepository::delete(&mut store, &SkillId::new("doomed"), "1.0.0").expect("gone");
    // And the row is really gone.
    assert!(
        SkillRepository::get(&store, &SkillId::new("doomed"), "1.0.0")
            .expect("fetch")
            .is_none()
    );
}
