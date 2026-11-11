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
    let tally: SharedTally = std::sync::Arc::default();
    let text = render_from(&store, &tally, 0).expect("renders");
    assert!(
        text.contains("athena_tasks_total{state=\"CREATED\"} 1"),
        "{text}"
    );
    assert!(
        text.contains("athena_tasks_total{state=\"FAILED\"} 1"),
        "{text}"
    );
}
#[test]
fn the_event_walk_folds_the_recorded_families() {
    // X-40 lane 1: the store snapshot observes the families its rows answer. A store
    // seeded with one failed agent, one retry, one Skill load and one blocked plan
    // step renders those counters beside the Task counts (never-invent: nothing is
    // observed that no row answers).
    let mut store = athena_core::AthenaStore::in_memory().expect("store");
    let task = athena_types::Task {
        id: athena_types::TaskId::new(),
        parent_id: None,
        title: "probe".into(),
        instruction: "probe".into(),
        workspace: "/work/x40".into(),
        goal: None,
        constraints: vec![],
        completion_criteria: vec![],
        state: athena_types::TaskState::Failed,
        priority: athena_types::Priority::Normal,
        plan_id: None,
        risk_level: athena_types::RiskLevel::Medium,
        attempt_count: 1,
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
    // The walk lane's recorded rows: one AgentFailed (error_class), one TaskRetrying
    // (error_class), one SkillLoaded (skill_id) and one SchedulerBlocked — the walk
    // folds exactly what these rows answer, nothing invented.
    let mut seed = |event_type: &str, payload: serde_json::Value| -> () {
        let event = athena_core::NewEvent {
            event_id: athena_types::EventId::new(),
            schema_version: 1,
            event_type: event_type.to_string(),
            occurred_at: "2026-11-12T00:00:01.000Z".to_string(),
            task_id: None,
            plan_step_id: None,
            agent_id: None,
            instance_id: None,
            skill_id: None,
            correlation_id: format!("cor-x40-{event_type}"),
            causation_id: None,
            actor: "system".into(),
            payload_json: payload.to_string(),
        };
        athena_core::EventRepository::append(&mut store, &event).expect("event row");
    };
    seed(
        "AgentFailed",
        serde_json::json!({ "error_class": "transient", "attempt": 1 }),
    );
    seed(
        "TaskRetrying",
        serde_json::json!({ "error_class": "transient", "attempt": 1 }),
    );
    seed(
        "SkillLoaded",
        serde_json::json!({ "skill_id": "pcb-orchestrator", "version": "1.0.0", "checksum": "sha256:aa" }),
    );
    seed(
        "SchedulerBlocked",
        serde_json::json!({ "blocking_limit": "max_concurrency" }),
    );
    // The walk folds exactly these four families; a family no row reached renders its
    // single zero row (the task row above also feeds TasksTotal/TasksRunning).
    let tally: crate::metrics::SharedTally = std::sync::Arc::default();
    let render = crate::metrics::render_from(&store, &tally, 1_794_441_600_000).expect("render");
    assert!(
        render.contains("athena_agent_failures_total{error_class=\"transient\"} 1"),
        "{render}"
    );
    assert!(
        render.contains("athena_retries_total{error_class=\"transient\"} 1"),
        "{render}"
    );
    assert!(
        render.contains("athena_skill_loads_total{skill_id=\"pcb-orchestrator\"} 1"),
        "{render}"
    );
    assert!(
        render.contains("athena_scheduler_blocked_total 1"),
        "{render}"
    );
    // The zero floor is per family, not per label value (D-44): the failures family holds an
    // observed `transient` row, so no other error_class row exists — and none is invented.
    assert!(
        !render.contains("athena_agent_failures_total{error_class=\"other\"}"),
        "{render}"
    );
    assert!(
        render.contains("athena_replans_total 0"),
        "a family no row reached renders its own zero row: {render}"
    );
    assert!(
        render.contains("athena_tasks_total{state=\"FAILED\"} 1"),
        "{render}"
    );
    assert!(render.contains("athena_events_written_total 4"), "{render}");
    assert!(
        render.contains("athena_core_agent_last_event_age_seconds "),
        "{render}"
    );
}

#[test]
fn the_dwell_gauge_counts_the_unaccepted_addressed_messages() {
    // X-40's last family: `athena_message_queue_depth` reads the message rows. Three rows are
    // seeded — one addressed and never delivered (dwelling), one addressed and accepted (not
    // dwelling), one unaddressed (ATH-MSG-010: no derivable delivery key, so the store
    // answers no fact about it and it is excluded rather than assumed).
    use athena_core::MessageRepository;
    let mut store = athena_core::AthenaStore::in_memory().expect("store");
    let to_instance = athena_types::InstanceId::new();
    let mut row = |addressed: bool| -> athena_types::MessageId {
        let id = athena_types::MessageId::new();
        let message = athena_core::MessageRow {
            message_id: id.clone(),
            message_type: athena_types::MessageType::Report,
            from_agent: athena_types::AgentId::new("sender"),
            to_agent: athena_types::AgentId::new("receiver"),
            to_instance_id: addressed.then(|| to_instance.clone()),
            from_instance_id: athena_types::InstanceId::new(),
            task_id: None,
            thread_id: "thread-x40".into(),
            reply_to: None,
            priority: athena_types::Priority::Normal,
            body: "probe".into(),
            payload_json: None,
            deadline: None,
            created_at: "2026-11-12T00:00:00.000Z".into(),
        };
        MessageRepository::insert(&mut store, &message).expect("message row");
        id
    };
    let dwelling = row(true);
    let accepted = row(true);
    let _unaddressed = row(false);
    assert!(
        MessageRepository::mark_delivered(&mut store, &to_instance, &accepted).expect("deliver"),
        "the first delivery of a message is recorded"
    );
    MessageRepository::mark_accepted(&mut store, &to_instance, &accepted).expect("accept");
    // A delivered-but-unaccepted row still dwells: acceptance is the receiving side's act.
    assert!(
        MessageRepository::mark_delivered(&mut store, &to_instance, &dwelling).expect("deliver"),
        "the dwelling row is delivered, not accepted"
    );
    assert_eq!(
        MessageRepository::count_dwelling(&store).expect("dwell"),
        1,
        "one addressed row is unaccepted; the accepted and the unaddressed rows are not counted"
    );
    let tally: crate::metrics::SharedTally = std::sync::Arc::default();
    let render = crate::metrics::render_from(&store, &tally, 1_794_441_600_000).expect("render");
    assert!(render.contains("athena_message_queue_depth 1"), "{render}");
}
