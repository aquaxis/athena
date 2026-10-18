//! The agent surface's behaviour (spec §8; P2-01/03/04/05/06/07/08).

use crate::*;
use athena_types::{
    AgentDefinition, AgentId, AgentLevel, HierarchyNodeDraft, ModelRef, PlanStepId, ScopeRef,
    ToolName,
};

fn definition(agent: &str, level: AgentLevel, allowed: &[ToolName]) -> AgentDefinition {
    AgentDefinition {
        id: AgentId::new(agent),
        name: agent.replace('-', " "),
        level,
        description: "test definition".into(),
        capabilities: vec!["cap-agent".to_owned()],
        persona_ref: agent.to_string(),
        persona_version: "1.0.0".into(),
        allowed_tools: allowed.to_vec(),
        delegable_tools: vec![],
        default_skills: vec![],
        runtime: athena_types::RuntimeType::AgentCli,
        model: ModelRef {
            provider: "claude".into(),
            name: "claude-opus-5".into(),
        },
        max_delegation_depth: 4,
        dynamic: false,
    }
}

// --- P2-01: definition validation --------------------------------------------------------

#[test]
fn a_definition_without_model_coordinates_is_refused_at_registration() {
    // ATH-AGENT-008 / R-AGT-01: both model coordinates are mandatory; ATHENA has no
    // provider default.
    let mut silent = definition("rtl-worker", AgentLevel::Worker, &[ToolName::FsRead]);
    silent.model = ModelRef {
        provider: String::new(),
        name: "claude-opus-5".into(),
    };
    let refusal = agent_manager::validate_definition(&silent).expect_err("refused");
    assert_eq!(refusal.code(), "AGENT_MODEL_UNSPECIFIED");
    assert_eq!(refusal.class(), "ValidationError");

    let mut unnamed = definition("rtl-worker", AgentLevel::Worker, &[ToolName::FsRead]);
    unnamed.model = ModelRef {
        provider: "claude".into(),
        name: String::new(),
    };
    agent_manager::validate_definition(&unnamed).expect_err("refused as well");
}

#[test]
fn a_dynamic_definition_cannot_widen_the_universe() {
    // ATH-HIER-018: dynamic generation only recombines existing personas, capabilities
    // and permission subsets.
    let base = definition("rtl-worker", AgentLevel::Worker, &[ToolName::FsRead]);
    let director = {
        let mut wide = definition(
            "rtl-director",
            AgentLevel::Director,
            &[ToolName::FsRead, ToolName::FsWrite],
        );
        wide.capabilities = vec!["cap-report".into()];
        wide
    };
    let universe = KnownUniverse::from_definitions(&[base.clone(), director.clone()]);

    // A legal recombination: existing persona, existing capability, a subset of tools.
    let mut recombined = definition("rtl-worker-2", AgentLevel::Worker, &[ToolName::FsRead]);
    recombined.dynamic = true;
    recombined.persona_ref = "rtl-worker".into();
    recombined.capabilities = vec!["cap-report".into()];
    agent_manager::validate_dynamic(&recombined, &universe).expect("legal");

    // A persona nobody carries is refused.
    let mut unknown_persona = recombined.clone();
    unknown_persona.persona_ref = "brand-new-persona".into();
    let refusal =
        agent_manager::validate_dynamic(&unknown_persona, &universe).expect_err("refused");
    assert_eq!(refusal.code(), "AGENT_UNKNOWN_PERSONA");

    // A capability widening is refused.
    let mut wide_cap = recombined.clone();
    wide_cap.capabilities = vec!["cap-report".into(), "cap-shell".into()];
    assert_eq!(
        agent_manager::validate_dynamic(&wide_cap, &universe)
            .expect_err("refused")
            .code(),
        "AGENT_UNKNOWN_CAPABILITY"
    );

    // A permission widening is refused.
    let mut wide_tools = recombined;
    wide_tools.allowed_tools = vec![ToolName::FsRead, ToolName::Shell];
    assert_eq!(
        agent_manager::validate_dynamic(&wide_tools, &universe)
            .expect_err("refused")
            .code(),
        "AGENT_UNKNOWN_PERMISSION"
    );
}

// --- P2-03/04/05/06/07: the hierarchy composition ----------------------------------------

fn draft(agent: &str, level: AgentLevel, superior: Option<&AgentId>) -> HierarchyNodeDraft {
    HierarchyNodeDraft {
        agent_id: AgentId::new(agent),
        level,
        superior_agent_id: superior.cloned(),
        scope_ref: Some(ScopeRef::PlanStep {
            plan_step_id: PlanStepId::new(),
        }),
        sibling_acl: vec![],
    }
}

