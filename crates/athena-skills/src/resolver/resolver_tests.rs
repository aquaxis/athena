//! Tests for the Skill Resolver (P5-06).

use super::*;
use crate::registry::SkillStore;
use athena_types::SkillId;
use std::collections::HashMap;

/// The store: rows seeded directly (the Registry layer's write path is its own tests').
#[derive(Default)]
struct Rows {
    table: HashMap<(String, String), Skill>,
}

impl Rows {
    fn add(&mut self, skill: Skill) {
        self.table.insert(
            (skill.id.as_str().to_string(), skill.version.clone()),
            skill,
        );
    }
}

impl SkillStore for Rows {
    fn write(&mut self, skill: &Skill) -> Result<(), String> {
        self.add(skill.clone());
        Ok(())
    }

    fn read(&self, id: &SkillId, version: &str) -> Result<Option<Skill>, String> {
        Ok(self
            .table
            .get(&(id.as_str().to_string(), version.to_string()))
            .cloned())
    }

    fn list_all(&self) -> Result<Vec<Skill>, String> {
        Ok(self.table.values().cloned().collect())
    }
}

fn skill(
    id: &str,
    version: &str,
    deps: Vec<SkillRef>,
    tools: Vec<ToolName>,
    perms: Vec<Permission>,
    state: SkillState,
) -> Skill {
    Skill {
        id: SkillId::new(id),
        name: id.to_string(),
        version: version.to_string(),
        description: "resolver fixture".into(),
        capabilities: vec!["summarize".into()],
        required_tools: tools,
        required_permissions: perms,
        supported_agents: vec![],
        dependencies: deps,
        runtime_provider: "agent-cli".into(),
        source: "local".into(),
        checksum: format!("sha256:{id}-{version}"),
        publisher: "local".into(),
        risk_level: athena_types::RiskLevel::Low,
        state,
        hot_reload_safe: false,
        registered_at: "t".into(),
        updated_at: "t".into(),
    }
}

fn granted_all() -> AgentFace {
    // The grant everything the fixture needs: both enums' relevant faces.
    let mut permissions = vec![];
    for tool in ToolName::ALL {
        permissions.push(Permission::Tool { tool: *tool });
    }
    permissions.push(Permission::Fs {
        mode: athena_types::FsMode::Read,
        scope: "/workspace".into(),
    });
    permissions.push(Permission::Fs {
        mode: athena_types::FsMode::Write,
        scope: "/workspace".into(),
    });
    permissions.push(Permission::Network {
        mode: athena_types::NetworkMode::Allowlist,
        hosts: Some(vec!["example.invalid".into()]),
    });
    permissions.push(Permission::Network {
        mode: athena_types::NetworkMode::Allow,
        hosts: None,
    });
    AgentFace {
        allowed_tools: ToolName::ALL.to_vec(),
        permissions,
    }
}

// --- The transitive closure picks the max satisfying version ------------------------------

#[test]
fn the_closure_picks_the_max_satisfying_version_of_each_range() {
    let mut store = Rows::default();
    store.add(skill(
        "root-skill",
        "1.0.0",
        vec![dep("child-skill", "^1.0.0")],
        vec![],
        vec![],
        SkillState::Enabled,
    ));
    store.add(skill(
        "child-skill",
        "1.2.3",
        vec![],
        vec![],
        vec![],
        SkillState::Enabled,
    ));
    store.add(skill(
        "child-skill",
        "1.4.0",
        vec![],
        vec![],
        vec![],
        SkillState::Enabled,
    ));
    store.add(skill(
        "child-skill",
        "2.0.0",
        vec![],
        vec![],
        vec![],
        SkillState::Enabled,
    ));

    let requested = skill_ref("root-skill", "1.0.0");
    let resolution = resolve(&requested, &granted_all(), ToolName::ALL, &store).expect("resolves");
    let pinned: Vec<(String, String)> = resolution
        .members
        .iter()
        .map(|member| (member.id.as_str().to_string(), member.version.clone()))
        .collect();
    assert!(
        pinned.contains(&("child-skill".into(), "1.4.0".into())),
        "the max within ^1.0.0 is 1.4.0, not 2.0.0 — got {pinned:?}"
    );
}

#[test]
fn the_closure_is_transitive_to_the_leaves() {
    let mut store = Rows::default();
    store.add(skill(
        "a-root",
        "1.0.0",
        vec![dep("b-mid", "1.0.0")],
        vec![],
        vec![],
        SkillState::Enabled,
    ));
    store.add(skill(
        "b-mid",
        "1.0.0",
        vec![dep("c-leaf", "1.0.0")],
        vec![],
        vec![],
        SkillState::Enabled,
    ));
    store.add(skill(
        "c-leaf",
        "1.0.0",
        vec![],
        vec![],
        vec![],
        SkillState::Enabled,
    ));
    let resolution = resolve(
        &skill_ref("a-root", "1.0.0"),
        &granted_all(),
        ToolName::ALL,
        &store,
    )
    .expect("resolves");
    assert_eq!(resolution.members.len(), 3, "root → mid → leaf all walked");
}

