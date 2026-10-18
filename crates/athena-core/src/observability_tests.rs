//! Observability-module behaviour (ATH-OBS-001…013).

use super::*;
use athena_types::InstanceId;
use std::sync::Mutex as SyncMutex;

// --- logs -------------------------------------------------------------------------------

#[derive(Default)]
struct Captured(SyncMutex<Vec<LogLine>>);

impl LogSink for Captured {
    fn emit(&self, line: &LogLine) {
        self.0.lock().expect("capture").push(line.clone());
    }
}

#[test]
fn the_log_sink_receives_every_line_with_the_correlation_id() {
    // ATH-OBS-009 / R-OBS-02: every line carries the correlation id; the emitter is the
    // single writer.
    let sink = Captured::default();
    let logger = Logger::new(&sink, "cor-1".into());
    let error = logger.log(
        Log::Error,
        "athena.core.session",
        "instance #0 failed",
        serde_json::json!({ "attempt": 2 }),
        "2026-10-14T00:00:00.000Z",
    );
    assert_eq!(error.level, Log::Error);
    assert_eq!(sink.0.lock().expect("capture").len(), 1);
    let logged = &sink.0.lock().expect("capture")[0];
    assert_eq!(logged.correlation_id, "cor-1");
    assert_eq!(logged.module.as_str(), "athena.core.session");
}

// --- metrics ----------------------------------------------------------------------------

#[test]
fn the_metric_catalog_carries_exactly_the_30_3_families() {
    // The §30.3 table's rows, one enum member each (the drift guard: a family added in
    // code without the table, or lost, fails here).
    let families = all_families();
    assert_eq!(families.len(), 23);
    for family in families {
        assert!(!family.name().is_empty());
        assert!(matches!(family.kind(), "counter" | "gauge" | "histogram"));
    }
}

#[test]
fn the_row_labels_match_the_specification() {
    // Spec §30.3: the label columns are exactly what the table says.
    assert_eq!(MetricFamily::TasksTotal.labels(), &["state"]);
    assert_eq!(MetricFamily::AgentInstancesRunning.labels(), &["runtime"]);
    assert_eq!(MetricFamily::RetriesTotal.labels(), &["error_class"]);
    assert_eq!(
        MetricFamily::ApiRequestsTotal.labels(),
        &["route", "status"]
    );
    assert_eq!(MetricFamily::InstancesWaiting.labels(), &["state"]);
    assert_eq!(
        MetricFamily::PrivilegedRequestsDeniedTotal.labels(),
        &["request_kind"]
    );
    assert_eq!(MetricFamily::TaskDurationSeconds.labels(), &[] as &[&str]);
}

#[test]
fn the_kind_column_splits_counters_gauges_and_histograms() {
    // 13 counters + 6 gauges + 4 histograms = 23 (spec §30.3's type column).
    let families = all_families();
    let counters = families.iter().filter(|f| f.kind() == "counter").count();
    let gauges = families.iter().filter(|f| f.kind() == "gauge").count();
    let histograms = families.iter().filter(|f| f.kind() == "histogram").count();
    assert_eq!((counters, gauges, histograms), (13, 6, 4));
}

// --- stall detectors --------------------------------------------------------------------

#[test]
fn the_silence_detector_fires_at_the_configured_line() {
    // ATH-OBS-012: `core.iteration_timeout × scheduler.stall_detection_factor` — the
    // threshold is the caller's product; the detector just compares.
    let threshold = 30 * 60 * 1000 * 2;
    assert_eq!(
        silence(threshold - 1, threshold),
        None,
        "silent for less: no signal"
    );
    let signal = silence(threshold, threshold).expect("at the line it fires");
    assert!(
        matches!(signal, Stall::Silent { silent_ms, threshold_ms } if silent_ms == threshold && threshold_ms == threshold)
    );
    assert_eq!(signal.kind(), "state_change_silence");
}

#[test]
fn the_wait_graph_detects_a_cycle_and_names_its_members() {
    // ATH-OBS-013 / ATH-EVENT-018: a circular wait is a stall, named from the graph.
    let a = InstanceId::new();
    let b = InstanceId::new();
    let c = InstanceId::new();
    let looped = vec![a.clone(), b.clone(), c.clone()];
    let edges = vec![
        (a.clone(), b.clone()),
        (b.clone(), c.clone()),
        (c.clone(), a.clone()),
    ];
    let signal = wait_cycle(&edges).expect("a cycle exists");
    let kind = signal.kind();
    let cycle = match signal {
        Stall::WaitCycle { cycle } => cycle.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(cycle.len(), 3);
    for member in looped {
        assert!(
            cycle.iter().any(|id| id == &member),
            "the cycle names its members: {cycle:?} vs {member:?}"
        );
    }
    assert_eq!(kind, "wait_graph_cycle");
}

#[test]
fn a_acyclic_wait_graph_produces_no_signal() {
    // A chain a → b → c is a legal Wait structure (a superior waits on a subordinate).
    let a = InstanceId::new();
    let b = InstanceId::new();
    let c = InstanceId::new();
    let edges = vec![(a.clone(), b.clone()), (b.clone(), c.clone())];
    assert_eq!(wait_cycle(&edges), None);
    // A disconnected pairing without a loop too.
    let d = InstanceId::new();
    assert_eq!(wait_cycle(&[(a, d)]), None);
}

#[test]
fn the_heartbeat_age_is_whole_seconds() {
    // ATH-OBS-011's absence side.
    assert_eq!(heartbeat_age_since(1_000, 21_000), 20);
    assert_eq!(
        heartbeat_age_since(21_000, 1_000),
        0,
        "future stamps do not back-date"
    );
}