fn facts(
    agent: &str,
    level: AgentLevel,
    allowed: Vec<ToolName>,
    delegable: Vec<ToolName>,
) -> DefinitionFacts {
    DefinitionFacts {
        agent_id: AgentId::new(agent),
        level,
        allowed_tools: allowed,
        delegable_tools: delegable,
    }
}

#[test]
fn a_composition_rejects_duplicate_agents_cycles_and_absent_superiors() {
    // ATH-HIER-002/003: unique parentage, acyclic.
    let producer = AgentId::core_agent();
    let director = AgentId::new("rtl-director");
    let drafts = vec![
        HierarchyNodeDraft {
            agent_id: producer.clone(),
            level: AgentLevel::Producer,
            superior_agent_id: None,
            scope_ref: None,
            sibling_acl: vec![],
        },
        draft("director", AgentLevel::Director, Some(&producer)),
        draft("director", AgentLevel::Director, Some(&director)), // duplicate + cycle
    ];
    let err = compose(&drafts, &[], 4, &[]).expect_err("refused");
    assert!(matches!(err, CompositionError::DuplicateAgent(_)), "{err}");

    // Cycle: director claims itself as its own superior.
    let self_loop = vec![
        HierarchyNodeDraft {
            agent_id: producer.clone(),
            level: AgentLevel::Producer,
            superior_agent_id: None,
            scope_ref: None,
            sibling_acl: vec![],
        },
        HierarchyNodeDraft {
            agent_id: director.clone(),
            level: AgentLevel::Director,
            superior_agent_id: Some(director.clone()),
            scope_ref: None,
            sibling_acl: vec![],
        },
    ];
    assert!(matches!(
        compose(&self_loop, &[], 4, &[]),
        Err(CompositionError::Cycle(_))
    ));

    // An absent superior is refused before the level read.
    let orphan = vec![draft(
        "lead",
        AgentLevel::Lead,
        Some(&AgentId::new("ghost")),
    )];
    assert!(matches!(
        compose(&orphan, &[], 4, &[]),
        Err(CompositionError::AbsentSuperior(_))
    ));
}

#[test]
fn delegation_is_immediate_child_only_unless_explicitly_granted() {
    // ATH-HIER-004: a Worker directly under the Producer is a two-level skip; a
    // Lead→Director link is one. The recorded grant widens precisely that pair.
    let producer = AgentId::core_agent();
    let drafts = vec![
        HierarchyNodeDraft {
            agent_id: producer.clone(),
            level: AgentLevel::Producer,
            superior_agent_id: None,
            scope_ref: None,
            sibling_acl: vec![],
        },
        draft("worker", AgentLevel::Worker, Some(&producer)),
    ];
    let fact = facts("worker", AgentLevel::Worker, vec![], vec![]);
    let err = compose(&drafts, &[fact], 4, &[]).expect_err("refused");
    assert!(matches!(err, CompositionError::LevelSkip { .. }), "{err}");
    let _ = producer;
}

#[test]
fn the_depth_ceiling_blocks_and_reports_as_policy() {
    // ATH-HIER-005: chains deeper than the ceiling refuse as PolicyError.
    let producer = AgentId::core_agent();
    let director = AgentId::new("director");
    let lead = AgentId::new("lead");
    let worker = AgentId::new("worker");
    let _sub = AgentId::new("sub");
    let drafts = vec![
        HierarchyNodeDraft {
            agent_id: producer.clone(),
            level: AgentLevel::Producer,
            superior_agent_id: None,
            scope_ref: None,
            sibling_acl: vec![],
        },
        draft("director", AgentLevel::Director, Some(&producer)),
        draft("lead", AgentLevel::Lead, Some(&director)),
        draft("worker", AgentLevel::Worker, Some(&lead)),
        draft("sub-worker", AgentLevel::Worker, Some(&worker)),
    ];
    // `worker → sub-worker` breaks the level mapping, so grant the skip explicitly and
    // let ONLY the depth ceiling surface (the test's subject).
    let granted = vec![(AgentId::new("sub-worker"), AgentLevel::Worker)];
    let err = compose(&drafts, &[], 3, &granted).expect_err("depth 4 over ceiling 3");
    assert!(err.is_policy(), "{err}");
    assert_eq!(err.class(), "PolicyError");
    assert_eq!(err.code(), "HIER_DEPTH_CEILING");
}