#[test]
fn nothing_satisfying_is_a_fact_refusal() {
    let mut store = Rows::default();
    store.add(skill(
        "root-skill",
        "1.0.0",
        vec![dep("ghost-skill", "^1.0.0")],
        vec![],
        vec![],
        SkillState::Enabled,
    ));
    let fault = resolve(
        &skill_ref("root-skill", "1.0.0"),
        &granted_all(),
        ToolName::ALL,
        &store,
    );
    assert!(matches!(fault, Err(ResolverFault::NothingSatisfies { .. })));
    // A range over *nothing in the store* is the same fact.
    let mut empty = Rows::default();
    empty.add(skill(
        "solo",
        "1.0.0",
        vec![],
        vec![],
        vec![],
        SkillState::Enabled,
    ));
    let _ = empty;
}

// --- Conflicts refuse, never silently pick -----------------------------------------------

#[test]
fn two_members_demanding_different_versions_of_one_skill_is_a_conflict() {
    let mut store = Rows::default();
    store.add(skill(
        "root-skill",
        "1.0.0",
        vec![dep("shared", "=1.0.0"), dep("other-branch", "=1.0.0")],
        vec![],
        vec![],
        SkillState::Enabled,
    ));
    store.add(skill(
        "other-branch",
        "1.0.0",
        vec![dep("shared", "=2.0.0")],
        vec![],
        vec![],
        SkillState::Enabled,
    ));
    store.add(skill(
        "shared",
        "1.0.0",
        vec![],
        vec![],
        vec![],
        SkillState::Enabled,
    ));
    store.add(skill(
        "shared",
        "2.0.0",
        vec![],
        vec![],
        vec![],
        SkillState::Enabled,
    ));
    let fault = resolve(
        &skill_ref("root-skill", "1.0.0"),
        &granted_all(),
        ToolName::ALL,
        &store,
    );
    assert!(
        matches!(fault, Err(ResolverFault::ConflictingRanges { .. })),
        "the same skill demanded at two pins is a SkillError, never a silent pick: {fault:?}"
    );
}

// --- The ENABLED gate --------------------------------------------------------------------

#[test]
fn a_disabled_dependency_veers_the_whole_set() {
    let mut store = Rows::default();
    store.add(skill(
        "root-skill",
        "1.0.0",
        vec![dep("sleepy", "1.0.0")],
        vec![],
        vec![],
        SkillState::Enabled,
    ));
    store.add(skill(
        "sleepy",
        "1.0.0",
        vec![],
        vec![],
        vec![],
        SkillState::Disabled,
    ));
    let fault = resolve(
        &skill_ref("root-skill", "1.0.0"),
        &granted_all(),
        ToolName::ALL,
        &store,
    );
    assert!(matches!(
        fault,
        Err(ResolverFault::NotEnableable {
            state: SkillState::Disabled,
            ..
        })
    ));
}

// --- The permission union and the two-axis tool check ------------------------------------

#[test]
fn the_union_over_the_closure_must_sit_inside_the_agent_grant() {
    let mut store = Rows::default();
    store.add(skill(
        "root-skill",
        "1.0.0",
        vec![dep("greedy", "1.0.0")],
        vec![],
        vec![],
        SkillState::Enabled,
    ));
    store.add(skill(
        "greedy",
        "1.0.0",
        vec![],
        vec![],
        vec![Permission::Network {
            mode: athena_types::NetworkMode::Allow,
            hosts: None,
        }],
        SkillState::Enabled,
    ));
    // Agent grants only file reads.
    let agent = AgentFace {
        allowed_tools: ToolName::ALL.to_vec(),
        permissions: vec![Permission::Tool {
            tool: ToolName::FsRead,
        }],
    };
    let fault = resolve(
        &skill_ref("root-skill", "1.0.0"),
        &agent,
        ToolName::ALL,
        &store,
    );
    assert!(matches!(fault, Err(ResolverFault::PermissionExceeded(_))));
    // The full-grant fixture resolves.
    assert!(
        resolve(
            &skill_ref("root-skill", "1.0.0"),
            &granted_all(),
            ToolName::ALL,
            &store
        )
        .is_ok()
    );
}

#[test]
fn the_tool_union_sits_inside_both_the_grant_and_the_platform() {
    let mut store = Rows::default();
    store.add(skill(
        "root-skill",
        "1.0.0",
        vec![],
        vec![ToolName::Shell],
        vec![],
        SkillState::Enabled,
    ));
    // Granted but the platform cannot honor it:
    let platform = vec![ToolName::FsRead];
    let fault = resolve(
        &skill_ref("root-skill", "1.0.0"),
        &granted_all(),
        &platform,
        &store,
    );
    assert!(matches!(fault, Err(ResolverFault::ToolNotCapable(_))));
    // Capable but not granted:
    let stingy_agent = AgentFace {
        allowed_tools: vec![ToolName::FsRead],
        permissions: Vec::new(),
    };
    let fault = resolve(
        &skill_ref("root-skill", "1.0.0"),
        &stingy_agent,
        ToolName::ALL,
        &store,
    );
    assert!(matches!(fault, Err(ResolverFault::ToolNotGranted(_))));
    // Both axes satisfied:
    assert!(
        resolve(
            &skill_ref("root-skill", "1.0.0"),
            &granted_all(),
            ToolName::ALL,
            &store
        )
        .is_ok()
    );
}

fn dep(id: &str, range: &str) -> SkillRef {
    SkillRef {
        id: SkillId::new(id),
        version: range.to_string(),
        checksum: None,
    }
}

fn skill_ref(id: &str, version: &str) -> SkillRef {
    dep(id, version)
}
