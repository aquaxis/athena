//! The report-aggregation checks (P2-20; ATH-HIER-015, ATH-HIER-016).
//!
//! The spec fixes two duties up the chain — a Lead aggregates and evaluates its Workers'
//! Results **before** reporting upward and never relays a Worker's Result verbatim
//! (ATH-HIER-015), and reports aggregate at every level (ATH-HIER-016). What ATHENA can
//! enforce mechanically, without ever replacing the evaluating agent's judgement, are
//! structural facts about the report flow:
//!
//! 1. **Coverage**: a node with subordinates owes an aggregated report. Its upward report
//!    must come *after* every subordinate's report in time — a Lead cannot report on work
//!    whose results it has not yet received.
//! 2. **Verbatim relay**: a subordinate's full output text carried contiguously inside the
//!    upward report is the pass-through the rule forbids.
//! 3. **Unresolved inputs**: an upward report claimed as `accept` while a subordinate's
//!    digest carries a non-accept verdict has not completed its aggregation.
//!    [`AggregateReport::breach`] surfaces both the relay and the unresolved case.
//!
//! Nothing here is refused: `report` is a never-dropped kind (ATH-MSG-012), persisted and
//! acknowledged even when a check names it. The recorded observation is what feeds the
//! evaluator's re-aggregation request — the *consequence* is the deciding agent's
//! (ATH-CORE-022), never Core Host's takeover (ATH-CORE-002). Level legality is not this
//! surface's business at all: it is the Hierarchy Manager's own check (ATH-HIER-004), and
//! the aggregation rule is level-blind (the same shape applies at every level).
//!
//! The checks are pure: the caller (Core Host's Broker driver / Result flow) passes the
//! message facts read from the store, so the crate's dependency list stays `athena-types`
//! (spec §32.2).

use athena_types::Verdict;
use std::collections::BTreeMap;

/// The stable wire code recorded on an aggregation observation.
pub const RELAY_OBSERVED: &str = "AGGREGATION_VERBATIM_RELAY";
/// The stable wire code when the coverage obligation is unmet.
pub const NOT_AGGREGATED: &str = "AGGREGATION_NOT_BEFORE_SUBORDINATES";

/// The aggregation rank of a level (ATH-HIER-016's chain direction): reports aggregate
/// upward, so a higher rank reports over lower-ranked inputs.
///
/// Worker < Lead < Director < Producer.
#[must_use]
pub fn aggregation_rank_of(level: &str) -> u8 {
    match level {
        "worker" => 0,
        "lead" => 1,
        "director" => 2,
        "producer" => 3,
        // An unknown level ranks lowest so it can never report over a known one.
        _ => 0,
    }
}

/// The relay observation for one subordinate whose output was passed through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerbatimRelay {
    /// The subordinate whose output rode the upward report verbatim.
    pub subordinate: String,
    /// Why the observation fired (the rule and the compared texts' shape).
    pub detail: String,
}

impl VerbatimRelay {
    /// The event material: (wire code, policy id). The recorded observation is what the
    /// evaluator's consequence cites.
    #[must_use]
    pub fn observation(&self) -> (&'static str, &'static str) {
        (RELAY_OBSERVED, "hier.aggregation")
    }
}

/// Why an upward report fails the aggregation contract (observations, never refusals).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AggregationBreach {
    /// A subordinate's full output rides the upward report verbatim (ATH-HIER-015's
    /// MUST NOT).
    VerbatimRelay {
        /// The relayed subordinate.
        subordinate: String,
    },
    /// The report claims `accept` while an input's verdict is unresolved
    /// (ATH-HIER-015: aggregate *and evaluate* before reporting).
    UnresolvedInputs {
        /// The subordinates whose verdicts are not yet resolved.
        agents: Vec<String>,
    },
}

impl std::fmt::Display for AggregationBreach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::VerbatimRelay { subordinate } => write!(
                f,
                "the upward report relays {subordinate}'s result verbatim (ATH-HIER-015)"
            ),
            Self::UnresolvedInputs { agents } => write!(
                f,
                "the upward report claims completion over unresolved inputs: {} (ATH-HIER-015)",
                agents.join(", ")
            ),
        }
    }
}

/// One subordinate's report digest, as folded into an aggregation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportDigest {
    /// The subordinate agent.
    pub agent_id: String,
    /// The summary of what it produced.
    pub summary: String,
    /// The digest's verdict state; only `Accept` counts as resolved.
    pub verdict: Verdict,
}

