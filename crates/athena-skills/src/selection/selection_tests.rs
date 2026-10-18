//! Tests for capability-matching selection (P5-14).

use super::*;
use athena_types::SkillState;

fn skill(
    id: &str,
    version: &str,
    capabilities: Vec<String>,
    agents: Vec<athena_types::AgentId>,
    tools: Vec<ToolName>,
    perms: Vec<athena_types::Permission>,
    risk: RiskLevel,
) -> Skill {
    Skill {
        id: athena_types::SkillId::new(id),
        name: id.into(),
        version: version.into(),
        description: "selection fixture".into(),
        capabilities,
        required_tools: tools,
        required_permissions: perms,
        supported_agents: agents,
        dependencies: vec![],
        runtime_provider: "agent-cli".into(),
        source: "local".into(),
        checksum: format!("sha256:{id}@{version}"),
        publisher: "aquaxis".into(),
        risk_level: risk,
        state: SkillState::Enabled,
        hot_reload_safe: false,
        registered_at: "t".into(),
        updated_at: "t".into(),
    }
}

fn usable_agent() -> AgentFace {
    let mut permissions = vec![];
    for tool in ToolName::ALL {
        permissions.push(athena_types::Permission::Tool { tool: *tool });
    }
    permissions.push(athena_types::Permission::Fs {
        mode: athena_types::FsMode::Read,
        scope: "/w".into(),
    });
    AgentFace {
        allowed_tools: ToolName::ALL.to_vec(),
        permissions,
    }
}

// --- The five axes -------------------------------------------------

#[test]
fn the_chosen_skill_matches_every_axis_and_the_top_candidate_wins() {
    let candidates = [
        Candidate {
            skill: skill(
                "one",
                "1.0.0",
                vec!["summarize".into()],
                vec![],
                vec![],
                vec![],
                RiskLevel::Low,
            ),
            approval: None,
        },
        Candidate {
            skill: skill(
                "two",
                "1.0.0",
                vec!["summarize".into(), "report".into()],
                vec![],
                vec![],
                vec![],
                RiskLevel::Low,
            ),
            approval: None,
        },
    ];
    let chosen = select(
        "summarize",
        &candidates,
        &usable_agent(),
        "timing-lead",
        ToolName::ALL,
    )
    .expect("selected");
    assert_eq!(
        chosen.id.as_str(),
        "one",
        "the top-scored search-order candidate wins (finer scoring is P8-02)"
    );
}

#[test]
fn no_candidate_is_a_named_fact() {
    let fault = select(
        "ghost-capability",
        &[],
        &usable_agent(),
        "any",
        ToolName::ALL,
    );
    assert!(matches!(fault, Err(SelectionFault::NoCandidate)));
}

#[test]
fn each_axis_refuses_its_candidate() {
    // A wrong agent:
    let wrong_agent = Candidate {
        skill: skill(
            "the-skill",
            "1.0.0",
            vec!["summarize".into()],
            vec![athena_types::AgentId::new("other-agent")],
            vec![],
            vec![],
            RiskLevel::Low,
        ),
        approval: None,
    };
    let fault = select(
        "summarize",
        &[wrong_agent],
        &usable_agent(),
        "timing-lead",
        ToolName::ALL,
    );
    assert!(
        matches!(fault, Err(SelectionFault::AllRefused { .. })),
        "{fault:?}"
    );

    // A non-capable platform tool:
    let tool_needing = Candidate {
        skill: skill(
            "the-skill",
            "1.0.0",
            vec!["summarize".into()],
            vec![],
            vec![ToolName::Shell],
            vec![],
            RiskLevel::Low,
        ),
        approval: None,
    };
    let fault = select(
        "summarize",
        &[tool_needing],
        &usable_agent(),
        "any",
        &[ToolName::FsRead],
    );
    assert!(
        matches!(fault, Err(SelectionFault::AllRefused { .. })),
        "{fault:?}"
    );

    // A non-SemVer version:
    let bad_version = Candidate {
        skill: skill(
            "the-skill",
            "beta",
            vec!["summarize".into()],
            vec![],
            vec![],
            vec![],
            RiskLevel::Low,
        ),
        approval: None,
    };
    let fault = select(
        "summarize",
        &[bad_version],
        &usable_agent(),
        "any",
        ToolName::ALL,
    );
    assert!(matches!(fault, Err(SelectionFault::AllRefused { .. })));

    // High risk with no approval record:
    let risky = Candidate {
        skill: skill(
            "the-skill",
            "1.0.0",
            vec!["summarize".into()],
            vec![],
            vec![],
            vec![],
            RiskLevel::High,
        ),
        approval: None,
    };
    let fault = select("summarize", &[risky], &usable_agent(), "any", ToolName::ALL);
    assert!(
        matches!(fault, Err(SelectionFault::AllRefused { .. })),
        "risk axis refuses: {fault:?}"
    );

    // A permission exceed:
    let perm_needing = Candidate {
        skill: skill(
            "the-skill",
            "1.0.0",
            vec!["summarize".into()],
            vec![],
            vec![],
            vec![athena_types::Permission::Network {
                mode: athena_types::NetworkMode::Allow,
                hosts: None,
            }],
            RiskLevel::Low,
        ),
        approval: None,
    };
    let fault = select(
        "summarize",
        &[perm_needing],
        &usable_agent(),
        "any",
        ToolName::ALL,
    );
    assert!(
        matches!(fault, Err(SelectionFault::AllRefused { .. })),
        "{fault:?}"
    );
}

