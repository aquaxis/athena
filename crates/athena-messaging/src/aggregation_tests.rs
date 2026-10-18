//! P2-20: report aggregation — Lead aggregates and evaluates Worker Results before
//! reporting (ATH-HIER-015), reports aggregate up the whole chain (ATH-HIER-016), and a
//! verbatim relay is never the pass condition (ATH-HIER-015).

use super::{AggregateReport, AggregationBreach, ReportDigest, aggregation_rank_of};
use athena_types::Verdict;

// --- ATH-HIER-015: aggregation over verbatim relay ---------------------------------------

#[test]
fn an_aggregate_of_verified_inputs_is_accepted() {
    // The Lead's own aggregated product: distinct from its inputs, and every input was
    // evaluated. This is the shape ATH-HIER-015 requires before an upward report.
    let inputs = vec![
        ReportDigest {
            agent_id: "rtl-worker-a".into(),
            summary: "tests pass, 12 added".into(),
            verdict: Verdict::Accept,
        },
        ReportDigest {
            agent_id: "rtl-worker-b".into(),
            summary: "lint clean, 1 fix".into(),
            verdict: Verdict::Accept,
        },
    ];
    let report = AggregateReport::collect(&inputs, "subtask tests: 12 new + 1 fix covered")
        .into_accepted("the aggregation is the Lead's evaluated product; inputs were checked")
        .expect("evaluated");
    assert!(report.is_evaluated());
    assert!(report.relayed_verbatim().is_none(), "not a relay");
    let breach = report.breach();
    assert_eq!(breach, None);
}

#[test]
fn a_verbatim_relay_of_an_accept_verdict_is_breaching() {
    // ATH-HIER-015's MUST NOT: relaying a Worker Result as `accept` skips aggregation
    // altogether. The observable is the breach; the consequence is Core Agent's decision
    // (ATH-CORE-022), not a Core Host takeover (ATH-CORE-002).
    let inputs = vec![ReportDigest {
        agent_id: "rtl-worker-a".into(),
        summary: "it worked".into(),
        verdict: Verdict::Accept,
    }];
    let report = AggregateReport::collect(&inputs, "it worked").into_accepted("looks fine to me");
    let breach = report.breach().expect("the relay is observable");
    assert!(matches!(breach, AggregationBreach::VerbatimRelay { .. }));
    assert!(
        breach.to_string().contains("rtl-worker-a"),
        "the breach names the relayed input: {breach}"
    );
    assert!(report.relayed_verbatim().is_some());
    assert!(!report.is_evaluated());
}

#[test]
fn the_aggregation_contract_is_level_blind() {
    // ATH-HIER-015's shape is the same at every level — a Lead over Workers, a Director
    // over Leads, the Producer over domains. No level check rides a report here: level
    // legality is the Hierarchy Manager's own check (ATH-HIER-004), and this model only
    // ranks the chain direction (ATH-HIER-016). The same aggregated shape, checked at two
    // ranks, behaves identically.
    let build = |agent: &str, summary: &str| ReportDigest {
        agent_id: agent.into(),
        summary: summary.into(),
        verdict: Verdict::Accept,
    };
    let lead_report = AggregateReport::collect(
        &[build("rtl-worker", "done")],
        "aggregated: worker's steps verified",
    )
    .into_accepted("evaluated at lead rank")
    .expect("evaluated");
    let director_report = AggregateReport::collect(
        &[build("rtl-lead", "domain verified")],
        "aggregated: both subtasks verified",
    )
    .into_accepted("evaluated at director rank")
    .expect("evaluated");
    for (rank, report) in [
        (aggregation_rank_of("lead"), lead_report),
        (aggregation_rank_of("director"), director_report),
    ] {
        assert!(
            report.is_evaluated(),
            "rank {rank}: the same contract holds"
        );
        assert_eq!(report.breach(), None, "rank {rank}: no breach");
    }
}

// --- ATH-HIER-016: aggregation up the chain ----------------------------------------------

