//! Capability-matching skill selection (P5-14; spec §12.6 / `ATH-SKILL-014`,
//! `ATH-SKILL-033`): from the Catalog search results over one needed capability to the
//! single chosen Skill Set — or the refusal whose fact is the Core Agent's
//! `SkillSelected`-less fallback.
//!
//! The five checks named in the spec's selection shape, each as a passed fact:
//!
//! 1. **Capability match** — the chosen skill offers the needed capability.
//! 2. **Agent compatibility** — the Agent supports the skill (`supported_agents`
//!    empty = any agent; listed = exact membership).
//! 3. **Tool availability** — the skill's `required_tools` sit inside both the
//!    agent's grant and the platform face (P5-06's two-axis rule, re-stated here as
//!    the *choice* gate rather than the closure gate).
//! 4. **Version compatibility** — the skill's version parses as a SemVer (`ATH-SKILL-020`'s
//!    pin face) and satisfies the needing edge when one is present.
//! 5. **Risk / permission** — `high` requires approval (`ATH-SKILL-024`), and every
//!    required permission sits inside the Agent's grant (`ATH-SKILL-023`).
//!
//! The choice among the candidates: deterministic — the top-scored candidate in the
//! search order (the score is capabilities-offered; the finer scoring is a Phase-8
//! build, P8-02).

use crate::resolver::AgentFace;
use crate::skill_use::approve_skill_use;
use athena_types::{ApprovalState, RiskLevel, Skill, ToolName};

/// Why a selection refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SelectionFault {
    /// No candidate offered the capability.
    #[error("no ENABLED skill offers the capability")]
    NoCandidate,
    /// Every candidate failed a selection axis; the reasons ride per candidate.
    #[error("every candidate refused: {reasons:?}")]
    AllRefused {
        /// Per-candidate reasons, in search order.
        reasons: Vec<String>,
    },
}

/// The selection's agent-side facts beyond [`AgentFace`]: the approval record for the
/// arriving high-risk skill, and the meaning of `supported_agents` for *this* agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The skill.
    pub skill: Skill,
    /// The approval state for this skill's use, when one exists (`ATH-SKILL-024`).
    pub approval: Option<ApprovalState>,
}

/// Select the Skill Set for one capability.
///
/// # Errors
/// [`SelectionFault`] with the refusal fact.
pub fn select(
    capability: &str,
    candidates: &[Candidate],
    agent: &AgentFace,
    agent_slug: &str,
    platform_tools: &[ToolName],
) -> Result<Skill, SelectionFault> {
    let mut reasons = Vec::new();
    for candidate in candidates {
        let skill = &candidate.skill;
        // 1 — the capability face (a search's postcondition re-asserted).
        if !skill
            .capabilities
            .iter()
            .any(|offered| offered == capability)
        {
            reasons.push(format!("{}: no capability", skill.id.as_str()));
            continue;
        }
        // 2 — the agent compatibility face.
        let supports_agent = skill.supported_agents.is_empty()
            || skill
                .supported_agents
                .iter()
                .any(|supported| supported.as_str() == agent_slug);
        if !supports_agent {
            reasons.push(format!(
                "{}: not supported for agent {agent_slug:?}",
                skill.id.as_str()
            ));
            continue;
        }
        // 3 — the two-axis tool gate.
        if let Some(refusal) = tools_refused(&skill.required_tools, agent, platform_tools) {
            reasons.push(format!("{}: {refusal}", skill.id.as_str()));
            continue;
        }
        // 4 — the version pin parses (`ATH-SKILL-020`).
        if !crate::manifest::Manifest::is_semver(&skill.version) {
            reasons.push(format!("{}: version {}", skill.id.as_str(), skill.version));
            continue;
        }
        // 5 — the risk / permission gate (`ATH-SKILL-024`, `ATH-SKILL-023`).
        if let Err(fault) =
            approve_skill_use(skill.id.as_str(), skill.risk_level, candidate.approval)
        {
            reasons.push(format!("{}: {fault}", skill.id.as_str()));
            continue;
        }
        let mut exceeded = false;
        for required in &skill.required_permissions {
            if !agent.permissions.contains(required) {
                reasons.push(format!("{}: permission", skill.id.as_str()));
                exceeded = true;
                break;
            }
        }
        if exceeded {
            continue;
        }
        return Ok(skill.clone());
    }
    if reasons.is_empty() {
        Err(SelectionFault::NoCandidate)
    } else {
        Err(SelectionFault::AllRefused { reasons })
    }
}

fn tools_refused(
    required: &[ToolName],
    agent: &AgentFace,
    platform: &[ToolName],
) -> Option<String> {
    for tool in required {
        if !agent.allowed_tools.contains(tool) {
            return Some(format!("tool {} not granted", tool.as_str()));
        }
        if !platform.contains(tool) {
            return Some(format!("tool {} not capable", tool.as_str()));
        }
    }
    None
}

/// Whether the selection's risk demands an approval record before the SkillSelected
/// Event fires (the wiring's gate; `ATH-SKILL-024`).
#[must_use]
pub fn requires_approval_record(risk: RiskLevel) -> bool {
    risk == RiskLevel::High
}

// --- P8-02: score-based skill selection (spec §12.6, `ATH-SKILL-015`) -------------

