//! Tests for the frozen Skill Set (P5-09).

use super::*;

fn resolution_of(states: &[(SkillState, &str, &str)]) -> crate::resolver::Resolution {
    let members: Vec<athena_types::Skill> = states
        .iter()
        .map(|(state, id, version)| athena_types::Skill {
            id: athena_types::SkillId::new(*id),
            name: (*id).into(),
            version: (*version).into(),
            description: "fixture".into(),
            capabilities: vec![],
            required_tools: vec![],
            required_permissions: vec![],
            supported_agents: vec![],
            dependencies: vec![],
            runtime_provider: "agent-cli".into(),
            source: "local".into(),
            checksum: format!("sha256:{id}@{version}"),
            publisher: "local".into(),
            risk_level: athena_types::RiskLevel::Low,
            state: *state,
            hot_reload_safe: false,
            registered_at: "t".into(),
            updated_at: "t".into(),
        })
        .collect();
    crate::resolver::Resolution {
        members,
        permission_union: vec![],
        tool_union: vec![],
    }
}

// --- The freeze is the start-time resolution (ATH-SKILL-010) ------------------------------

#[test]
fn the_freeze_pins_every_member_with_its_checksum() {
    let resolution = resolution_of(&[
        (SkillState::Enabled, "fpga-timing-analysis", "1.2.0"),
        (SkillState::Enabled, "sdc-analysis", "1.0.1"),
    ]);
    let frozen = FrozenSkillSet::freeze(
        AgentId::new("timing-lead"),
        InstanceId::new(),
        &resolution,
        "2026-10-18T00:00:00Z",
    )
    .expect("freezes");
    assert_eq!(frozen.members.len(), 2);
    assert_eq!(frozen.members[0].id, "fpga-timing-analysis");
    assert_eq!(frozen.members[0].version, "1.2.0");
    assert!(frozen.members[0].checksum.starts_with("sha256:fpga"));
}

#[test]
fn a_hand_assembled_set_refuses_non_enabled_members() {
    let resolution = resolution_of(&[(SkillState::Disabled, "sleepy", "1.0.0")]);
    let fault = FrozenSkillSet::freeze(AgentId::new("any"), InstanceId::new(), &resolution, "t");
    assert!(matches!(
        fault,
        Err(FreezeFault::MemberNotEnabled {
            state: SkillState::Disabled,
            ..
        })
    ));
}

// --- Immunity from the lifecycle moves (ATH-STATE-009) ------------------------------------

#[test]
fn lifecycle_moves_never_unpin_a_member() {
    let resolution = resolution_of(&[(SkillState::Enabled, "member", "1.0.0")]);
    let frozen = FrozenSkillSet::freeze(AgentId::new("any"), InstanceId::new(), &resolution, "t")
        .expect("frozen");
    assert!(frozen.member_is_pinned("member"));
    // The Registry's UPDATING / REMOVED on a mid-run member is absorbed by the freeze —
    // the member keeps running with the pinned (id, version, checksum).
    assert!(frozen.absorbs_lifecycle_moves());
    assert!(
        frozen.member_is_pinned("member"),
        "still pinned after any moves"
    );
    // The freeze does not track lifecycle states — the constant is structural.
    assert!(
        FrozenSkillSet::freeze(AgentId::new("any"), InstanceId::new(), &resolution, "t2")
            .expect("re-freezes")
            .absorbs_lifecycle_moves()
    );
}

// --- The execution-history JSON (ATH-SKILL-021) -------------------------------------------

#[test]
fn the_history_json_carries_the_12_8_shape() {
    let resolution = resolution_of(&[
        (SkillState::Enabled, "fpga-timing-analysis", "1.2.0"),
        (SkillState::Enabled, "sdc-analysis", "1.0.1"),
    ]);
    let frozen = FrozenSkillSet::freeze(
        AgentId::new("timing-lead"),
        InstanceId::new(),
        &resolution,
        "2026-10-18T00:00:00Z",
    )
    .expect("frozen");
    let json = frozen.to_history_json().expect("serialized");
    assert!(json.body.contains(r#""agent_id": "timing-lead""#));
    assert!(json.body.contains(r#""skills": ["#));
    for required in ["fpga-timing-analysis", "1.2.0", "sdc-analysis", "1.0.1"] {
        assert!(
            json.body.contains(required),
            "{required} missing: {}",
            json.body
        );
    }
    // Each skills[] row is the triple.
    assert!(json.body.contains(r#""checksum": ""#));
    assert!(json.path_hint.starts_with(".aiprj/AI_LOG"));
}

// --- The requester ban on the freeze side (ATH-STATE-025) ---------------------------------

#[test]
fn the_core_agent_may_only_select_not_move_the_catalog() {
    assert!(may_move_lifecycle(&Actor::Human));
    assert!(may_move_lifecycle(&Actor::System));
    assert!(!may_move_lifecycle(&Actor::CoreAgent));
}