#[test]
fn permission_containment_is_enforced_downward() {
    // ATH-HIER-006 / ATH-SEC-009: the child's allowed set must sit inside the superior's
    // delegable set, exactly the containment the whole design leans on.
    let producer = AgentId::core_agent();
    let drafts = vec![
        HierarchyNodeDraft {
            agent_id: producer.clone(),
            level: AgentLevel::Producer,
            superior_agent_id: None,
            scope_ref: None,
            sibling_acl: vec![],
        },
        draft("worker", AgentLevel::Worker, Some(&producer)),
    ];
    let escalation = [
        facts(producer.as_str(), AgentLevel::Producer, vec![], vec![]),
        facts("worker", AgentLevel::Worker, vec![ToolName::Shell], vec![]),
    ];
    let granted = vec![(AgentId::new("worker"), AgentLevel::Producer)];
    let refused = compose(&drafts, &escalation, 4, &granted).expect_err("refused");
    assert!(
        matches!(refused, CompositionError::PermissionEscalation { .. }),
        "a level-granted worker still escalates permissions: {refused:?}"
    );
    assert!(matches!(
        refused,
        CompositionError::PermissionEscalation { .. }
    ));
    assert_eq!(refused.code(), "HIER_PERMISSION_ESCALATION");
    assert_eq!(refused.class(), "ValidationError");
}

// --- P2-08: the capability-matching selection --------------------------------------------

#[test]
fn selection_reports_every_candidate_and_refuses_an_empty_pool() {
    // ATH-AGENT-004/005/007: matched pool at least one, rationale per candidate, no
    // silent assignment on a miss.
    let def = definition("rtl-worker", AgentLevel::Worker, &[ToolName::FsRead]);
    let def = AgentDefinition {
        capabilities: vec!["fpga".into(), "timing".into()],
        ..def
    };
    let roster = vec![def];
    let matched = select(&["fpga".into(), "timing".into()], &roster).expect("a match");
    assert_eq!(matched.matched.len(), 1);
    assert_eq!(matched.evaluated.len(), 1);
    assert!(matched.evaluated[0].matched);
    assert!(
        matched.rationale.iter().all(|(_, why)| !why.is_empty()),
        "every candidate carries its rationale"
    );

    // No match: refused never assigned.
    let refused = select(&["linux-kernel".into()], &roster).expect_err("no match");
    assert!(refused.to_string().contains("linux-kernel"));
}

// --- P2-18..P2-21: the persona loader ----------------------------------------------------

#[test]
fn the_persona_loader_carries_front_matter_and_levels() {
    // R-PER-01/02/03 + P2-18..P2-21: parse, level pairing, and grant containment.
    use crate::persona::{PersonaFault, PersonaLedger, front_matter, load_persona_from_text};
    let text = concat!(
        "---\n",
        "name: fpga timing lead\n",
        "role: lead\n",
        "level: lead\n",
        "allowed_tools:\n",
        "  - fs_read\n",
        "---\n\n",
        "Read the schedule; report the slack."
    );
    let defined = definition(
        "tpl-lead",
        AgentLevel::Lead,
        &[ToolName::FsRead, ToolName::SendTo],
    );
    let persona =
        load_persona_from_text(text, &AgentId::new("fpga-lead"), &defined).expect("loads");
    assert_eq!(persona.front_matter.name, "fpga timing lead");
    assert_eq!(persona.front_matter.level, AgentLevel::Lead);

    // R-PER-03: a persona asking for more than the definition grants is refused.
    let overreaching = text.replace("  - fs_read\n", "  - fs_read\n  - shell\n");
    let refusal = load_persona_from_text(&overreaching, &AgentId::new("fpga-lead"), &defined)
        .expect_err("refused");
    assert!(
        matches!(refusal, PersonaFault::PermissionExceeded(_)),
        "{refusal}"
    );
    assert_eq!(refusal.class(), "ValidationError");

    // The level pairing is checked too.
    let worker_definition = definition("wrong-worker", AgentLevel::Worker, &[ToolName::FsRead]);
    assert!(matches!(
        load_persona_from_text(text, &AgentId::new("fpga-lead"), &worker_definition)
            .expect_err("levels must match"),
        PersonaFault::LevelMismatched {
            front: AgentLevel::Lead,
            defined: AgentLevel::Worker
        }
    ),);

    // R-PER-05: the ledger records the identifier and version.
    let mut ledger = PersonaLedger::default();
    ledger.record(&AgentId::new("fpga-lead"), "leads/fpga-timing", "1.2.0");
    assert_eq!(
        ledger.history(&AgentId::new("fpga-lead")),
        vec![("leads/fpga-timing".to_string(), "1.2.0".to_string())]
    );
    let (bound_agent, bound_level) = front_matter(&persona);
    assert_eq!(bound_agent, AgentId::new("fpga-lead"));
    assert_eq!(bound_level, AgentLevel::Lead);
}

