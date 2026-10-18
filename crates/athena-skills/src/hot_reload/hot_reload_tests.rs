//! Tests for the hot reload rules (P5-10).

use super::*;
use athena_types::{AgentId, InstanceId};

fn running_with(members: &[(&str, &str, &str)]) -> FrozenSkillSet {
    FrozenSkillSet {
        agent_id: AgentId::new("hot-lead"),
        instance_id: InstanceId::new(),
        members: members
            .iter()
            .map(|(id, version, checksum)| FrozenMember {
                id: (*id).into(),
                version: (*version).into(),
                checksum: (*checksum).into(),
            })
            .collect(),
        frozen_at: "2026-10-18T00:00:00Z".into(),
    }
}

fn candidate(
    id: &str,
    version: &str,
    declared: bool,
    material: Option<&str>,
    permissions: Vec<Permission>,
) -> ReloadCandidate {
    ReloadCandidate {
        member: FrozenMember {
            id: id.into(),
            version: version.into(),
            checksum: format!("sha256:{id}@{version}"),
        },
        declared_safe: declared,
        material_path: material.map(|stamp| format!("/workspace/skills/{stamp}/SKILL.md")),
        permissions,
    }
}

#[test]
fn an_undeclared_candidate_refuses() {
    let running = running_with(&[]);
    let arrival = candidate("sneak", "1.0.0", false, Some("sneak"), vec![]);
    let fault = attempt(&running, &arrival, &[], 1);
    assert!(matches!(fault, Err(HotReloadFault::NotDeclaredSafe { .. })));
}

#[test]
fn a_prose_only_reload_refuses() {
    let running = running_with(&[]);
    let arrival = candidate("named-only", "1.0.0", true, None, vec![]);
    let fault = attempt(&running, &arrival, &[], 1);
    assert!(matches!(fault, Err(HotReloadFault::MaterialMissing { .. })));
}

#[test]
fn reloading_a_pinned_member_changes_nothing() {
    let running = running_with(&[("already", "1.0.0", "sha256:already")]);
    let arrival = candidate("already", "1.0.0", true, Some("already"), vec![]);
    let outcome = attempt(&running, &arrival, &[], 3).expect("no-op");
    assert_eq!(outcome.new_revision, 3, "the set did not change");
    assert_eq!(outcome.members.len(), 1);
}

#[test]
fn the_enlarged_union_is_rechecked_against_the_current_grant() {
    let running = running_with(&[]);
    let within = candidate(
        "safe-add",
        "1.0.0",
        true,
        Some("safe-add"),
        vec![Permission::Tool {
            tool: athena_types::ToolName::FsRead,
        }],
    );
    let grant = vec![Permission::Tool {
        tool: athena_types::ToolName::FsRead,
    }];
    let outcome = attempt(&running, &within, &grant, 1).expect("admitted");
    assert_eq!(outcome.new_revision, 2, "the new revision rides N+1");
    assert_eq!(outcome.members.len(), 1);

    let arrival = candidate(
        "greedy",
        "1.0.0",
        true,
        Some("greedy"),
        vec![Permission::Network {
            mode: athena_types::NetworkMode::Allow,
            hosts: None,
        }],
    );
    let fault = attempt(&running, &arrival, &grant, 1);
    assert!(matches!(fault, Err(HotReloadFault::PermissionExceeded(_))));
}

#[test]
fn the_replacement_freezes_again_and_keeps_the_frozen_facts() {
    let running = running_with(&[("keep-me", "1.0.0", "sha256:keep")]);
    let arrival = candidate("added", "1.0.0", true, Some("added"), vec![]);
    let grant = vec![];
    let outcome = attempt(&running, &arrival, &grant, 4).expect("admitted");
    let replacement = freeze_replacement(&outcome, &running).expect("freezes");
    assert_eq!(replacement.members.len(), 2);
    assert!(replacement.member_is_pinned("keep-me"));
    assert!(replacement.member_is_pinned("added"));
    assert!(replacement.absorbs_lifecycle_moves());
}