/// The scored factors as supplied facts — never invented here (an absent fact
/// contributes `None` and reads *unknown* in the rationale). Values in `[0.0, 1.0]`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SkillScoreFacts {
    /// Quality: the recorded usage quality of the skill's runs.
    pub quality: Option<f64>,
    /// Reliability: the recorded acceptance rate of the skill's runs.
    pub reliability: Option<f64>,
    /// Availability: the skill's present readiness face.
    pub availability: Option<f64>,
    /// Cost: subtracted per the formula.
    pub cost: Option<f64>,
    /// Risk: subtracted per the formula (the configured risk face, never the agent's
    /// prose — `ATH-CORE-038`).
    pub risk: Option<f64>,
}

/// Why a scored skill ranking refused.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SkillScoringFault {
    /// A fact outside `[0.0, 1.0]` — a wiring bug, never clamped away.
    #[error("{0}: fact {1} = {2} is outside [0, 1]")]
    OutOfDomain(String, &'static str, f64),
}

/// The factor name + getter pair.
type SkillFactorGetter = (&'static str, fn(&SkillScoreFacts) -> Option<f64>);

/// The factor set in the formula's order (`ATH-SKILL-015`).
const SKILL_FACTORS: &[SkillFactorGetter] = &[
    ("quality", |facts| facts.quality),
    ("reliability", |facts| facts.reliability),
    ("availability", |facts| facts.availability),
    ("cost", |facts| facts.cost),
    ("risk", |facts| facts.risk),
];

/// Validate every present fact against the `[0.0, 1.0]` domain.
///
/// # Errors
/// [`SkillScoringFault::OutOfDomain`].
pub fn check_facts(skill_id: &str, facts: &SkillScoreFacts) -> Result<(), SkillScoringFault> {
    for (name, value) in SKILL_FACTORS
        .iter()
        .filter_map(|(name, getter)| getter(facts).map(|value| (*name, value)))
    {
        if !(0.0..=1.0).contains(&value) {
            return Err(SkillScoringFault::OutOfDomain(
                skill_id.to_owned(),
                name,
                value,
            ));
        }
    }
    Ok(())
}

/// The capability factor (1.0 when the skill offers the needed capability — the
/// search's postcondition) and the compatibility factor (1.0 when `supported_agents`
/// admits the agent — empty meaning any agent).
#[must_use]
pub fn skill_factor_scores(skill: &Skill, capability: &str, agent_slug: &str) -> (f64, f64) {
    let capability = if skill
        .capabilities
        .iter()
        .any(|offered| offered == capability)
    {
        1.0
    } else {
        0.0
    };
    let compatibility = if skill.supported_agents.is_empty()
        || skill
            .supported_agents
            .iter()
            .any(|supported| supported.as_str() == agent_slug)
    {
        1.0
    } else {
        0.0
    };
    (capability, compatibility)
}

/// The risk term the formula subtracts, from the configuration-declared risk face
/// (`ATH-SKILL-024`'s roster; never agent prose).
#[must_use]
pub fn risk_term(risk: RiskLevel) -> f64 {
    match risk {
        RiskLevel::Low => 0.0,
        RiskLevel::Medium => 0.5,
        // `exhaustive_enums` rationale: the three levels are the whole domain.
        RiskLevel::High => 1.0,
    }
}

/// The total per the §12.6 formula.
#[must_use]
pub fn skill_total(capability: f64, compatibility: f64, facts: &SkillScoreFacts) -> f64 {
    capability
        + compatibility
        + facts.quality.unwrap_or(0.0)
        + facts.reliability.unwrap_or(0.0)
        + facts.availability.unwrap_or(0.0)
        - facts.cost.unwrap_or(0.0)
        - facts.risk.unwrap_or(0.0)
}

/// The rationale lines (absent facts named unknown).
#[must_use]
pub fn skill_rationale_lines(
    capability: f64,
    compatibility: f64,
    facts: &SkillScoreFacts,
) -> Vec<String> {
    let mut lines = vec![
        format!("capability {capability:.2}"),
        format!("compatibility {compatibility:.2}"),
    ];
    for (name, getter) in SKILL_FACTORS {
        match getter(facts) {
            Some(value) => lines.push(format!("{name} {value:.2}")),
            None => lines.push(format!("{name} unknown")),
        }
    }
    lines
}

/// The scored ordering over the *gate-passing* candidates (the five selection axes run
/// first; the score only orders among the admissible — scoring is not an alternative to
/// permission). Descending total; ties by skill id then version, both sortable strings
/// here because the roster's shapes carry them.
///
/// # Errors
/// [`SkillScoringFault::OutOfDomain`].
pub fn rank_skill_scored(
    capability: &str,
    agent_slug: &str,
    gate_passed: &[Skill],
    facts_by_skill: &std::collections::BTreeMap<String, SkillScoreFacts>,
) -> Result<Vec<(String, Vec<String>)>, SkillScoringFault> {
    let mut ranked: Vec<(String, f64, Vec<String>)> = Vec::with_capacity(gate_passed.len());
    for skill in gate_passed {
        let facts = facts_by_skill
            .get(skill.id.as_str())
            .cloned()
            .unwrap_or_default();
        check_facts(skill.id.as_str(), &facts)?;
        let (capability_score, compatibility) = skill_factor_scores(skill, capability, agent_slug);
        // The offer is the search's postcondition — a roster entry without the
        // capability is not a candidate at all (never scored below an offering one).
        if capability_score == 0.0 {
            continue;
        }
        ranked.push((
            skill.id.as_str().to_owned(),
            skill_total(capability_score, compatibility, &facts),
            skill_rationale_lines(capability_score, compatibility, &facts),
        ));
    }
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    Ok(ranked
        .into_iter()
        .map(|(skill_id, _total, why)| (skill_id, why))
        .collect())
}

#[cfg(test)]
mod selection_tests;