// --- P2-13: sibling-ACL recording at composition ------------------------------------------

#[test]
fn the_sibling_acl_rides_the_settled_nodes_untouched() {
    // ATH-MSG-018: the ACL is Core Agent's compose_hierarchy record — composition records
    // it verbatim (it must not edit, widen, or drop it); the delivery-time check belongs
    // to the Broker (athena-messaging::route).
    let producer = AgentId::core_agent();
    let lead_a = AgentId::new("rtl-lead-a");
    let lead_b = AgentId::new("rtl-lead-b");
    let drafts = vec![
        HierarchyNodeDraft {
            agent_id: producer.clone(),
            level: AgentLevel::Producer,
            superior_agent_id: None,
            scope_ref: None,
            sibling_acl: vec![],
        },
        HierarchyNodeDraft {
            agent_id: lead_a.clone(),
            level: AgentLevel::Director,
            superior_agent_id: Some(producer.clone()),
            scope_ref: None,
            sibling_acl: vec![lead_b.clone()],
        },
        HierarchyNodeDraft {
            agent_id: lead_b.clone(),
            level: AgentLevel::Director,
            superior_agent_id: Some(producer.clone()),
            scope_ref: None,
            sibling_acl: vec![lead_a.clone()],
        },
    ];
    let defs = vec![
        facts("athena-core-agent", AgentLevel::Producer, vec![], vec![]),
        facts("rtl-lead-a", AgentLevel::Director, vec![], vec![]),
        facts("rtl-lead-b", AgentLevel::Director, vec![], vec![]),
    ];
    let settled = compose(&drafts, &defs, 4, &[]).expect("composes");
    let of = |agent: &AgentId| {
        settled
            .iter()
            .find(|node| &node.agent_id == agent)
            .expect("node")
    };
    assert_eq!(of(&lead_a).sibling_acl, vec![lead_b.clone()]);
    assert_eq!(of(&lead_b).sibling_acl, vec![lead_a]);
    assert!(of(&producer).sibling_acl.is_empty());

    // An empty request stays empty — a composition without grants opens no sibling route.
    let mut no_acl = drafts.clone();
    no_acl[1].sibling_acl = vec![];
    no_acl[2].sibling_acl = vec![];
    let settled = compose(&no_acl, &defs, 4, &[]).expect("composes");
    assert!(
        settled.iter().all(|node| node.sibling_acl.is_empty()),
        "no ACL recorded means no sibling channel granted"
    );
}

// --- P2-19: the shipped level personas load through the real loader ------------------------

#[test]
fn the_shipped_level_personas_load_through_the_persona_loader() {
    // P2-19's drift guard: the distributed defaults (`personas/director.md`,
    // `personas/lead.md`, `personas/worker.md`) must parse through the real loader, pair
    // their level, and stay inside the minimal granted set a definition carries
    // (R-PER-03). Any future edit that breaks the front-matter contract or the
    // minimal-tool policy fails here before it can ship.
    use crate::persona::load_persona_from_text;
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../personas");
    let cases = [
        ("director.md", AgentLevel::Director),
        ("lead.md", AgentLevel::Lead),
        ("worker.md", AgentLevel::Worker),
    ];
    // The minimal working set: read and write inside the Workspace, and send reports
    // (ATH-SEC-008's minimal-default policy; the aiprj convention needs exactly this).
    let granted = [ToolName::FsRead, ToolName::FsWrite, ToolName::SendTo];
    for (file, level) in cases {
        let path = format!("{dir}/{file}");
        let text = std::fs::read_to_string(&path).expect("the shipped persona exists");
        let defined = definition(&format!("tpl-{file}"), level, &granted);
        let agent_id = AgentId::new(format!("shipped-{file}"));
        let persona = load_persona_from_text(&text, &agent_id, &defined)
            .unwrap_or_else(|fault| panic!("{file} must load through the loader: {fault}"));
        assert_eq!(persona.front_matter.level, level, "{file} level pairing");
        // No tool outside the grant, and none beyond the three minimal ones.
        assert!(
            persona
                .front_matter
                .allowed_tools
                .iter()
                .all(|tool| granted.contains(tool)),
            "{file} carries only the minimal working set"
        );
    }
}
