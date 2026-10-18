//! Tests for score-based agent selection (P8-01) and the track record (P8-04).

use crate::scoring::{
    RecordedOutcome, ScoreFacts, capability_score, performance, rank_scored, reliability, total,
};
use athena_types::AgentId;

fn id(slug: &str) -> AgentId {
    AgentId::new(slug)
}

#[test]
fn the_capability_factor_is_the_required_fraction() {
    let required = ["systemverilog".to_string(), "rtl-analysis".to_string()];
    assert_eq!(
        capability_score(
            &required,
            &[
                "systemverilog".into(),
                "rtl-analysis".into(),
                "synthesis".into()
            ]
        ),
        1.0
    );
    assert_eq!(
        capability_score(&required, &["systemverilog".into()]),
        1.0 / 2.0
    );
    // No requirement at all is a full match (nothing to contain).
    assert_eq!(capability_score(&[], &[]), 1.0);
}

#[test]
fn the_formula_sums_known_facts_and_names_unknowns_in_the_rationale() {
    let full = (
        id("a"),
        1.0,
        ScoreFacts {
            reliability: Some(0.8),
            availability: Some(0.5),
            performance: None,
            cost: Some(0.1),
        },
    );
    let ranked = rank_scored(&[full]).expect("ranked");
    let (agent, rationale) = &ranked[0];
    assert_eq!(agent, &id("a"));
    // 1.0 + 0.8 + 0.5 + 0.0 - 0.1
    assert!(
        rationale
            .iter()
            .any(|line| line.contains("reliability 0.80"))
    );
    assert!(
        rationale
            .iter()
            .any(|line| line.contains("performance unknown"))
    );
    let scored_total = total(
        1.0,
        &ScoreFacts {
            reliability: Some(0.8),
            availability: Some(0.5),
            performance: None,
            cost: Some(0.1),
        },
    );
    assert!((scored_total - 2.2).abs() < 1e-9, "{scored_total}");
}

#[test]
fn the_ordering_is_deterministic_and_ties_resolve_by_agent_id() {
    let matched = vec![
        (
            id("beta"),
            1.0,
            ScoreFacts {
                reliability: Some(0.5),
                ..ScoreFacts::default()
            },
        ),
        (
            id("alpha"),
            1.0,
            ScoreFacts {
                reliability: Some(0.5),
                ..ScoreFacts::default()
            },
        ),
        (
            id("top"),
            1.0,
            ScoreFacts {
                reliability: Some(0.9),
                ..ScoreFacts::default()
            },
        ),
    ];
    let ranked = rank_scored(&matched).expect("ranked");
    let names: Vec<String> = ranked
        .iter()
        .map(|(agent, _)| agent.as_str().to_owned())
        .collect();
    assert_eq!(names, vec!["top", "alpha", "beta"]);
}

#[test]
fn a_fact_outside_the_domain_refuses_rather_than_clamps() {
    let bad = [(
        id("a"),
        1.0,
        ScoreFacts {
            reliability: Some(1.4),
            ..ScoreFacts::default()
        },
    )];
    let refusal = rank_scored(&bad).expect_err("out-of-domain refuses");
    assert!(
        matches!(refusal, crate::scoring::ScoringFault::OutOfDomain(_, _, _)),
        "{refusal}"
    );
}

#[test]
fn the_track_record_is_a_smoothed_acceptance_rate() {
    // No history is unknown — never a fabricated 1.0.
    assert_eq!(reliability(&[]), None);
    // (3 accepted + 1) / (4 + 2).
    let history = [
        RecordedOutcome::Accepted,
        RecordedOutcome::Accepted,
        RecordedOutcome::Accepted,
        RecordedOutcome::Failed,
    ];
    let rate = reliability(&history).expect("rate");
    assert!((rate - 4.0 / 6.0).abs() < f64::EPSILON);
    // A first-attempt agent holds the neutral 0.5.
    assert!((reliability(&[RecordedOutcome::Accepted]).unwrap() - 2.0 / 3.0).abs() < 1e-9);
}

#[test]
fn the_performance_factor_is_budget_normalized_and_never_invented() {
    assert_eq!(performance(&[], 60_000), None);
    assert_eq!(performance(&[1_000], 0), None);
    // Elapsed equal to the budget: exactly half credit.
    let on_budget = performance(&[60_000], 60_000).expect("scored");
    assert!((on_budget - 0.5).abs() < f64::EPSILON);
    // Twice as fast: double credit, capped at the domain.
    let fast = performance(&[30_000, 30_000], 60_000).expect("scored");
    assert!((fast - 1.0).abs() < f64::EPSILON);
}