#[test]
fn the_report_ranks_say_aggregation_runs_upward() {
    // Worker < Lead < Director < Producer: each level must have evaluated before its own
    // report answers upward — rank(Worker) is lowest, rank(Producer) highest, verified in
    // the spec §8.2.1 chain order.
    let values: &[(&str, u8)] = &[
        ("worker", aggregation_rank_of("worker")),
        ("lead", aggregation_rank_of("lead")),
        ("director", aggregation_rank_of("director")),
        ("producer", aggregation_rank_of("producer")),
    ];
    for pair in values.windows(2) {
        assert!(
            pair[0].1 < pair[1].1,
            "{} must rank below {}: {:?}",
            pair[0].0,
            pair[1].0,
            values
        );
    }
}

#[test]
fn a_director_report_aggregates_across_its_leads() {
    // The Director aggregates *Lead* reports (ATH-HIER-016) — the same rule one level up.
    let leads = vec![
        ReportDigest {
            agent_id: "rtl-lead-a".into(),
            summary: "both workers verified".into(),
            verdict: Verdict::Accept,
        },
        ReportDigest {
            agent_id: "rtl-lead-b".into(),
            summary: "workers verified, one retried".into(),
            verdict: Verdict::Accept,
        },
    ];
    let outcome = AggregateReport::collect(&leads, "domain summary across both leads")
        .into_accepted("evaluated across the leads I own");
    assert!(outcome.is_evaluated());
    let breach = outcome.breach();
    assert_eq!(breach, None);
    assert_eq!(outcome.report.inputs.len(), 2);
}

#[test]
fn an_aggregate_cannot_pass_with_an_unresolved_input() {
    // A Lead aggregating a Worker whose verdict is Retry/Replan/Fail/Escalate cannot carry
    // an `accept` outcome before resolving it: the unresolved input breaches
    // `UnresolvedInputs` — which is what ATH-HIER-015's "aggregated and evaluated" means.
    let inputs = vec![
        ReportDigest {
            agent_id: "rtl-worker-a".into(),
            summary: "done".into(),
            verdict: Verdict::Accept,
        },
        ReportDigest {
            agent_id: "rtl-worker-b".into(),
            summary: "compile error".into(),
            verdict: Verdict::Retry,
        },
    ];
    let report = AggregateReport::collect(&inputs, "aggregation text")
        .into_accepted("accepting anyway is the case this refuses");
    let breach = report.breach().expect("the unresolved input is observable");
    assert!(matches!(breach, AggregationBreach::UnresolvedInputs { .. }));
    assert!(
        breach.to_string().contains("rtl-worker-b"),
        "the breach names the unresolved input: {breach}"
    );
}

#[test]
fn an_aggregate_of_distinct_output_satisfies_the_contract() {
    // Distinct text from every input, all inputs resolved: no breach, the Lead may report.
    let inputs = vec![
        ReportDigest {
            agent_id: "rtl-worker-a".into(),
            summary: "fsm passes".into(),
            verdict: Verdict::Accept,
        },
        ReportDigest {
            agent_id: "rtl-worker-b".into(),
            summary: "timing met".into(),
            verdict: Verdict::Accept,
        },
    ];
    let report = AggregateReport::collect(&inputs, "fsm passes and timing is met")
        .into_accepted("both verified and folded into one SubTask verdict");
    assert_eq!(report.breach(), None);
    assert!(report.is_evaluated());
}
#[test]
fn a_stitched_listing_of_outputs_breaches_without_added_words() {
    // The second structural floor: outputs glued with separators only — "A: x; B: y" —
    // is a pass-through with punctuation, not a summary. Added *words* ("both pass") are
    // what distinguishes a real fold, and the floor leaves them below the observation.
    let inputs = vec![
        ReportDigest {
            agent_id: "rtl-worker-a".into(),
            summary: "fsm passes".into(),
            verdict: Verdict::Accept,
        },
        ReportDigest {
            agent_id: "rtl-worker-b".into(),
            summary: "timing met".into(),
            verdict: Verdict::Accept,
        },
    ];
    let stitched = "fsm passes; timing met";
    let report = AggregateReport::collect(&inputs, stitched)
        .into_accepted("a bullet-list relay, not an aggregation");
    let breach = report.breach().expect("the stitched listing is observable");
    assert!(
        matches!(breach, AggregationBreach::VerbatimRelay { .. }),
        "{breach}"
    );

    // The same outputs folded into the reporter's own sentence stay below the floor:
    // the words in the middle are the aggregation. (A *labelled* list — "A: x; B: y" —
    // also stays below it: a label cannot be told from prose mechanically, and naming
    // honest labelled summaries would be the false positive the rule refuses. Adding
    // judgement on prose is Core's to never do.)
    let folded = "\"fsm passes\" and \"timing met\" — both verified by my own run";
    let report = AggregateReport::collect(&inputs, folded)
        .into_accepted("a genuine fold quotes parts inside new prose");
    assert_eq!(report.breach(), None, "quoting parts is not a relay");
}

