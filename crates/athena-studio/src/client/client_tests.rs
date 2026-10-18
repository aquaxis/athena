//! Tests for the Core-API/SSE client layer (P7-02).

use super::*;

#[test]
fn the_views_publish_request_plans_over_the_published_endpoints_only() {
    // The Dashboard rides the tasks list.
    let dashboard = requests_for_view(&View::Dashboard);
    assert_eq!(dashboard.len(), 1);
    assert_eq!(dashboard[0].path, "/api/v1/tasks");

    // The Task view rides the task + its plan.
    let task = requests_for_view(&View::Task("task-01JA".into()));
    assert_eq!(task.len(), 2);
    assert_eq!(task[0].path, "/api/v1/tasks/task-01JA");
    assert_eq!(task[1].path, "/api/v1/tasks/task-01JA/plan");

    // The Approvals ride the approvals list; the stream rides the SSE face.
    let approvals = requests_for_view(&View::Approvals);
    assert_eq!(approvals[0].path, "/api/v1/approvals");
    let stream = requests_for_view(&View::EventStream);
    assert_eq!(stream[0].path, "/api/v1/events/stream");
}

#[test]
fn the_stream_chunks_decode_to_event_rows_and_skip_garbage() {
    let face = r#"data: {"event_type":"TaskStarted","task_id":"task-01JA","occurred_at":"2026-10-18T00:00:03Z"}
data: {"event_type":"AgentStarted"}
data: not json at all
"#;
    let rows = parse_stream_chunk(face);
    assert_eq!(rows.len(), 2, "the malformed line is skipped, not invented");
    assert_eq!(rows[0].event_type, "TaskStarted");
    assert_eq!(rows[0].task_id.as_deref(), Some("task-01JA"));
    assert_eq!(rows[1].event_type, "AgentStarted");
    assert!(rows[1].task_id.is_none());
}

#[test]
fn the_api_path_guard_refuses_the_traversal() {
    assert!(safe_api_path("/api/v1/tasks").is_some());
    assert!(safe_api_path("/../../etc/passwd").is_none());
}
