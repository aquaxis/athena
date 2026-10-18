//! P7-06's rows: live reductions over the Event Stream.

use crate::client::{EventRow, TaskRow};
use crate::live::{LiveBoard, render};

fn task_row(id: &str, state: &str) -> TaskRow {
    TaskRow {
        id: id.to_string(),
        state: state.to_string(),
        title: format!("title-{id}"),
        instruction: format!("instruction-{id}"),
    }
}

fn event(event_type: &str, task_id: Option<&str>) -> EventRow {
    EventRow {
        event_type: event_type.to_string(),
        task_id: task_id.map(str::to_string),
        occurred_at: "2026-10-18T00:00:00Z".to_string(),
    }
}

#[test]
fn a_state_event_moves_the_named_tasks_row_in_real_time() {
    let mut board = LiveBoard::from_tasks(vec![task_row("tsk-1", "CREATED")]);
    board.apply_chunk("data: {\"event_type\":\"TaskStarted\",\"task_id\":\"tsk-1\",\"occurred_at\":\"2026-10-18T00:00:01Z\"}\n\n");
    assert_eq!(board.tasks[0].state, "RUNNING");
    // Approval/clarification silence moves the state face too (the §17.1 derived
    // precedence the board mirrors).
    board.apply_chunk(
        "data: {\"event_type\":\"ApprovalRequested\",\"task_id\":\"tsk-1\",\"occurred_at\":\"2026-10-18T00:00:02Z\"}\n\n",
    );
    assert_eq!(board.tasks[0].state, "WAITING_APPROVAL");
    // A terminal event moves the row terminal.
    board.apply_chunk("data: {\"event_type\":\"TaskCompleted\",\"task_id\":\"tsk-1\",\"occurred_at\":\"2026-10-18T00:00:03Z\"}\n\n");
    assert_eq!(board.tasks[0].state, "SUCCESS");
    // The title/instruction were never touched by the stream.
    assert_eq!(board.tasks[0].title, "title-tsk-1");
}

#[test]
fn a_task_first_seen_through_the_stream_is_created_without_invented_prose() {
    let mut board = LiveBoard::from_tasks(vec![]);
    board.apply_chunk("data: {\"event_type\":\"TaskQueued\",\"task_id\":\"tsk-9\",\"occurred_at\":\"2026-10-18T00:00:00Z\"}\n\n");
    assert_eq!(board.tasks.len(), 1);
    assert_eq!(board.tasks[0].state, "QUEUED");
    assert_eq!(board.tasks[0].title, String::new());
    assert_eq!(board.tasks[0].instruction, String::new());
}

#[test]
fn non_state_events_tail_only_and_malformed_lines_invent_nothing() {
    let mut board = LiveBoard::from_tasks(vec![task_row("tsk-1", "RUNNING")]);
    board.apply_chunk(
        "data: {\"event_type\":\"AgentMessageSent\",\"task_id\":\"tsk-1\",\"occurred_at\":\"2026-10-18T00:00:01Z\"}\n\n\
         data: not-json-at-all\n\n\
         event: keepalive\n\n",
    );
    // No invented state change…
    assert_eq!(board.tasks[0].state, "RUNNING");
    // …and exactly one tail row (the malformed lines contributed nothing).
    assert_eq!(
        board.tail,
        vec![EventRow {
            event_type: "AgentMessageSent".to_string(),
            task_id: Some("tsk-1".to_string()),
            occurred_at: "2026-10-18T00:00:01Z".to_string(),
        }]
    );
}

#[test]
fn the_tail_is_capped_at_the_most_recent_events() {
    let mut board = LiveBoard::from_tasks(vec![]);
    for stamp in 0..130 {
        board.apply_event(event("AgentMessageSent", Some("tsk-1")));
        let last = board.tail.last_mut().expect("just pushed");
        last.occurred_at = format!("2026-10-18T00:{stamp:04}");
    }
    assert_eq!(board.tail.len(), 100);
    assert_eq!(board.tail[0].occurred_at, "2026-10-18T00:0030");
    assert_eq!(board.tail[99].occurred_at, "2026-10-18T00:0129");
}

#[test]
fn no_payload_content_ever_reaches_the_screen() {
    // A streamed payload carrying an agent's full input (chain-of-thought face) and a
    // credential-shaped value: the EventRow face strips both by construction — the
    // rendered board must not contain either substring (`ATH-EVENT-007`,
    // `ATH-MEM-004`, `ATH-STUDIO-006`).
    let mut board = LiveBoard::from_tasks(vec![]);
    board.apply_chunk(
        "data: {\"event_type\":\"TaskStarted\",\"task_id\":\"tsk-1\",\"occurred_at\":\"2026-10-18T00:00:00Z\",\
         \"instruction\":\"do the secret thing sk-ant-api-key-XYZZY\",\"reasoning\":\"hidden chain of thought\"}\n\n",
    );
    let rendered = render(&board);
    assert!(!rendered.contains("sk-ant-api-key-XYZZY"));
    assert!(!rendered.contains("hidden chain of thought"));
    assert!(!rendered.contains("do the secret thing"));
    // The observed face is present, escaped, and shaped.
    assert!(
        rendered
            .contains("<div class=\"live-task\" data-task-id=\"tsk-1\" data-state=\"RUNNING\">")
    );
    assert!(rendered.contains("<li data-type=\"TaskStarted\" data-task-id=\"tsk-1\">"));
}

#[test]
fn hostile_event_text_renders_inert() {
    let mut board = LiveBoard::from_tasks(vec![]);
    board.apply_chunk(
        "data: {\"event_type\":\"<script>alert(1)</script>\",\"task_id\":\"tsk-1\",\"occurred_at\":\"2026-10-18T00:00:00Z\"}\n\n",
    );
    let rendered = render(&board);
    assert!(rendered.contains("&lt;script&gt;"), "{rendered}");
    assert!(!rendered.contains("<script>"));
}