// --- the coverage facts (Broker-side, pure) ----------------------------------------------

#[test]
fn the_coverage_obligation_lists_unique_sorted_subordinates() {
    // `obligation_for`: deduplication + determinism, so the same composition reads the
    // same owed set everywhere.
    use super::obligation_for;
    let owed = obligation_for(&["b".into(), "a".into(), "b".into()]);
    assert_eq!(owed, vec!["a".to_string(), "b".to_string()]);
    assert!(obligation_for(&[]).is_empty(), "a leaf owes nothing");
}

#[test]
fn a_report_before_its_subordinate_report_is_uncovered() {
    // ATH-HIER-015: aggregate BEFORE reporting upward. A subordinate report that arrived
    // after the upward report cannot have been folded into it.
    use super::{ReportFacts, SubordinateReport, uncovered};
    let facts = ReportFacts {
        upward_reported_at: "2026-10-14T00:10:00.000Z".into(),
        subordinate_reports: vec![
            SubordinateReport {
                agent_id: "rtl-worker-a".into(),
                reported_at: "2026-10-14T00:05:00.000Z".into(),
            },
            SubordinateReport {
                agent_id: "rtl-worker-b".into(),
                reported_at: "2026-10-14T00:20:00.000Z".into(),
            },
        ],
    };
    let owed = ["rtl-worker-a".to_string(), "rtl-worker-b".to_string()];
    let unmet = uncovered(&facts, &owed);
    assert_eq!(
        unmet,
        vec!["rtl-worker-b".to_string()],
        "a came before, b came after"
    );
}

#[test]
fn a_missing_subordinate_report_is_uncovered_entirely() {
    use super::{ReportFacts, uncovered};
    let facts = ReportFacts {
        upward_reported_at: "2026-10-14T00:10:00.000Z".into(),
        subordinate_reports: vec![],
    };
    let owed = ["rtl-worker-a".to_string()];
    assert_eq!(
        uncovered(&facts, &owed),
        owed,
        "nothing received: everything unmet"
    );
}

#[test]
fn every_subordinate_report_before_the_upward_report_is_covered() {
    // The compliant case: both inputs arrived before the aggregation went out.
    use super::{ReportFacts, SubordinateReport, uncovered};
    let facts = ReportFacts {
        upward_reported_at: "2026-10-14T00:30:00.000Z".into(),
        subordinate_reports: vec![
            SubordinateReport {
                agent_id: "rtl-worker-a".into(),
                reported_at: "2026-10-14T00:05:00.000Z".into(),
            },
            SubordinateReport {
                agent_id: "rtl-worker-b".into(),
                reported_at: "2026-10-14T00:20:00.000Z".into(),
            },
        ],
    };
    let owed = ["rtl-worker-a".to_string(), "rtl-worker-b".to_string()];
    assert!(uncovered(&facts, &owed).is_empty(), "both folded in time");
}
