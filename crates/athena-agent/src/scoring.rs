//! Score-based agent selection (P8-01, spec §8.4 `ATH-AGENT-006`) and the evaluation
//! feedback that feeds it (P8-04, `ATH-AGENT-007`'s history).
//!
//! The spec's formula, as facts supplied by the caller — never invented here:
//!
//! ```text
//! Score = capability_score + reliability_score + availability_score
//!         + performance_score - cost_score
//! ```
//!
//! Every factor's value is a passed observation (the wiring reads them from recorded
//! history, live load, or configuration). An absent fact contributes nothing and is
//! reported as unknown in the rationale, so a scorer never fabricates reliability
//! (the facts-only discipline applies to selection too). The ordering is
//! deterministic: same roster and facts ⇒ same order; ties resolve by agent id.

use athena_types::AgentId;

/// The per-candidate observation facts (`ATH-AGENT-006`'s factors, each `None` when
/// unknown). Values are fractions in `[0.0, 1.0]`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ScoreFacts {
    /// Reliability: recorded acceptance rate over this agent's history (P8-04).
    pub reliability: Option<f64>,
    /// Availability: the agent's free-capacity face.
    pub availability: Option<f64>,
    /// Performance: the latency-derived speed face.
    pub performance: Option<f64>,
    /// Cost: subtracted per the formula.
    pub cost: Option<f64>,
}

/// Why a scored ranking refused.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ScoringFault {
    /// A supplied fact is outside the domain (`[0.0, 1.0]`) — a wiring bug the caller
    /// must fix, never a value the scorer clamps.
    #[error("{0}: fact {1} = {2} is outside [0, 1]")]
    OutOfDomain(String, &'static str, f64),
}

/// The capability factor: the fraction of the required set the agent carries (an exact
/// match is `1.0`; no requirement at all is a full match — nothing to contain).
#[must_use]
pub fn capability_score(required: &[String], capabilities: &[String]) -> f64 {
    if required.is_empty() {
        return 1.0;
    }
    let present = required
        .iter()
        .filter(|need| capabilities.contains(need))
        .count();
    present as f64 / required.len() as f64
}

/// Validate every present fact against the `[0.0, 1.0]` domain.
///
/// # Errors
/// [`ScoringFault::OutOfDomain`] — a wiring bug, never clamped away.
pub fn checked(agent_id: &AgentId, facts: &ScoreFacts) -> Result<(), ScoringFault> {
    for (name, value) in FACTORS
        .iter()
        .filter_map(|(name, getter)| getter(facts).map(|value| (*name, value)))
    {
        if !(0.0..=1.0).contains(&value) {
            return Err(ScoringFault::OutOfDomain(
                agent_id.as_str().to_owned(),
                name,
                value,
            ));
        }
    }
    Ok(())
}

/// The factor name + getter pair.
type FactorGetter = (&'static str, fn(&ScoreFacts) -> Option<f64>);

/// The factor names and their getters, in the formula's order.
const FACTORS: &[FactorGetter] = &[
    ("reliability", |facts| facts.reliability),
    ("availability", |facts| facts.availability),
    ("performance", |facts| facts.performance),
    ("cost", |facts| facts.cost),
];

/// The total score of one candidate per the §8.4 formula.
#[must_use]
pub fn total(capability_score: f64, facts: &ScoreFacts) -> f64 {
    capability_score
        + facts.reliability.unwrap_or(0.0)
        + facts.availability.unwrap_or(0.0)
        + facts.performance.unwrap_or(0.0)
        - facts.cost.unwrap_or(0.0)
}

/// The rationale lines for one candidate (absent facts named unknown; recorded
/// verbatim in `AgentSelected` — `ATH-AGENT-007`).
#[must_use]
pub fn rationale_lines(capability_score: f64, facts: &ScoreFacts) -> Vec<String> {
    let mut lines = vec![format!("capability {capability_score:.2}")];
    for (name, getter) in FACTORS {
        match getter(facts) {
            Some(value) => lines.push(format!("{name} {value:.2}")),
            None => lines.push(format!("{name} unknown")),
        }
    }
    lines
}

/// The scored ordering: candidates with their rationale, in descending score with ties
/// resolved by agent id — the ordering the Core Agent's judgement picks among, the
/// whole of it recorded with the selection (`ATH-AGENT-007`).
///
/// # Errors
/// [`ScoringFault::OutOfDomain`] when a fact is outside `[0.0, 1.0]`.
pub fn rank_scored(
    matched: &[(AgentId, f64, ScoreFacts)],
) -> Result<Vec<(AgentId, Vec<String>)>, ScoringFault> {
    for (agent_id, _capability, facts) in matched {
        checked(agent_id, facts)?;
    }
    let mut ranked: Vec<(AgentId, f64, Vec<String>)> = matched
        .iter()
        .map(|(agent_id, capability, facts)| {
            (
                agent_id.clone(),
                total(*capability, facts),
                rationale_lines(*capability, facts),
            )
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    Ok(ranked
        .into_iter()
        .map(|(agent_id, _score, why)| (agent_id, why))
        .collect())
}

/// The recorded per-agent outcomes the track record folds (`ATH-AGENT-007`'s history;
/// P8-04's evaluation feedback). Sourced from the store's Results/Evaluations rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    clippy::exhaustive_enums,
    reason = "the recorded verdict destinations are exhaustive"
)]
pub enum RecordedOutcome {
    /// The evaluation accepted the Result.
    Accepted,
    /// The evaluation sent the work back (retry / replan).
    SentBack,
    /// The row failed outright.
    Failed,
}

/// The reliability fact from an agent's recorded outcomes (P8-04): a Laplace-smoothed
/// acceptance rate — `(accepted + 1) / (total + 2)` — so a first-attempt agent holds a
/// neutral rate instead of a fabricated `1.0`, and history drives the score without
/// overcommitting on small samples. `None` when no history exists at all (the scoring
/// face reports it unknown).
#[must_use]
pub fn reliability(outcomes: &[RecordedOutcome]) -> Option<f64> {
    if outcomes.is_empty() {
        return None;
    }
    let accepted = outcomes
        .iter()
        .filter(|outcome| **outcome == RecordedOutcome::Accepted)
        .count();
    Some((accepted as f64 + 1.0) / (outcomes.len() as f64 + 2.0))
}

/// The performance fact from recorded latencies (P8-04): budget-normalized so faster
/// work scores higher, capped at the domain. `None` when no history exists.
#[must_use]
pub fn performance(elapsed_ms: &[u64], budget_ms: u64) -> Option<f64> {
    if elapsed_ms.is_empty() || budget_ms == 0 {
        return None;
    }
    let average = elapsed_ms.iter().sum::<u64>() as f64 / elapsed_ms.len() as f64;
    Some(((budget_ms as f64 / average.max(1.0)) * 0.5).clamp(0.0, 1.0))
}

#[cfg(test)]
#[path = "scoring_tests.rs"]
mod scoring_tests;