// --- The approval-record gate face -----------------------------------------

#[test]
fn the_approval_record_face_demands_one_for_high_risk() {
    assert!(!requires_approval_record(RiskLevel::Low));
    assert!(!requires_approval_record(RiskLevel::Medium));
    assert!(requires_approval_record(RiskLevel::High));
}

#[test]
fn a_high_risk_candidate_with_a_grant_is_selectable() {
    let risky = Candidate {
        skill: skill(
            "the-skill",
            "1.0.0",
            vec!["summarize".into()],
            vec![],
            vec![],
            vec![],
            RiskLevel::High,
        ),
        approval: Some(athena_types::ApprovalState::Granted),
    };
    let chosen =
        select("summarize", &[risky], &usable_agent(), "any", ToolName::ALL).expect("selected");
    assert_eq!(chosen.id.as_str(), "the-skill");
}

// --- P8-02: score-based skill selection --------------------------------------------

mod scoring_tests {
    use crate::selection::{
        SkillScoreFacts, rank_skill_scored, risk_term, skill_factor_scores, skill_total,
    };
    use athena_types::{RiskLevel, Skill};

    fn skill(id: &str, capability: &str, agents: Vec<&str>) -> Skill {
        super::skill(
            id,
            "1.0.0",
            vec![capability.to_string()],
            agents.into_iter().map(athena_types::AgentId::new).collect(),
            vec![],
            vec![],
            RiskLevel::Medium,
        )
    }

    #[test]
    fn the_factor_scores_walk_the_two_recorded_faces() {
        let offering = skill("a", "summarize", vec![]);
        let (capability, compatibility) = skill_factor_scores(&offering, "summarize", "any");
        assert_eq!(capability, 1.0);
        assert_eq!(
            compatibility, 1.0,
            "an empty supported_agents admits any agent"
        );

        let gated = skill("b", "other", vec!["timing-lead"]);
        let (capability, compatibility) = skill_factor_scores(&gated, "summarize", "any");
        assert_eq!(capability, 0.0);
        assert_eq!(
            compatibility, 0.0,
            "a listed roster that excludes the agent"
        );
    }

    #[test]
    fn the_risk_term_names_configuration_not_prose() {
        assert_eq!(risk_term(RiskLevel::Low), 0.0);
        assert_eq!(risk_term(RiskLevel::Medium), 0.5);
        assert_eq!(risk_term(RiskLevel::High), 1.0);
    }

    #[test]
    fn the_ordering_is_deterministic_and_absent_facts_are_unknown() {
        let candidates = vec![
            skill("beta", "summarize", vec![]),
            skill("alpha", "summarize", vec![]),
            skill("other", "else", vec![]),
        ];
        let facts = std::collections::BTreeMap::from([(
            "alpha".to_string(),
            SkillScoreFacts {
                quality: Some(0.9),
                ..SkillScoreFacts::default()
            },
        )]);
        let ranked = rank_skill_scored("summarize", "any", &candidates, &facts).expect("ranked");
        let names: Vec<String> = ranked.iter().map(|(skill, _)| skill.clone()).collect();
        assert_eq!(names, vec!["alpha", "beta"]);

        let alpha_total = skill_total(
            1.0,
            1.0,
            &SkillScoreFacts {
                quality: Some(0.9),
                ..SkillScoreFacts::default()
            },
        );
        assert!((alpha_total - 2.9).abs() < 1e-9, "{alpha_total}");
    }
}