/// An upward report claimed as an evaluated aggregation over its inputs
/// (ATH-HIER-015's shape). The breach check is a pure derivation over the recorded
/// facts, so the same report reads the same way everywhere it is replayed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregateReport {
    /// The digests the report aggregates over.
    pub inputs: Vec<ReportDigest>,
    /// The upward report's own text.
    pub upward_text: String,
    /// The reporter's claimed evaluation rationale.
    pub rationale: String,
}

impl AggregateReport {
    /// Collect an aggregation report over its input digests (not yet claimed).
    #[must_use]
    pub fn collect(inputs: &[ReportDigest], upward_text: impl Into<String>) -> Self {
        Self {
            inputs: inputs.to_vec(),
            upward_text: upward_text.into(),
            rationale: String::new(),
        }
    }

    /// Claim the report as an evaluated aggregation with the reporter's rationale.
    #[must_use]
    pub fn into_accepted(self, rationale: impl Into<String>) -> AggregationOutcome {
        let breach = self.breach();
        AggregationOutcome {
            report: Self {
                rationale: rationale.into(),
                ..self
            },
            breach,
        }
    }

    /// The aggregation breach, `None` when the report satisfies the contract.
    ///
    /// A verbatim relay takes precedence: it means no aggregation happened at all,
    /// which is the stronger observation.
    #[must_use]
    pub fn breach(&self) -> Option<AggregationBreach> {
        if let Some(relay) = self
            .verbatim_relays()
            .first()
            .map(|relay| relay.subordinate.clone())
        {
            return Some(AggregationBreach::VerbatimRelay { subordinate: relay });
        }
        let unresolved: Vec<String> = self
            .inputs
            .iter()
            .filter(|input| input.verdict != Verdict::Accept)
            .map(|input| input.agent_id.clone())
            .collect();
        if !unresolved.is_empty() {
            return Some(AggregationBreach::UnresolvedInputs { agents: unresolved });
        }
        None
    }

    /// Whether the report carries a completed evaluation (no breach).
    #[must_use]
    pub fn is_evaluated(&self) -> bool {
        self.breach().is_none()
    }

    /// The relayed subordinate, when the report is a pass-through.
    #[must_use]
    pub fn relayed_verbatim(&self) -> Option<String> {
        match self.breach() {
            Some(AggregationBreach::VerbatimRelay { subordinate }) => Some(subordinate),
            _ => None,
        }
    }

    /// Every verbatim relay in the report, ordered by subordinate (the raw observations).
    #[must_use]
    pub fn verbatim_relays(&self) -> Vec<VerbatimRelay> {
        verbatim_relays(
            &self.upward_text,
            &self
                .inputs
                .iter()
                .map(|input| (input.agent_id.clone(), input.summary.clone()))
                .collect::<Vec<_>>(),
        )
    }
}

/// The outcome of claiming an aggregation report as accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregationOutcome {
    /// The claimed report.
    pub report: AggregateReport,
    /// The breach observed at claim time, `None` when the claim stands.
    breach: Option<AggregationBreach>,
}

impl AggregationOutcome {
    /// Unwrap the report, panicking when the claim does not stand.
    ///
    /// # Panics
    /// When the aggregation contract is breached — the caller asked for a standing
    /// report and the facts refuse.
    #[track_caller]
    #[must_use]
    pub fn expect(self, message: &str) -> AggregateReport {
        match self.breach {
            None => self.report,
            Some(breach) => panic!("{message}: {breach}"),
        }
    }

    /// The observed breach, if any.
    #[must_use]
    pub fn breach(&self) -> Option<AggregationBreach> {
        self.breach.clone()
    }

    /// Whether the claim stands.
    #[must_use]
    pub fn is_evaluated(&self) -> bool {
        self.breach.is_none()
    }

    /// The relayed subordinate, when the report is a pass-through.
    #[must_use]
    pub fn relayed_verbatim(&self) -> Option<String> {
        self.report.relayed_verbatim()
    }
}

/// One subordinate's report fact, as read from the message store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubordinateReport {
    /// The subordinate agent.
    pub agent_id: String,
    /// The RFC 3339 UTC time its report arrived (ATH-DATA-002).
    pub reported_at: String,
}

/// The facts the coverage check needs for one upward report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportFacts {
    /// When the upward report arrived.
    pub upward_reported_at: String,
    /// The subordinates' report arrivals, with their times.
    pub subordinate_reports: Vec<SubordinateReport>,
}

/// The coverage obligation derived from the settled composition: a node holding the given
/// subordinates owes an aggregated report over precisely their results.
///
/// Empty subordinate sets owe nothing — a Worker's own report is the leaf result.
#[must_use]
pub fn obligation_for(subordinates: &[String]) -> Vec<String> {
    let mut owed: Vec<String> = subordinates.to_vec();
    owed.sort();
    owed.dedup();
    owed
}

