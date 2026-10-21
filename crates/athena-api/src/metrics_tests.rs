use super::*;
use athena_core::observability::MetricFamily;

#[test]
fn the_registry_renders_the_type_line_for_every_family() {
    // ATH-OBS-007 / ATH-API-020: all families ride the wire, at zero when unobserved, so
    // a scraper's chart never toggles on series presence.
    let text = Metrics::new().render();
    for family in all_families() {
        assert!(
            text.contains(&format!("# TYPE {} {}", family.name(), family.kind())),
            "{} must carry its type line: {text}",
            family.name()
        );
        assert!(
            text.contains(&format!("\n{} 0\n", family.name())),
            "{} at least zeroed",
            family.name()
        );
    }
}

#[test]
fn the_observed_rows_carry_their_labels() {
    // ATH-OBS-006's label columns: the rendered row pairs label=value pairs.
    let mut registry = Metrics::new();
    registry.observe(
        MetricFamily::TasksTotal,
        [("state".to_string(), "RUNNING".to_string())]
            .into_iter()
            .collect(),
        7.0,
    );
    let text = registry.render();
    assert!(
        text.contains("athena_tasks_total{state=\"RUNNING\"} 7"),
        "{text}"
    );
}

#[test]
fn the_api_render_includes_a_task_count_from_the_store() {
    // R-OBS-01: the *store* is the observation source, not vibes; a store with two Tasks
    // in different states renders distinct `athena_tasks_total{state=…}` rows.
    let mut store = athena_core::AthenaStore::in_memory().expect("store");
    for (id_suffix, state) in [
        ("t1", athena_types::TaskState::Created),
        ("t2", athena_types::TaskState::Failed),
    ] {
        let task = athena_types::Task {
            id: athena_types::TaskId::new(),
            parent_id: None,
            title: id_suffix.to_string(),
            instruction: id_suffix.to_string(),
            workspace: format!("/work/{id_suffix}"),
            goal: None,
            constraints: vec![],
            completion_criteria: vec![],
            state,
            priority: athena_types::Priority::Normal,
            plan_id: None,
            risk_level: athena_types::RiskLevel::Medium,
            attempt_count: 0,
            max_attempts: 3,
            iteration_count: 0,
            max_iterations: 50,
            deadline: None,
            created_by: "test".into(),
            created_at: "2026-10-14T00:00:00.000Z".into(),
            updated_at: "2026-10-14T00:00:00.000Z".into(),
            finished_at: None,
        };
        athena_core::TaskRepository::insert(&mut store, &task).expect("seed");
    }
    let text = render_from(&store).expect("renders");
    assert!(
        text.contains("athena_tasks_total{state=\"CREATED\"} 1"),
        "{text}"
    );
    assert!(
        text.contains("athena_tasks_total{state=\"FAILED\"} 1"),
        "{text}"
    );
}