/// The unmet half of the coverage obligation: subordinates whose report is missing
/// entirely or arrived **after** the upward report — a Lead cannot have aggregated a
/// result it had not yet received. RFC 3339 UTC stamps compare lexicographically
/// (ATH-DATA-002 fixes the format); equality counts as covered, ordering is what the
/// rule demands ("aggregate … before reporting upward").
///
/// The returned ids are sorted, so the observation is reproducible from the same facts.
#[must_use]
pub fn uncovered(facts: &ReportFacts, owed: &[String]) -> Vec<String> {
    let received: BTreeMap<&str, &str> = facts
        .subordinate_reports
        .iter()
        .map(|report| (report.agent_id.as_str(), report.reported_at.as_str()))
        .collect();
    let mut unmet = Vec::new();
    for agent in owed {
        match received.get(agent.as_str()) {
            // Missing: nothing to aggregate over.
            None => unmet.push(agent.clone()),
            // Present but later than the upward report: aggregation could not have read it.
            Some(at) if *at > facts.upward_reported_at.as_str() => unmet.push(agent.clone()),
            Some(_) => {}
        }
    }
    unmet
}

/// The verbatim-relay check: which of the subordinates' outputs pass through into the
/// upward report. The structural floor is deliberately narrow — false positives here
/// would name honest aggregations, and an *evaluative* judgement is Core's to never make:
///
/// - **Exact equality**: the upward report *is* the subordinate's output.
/// - **Stitched listing**: the body consists of the outputs glued with at most separator
///   characters (whitespace, punctuation, bullets) and **no added words** — e.g.
///   `"A: <out-a>; B: <out-b>"`. Stripping every contained output must leave only
///   separators. A folded sentence that covers the results in the reporter's own words
///   (`"<out-a> and the timing case"`) leaves prose behind and stays below the floor,
///   which is exactly what `ATH-HIER-015` wants distinguishable.
///
/// Nothing is refused here — the observation is recorded and the treatment (re-request a
/// real aggregation, reject the evaluation, re-plan) belongs to the deciding agent.
#[must_use]
pub fn verbatim_relays(
    upward_report_body: &str,
    subordinate_outputs: &[(String, String)],
) -> Vec<VerbatimRelay> {
    let body = upward_report_body.trim();
    let mut relays = Vec::new();
    if body.is_empty() {
        return relays;
    }
    let mut candidates: Vec<(&str, &str)> = subordinate_outputs
        .iter()
        .map(|(agent, output)| (agent.as_str(), output.trim()))
        .filter(|(_, output)| !output.is_empty())
        .collect();
    // Longest first: a stitched listing strips overlaps greedily from the body.
    candidates.sort_by(|a, b| b.1.chars().count().cmp(&a.1.chars().count()));

    for (agent, output) in &candidates {
        if body == *output {
            relays.push(VerbatimRelay {
                subordinate: (*agent).to_string(),
                detail: format!(
                    "the upward report is {agent}'s result output verbatim \
                     ({len} chars, ATH-HIER-015 forbids the pass-through)",
                    len = body.chars().count()
                ),
            });
        }
    }
    if !relays.is_empty() {
        relays.sort_by(|a, b| a.subordinate.cmp(&b.subordinate));
        return relays;
    }

    // The stitched-listing floor: strip each contained output and require the remainder
    // to be separators only.
    let mut leftover = body.to_string();
    let mut stripped: Vec<&str> = Vec::new();
    for (agent, output) in &candidates {
        if let Some(pos) = leftover.find(output) {
            leftover.replace_range(pos..pos + output.chars().count(), "");
            stripped.push(agent);
        }
    }
    if !stripped.is_empty() && leftover.chars().all(is_separator_char) {
        for agent in stripped {
            relays.push(VerbatimRelay {
                subordinate: agent.to_string(),
                detail: format!(
                    "the upward report lists {agent}'s result output with no added wording \
                     (a stitched listing, ATH-HIER-015 forbids the pass-through)"
                ),
            });
        }
    }
    relays.sort_by(|a, b| a.subordinate.cmp(&b.subordinate));
    relays
}

/// Whether a character may appear between verbatim outputs in a stitched listing.
const fn is_separator_char(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            ';' | ':'
                | ','
                | '.'
                | '-'
                | '*'
                | '+'
                | '|'
                | '/'
                | '('
                | ')'
                | '['
                | ']'
                | '"'
                | '\''
                | '·'
                | '—'
                | '、'
                | '。'
        )
}

#[cfg(test)]
#[path = "aggregation_tests.rs"]
mod aggregation_tests;
