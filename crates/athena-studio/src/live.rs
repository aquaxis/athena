//! The live-update layer (P7-06; `ATH-STUDIO-004` / R-STU-03 — Task progress updates in
//! real time from the Event Stream; approvals ride `P7-05`'s actionable plans):
//!
//! The board holds the Task rows and a bounded tail of recent Events; each streamed
//! chunk is applied as a *reduction* — a Task-state-bearing event type moves the named
//! Task's row, every event appends to the tail, malformed chunks never invent a row
//! (`parse_stream_chunk`'s discipline), and no payload content ever propagates: the
//! `EventRow` face carries type / task / stamp only, so neither chain-of-thought nor
//! secrets can reach the screen (`ATH-EVENT-007`, `ATH-MEM-004`, `ATH-STUDIO-006`).
//!
//! The browser transport (an `EventSource` over `/api/v1/events/stream`) feeds its
//! `data:` accumulation into [`LiveBoard::apply_chunk`]; this layer is the deterministic
//! contract both the wasm face and the unit tests resolve against.

use crate::client::{EventRow, TaskRow, parse_stream_chunk};

/// How many tail Events the board keeps (the §25 Event View rides the API's own
/// replay face for older rows; the tail is the live surface).
const TAIL_CAP: usize = 100;

/// The Task states a streamed Event *type* observes. The face is the map from the
/// §23.2 Task-family event names to the §17.1 state the emitted Event denotes —
/// an event whose type carries no Task-state fact leaves the row untouched
/// (a non-state event is never read as one).
#[must_use]
pub fn task_state_of_event(event_type: &str) -> Option<&'static str> {
    match event_type {
        "TaskCreated" => Some("CREATED"),
        "TaskQueued" => Some("QUEUED"),
        "TaskStarted" => Some("RUNNING"),
        "ApprovalRequested" => Some("WAITING_APPROVAL"),
        "ClarificationRequested" => Some("WAITING_INPUT"),
        "TaskRetrying" => Some("RETRYING"),
        "TaskReplanning" => Some("REPLANNING"),
        "TaskResultReceived" => Some("EVALUATING"),
        "TaskCancelling" => Some("CANCELLING"),
        "TaskCancelled" => Some("CANCELLED"),
        "TaskCompleted" => Some("SUCCESS"),
        "TaskFailed" => Some("FAILED"),
        _ => None,
    }
}

/// The live board: the Task rows the Dashboard/Task View render, updated from the
/// stream, plus the recent-Events tail.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LiveBoard {
    /// The Task rows, in first-seen order.
    pub tasks: Vec<TaskRow>,
    /// The recent Events (the live surface; capped at
    /// [`TAIL_CAP`], newest last).
    pub tail: Vec<EventRow>,
}

impl LiveBoard {
    /// Seed the board from the Task API's rows (the Dashboard fetch),
    /// then let the stream keep it current.
    #[must_use]
    pub fn from_tasks(tasks: Vec<TaskRow>) -> Self {
        Self {
            tasks,
            tail: Vec::new(),
        }
    }

    /// Apply one streamed chunk (the raw `data:` accumulation) into the board.
    ///
    /// State events move (or create, minimized) the named Task's row; every parsed
    /// event appends to the tail; malformed or payload-less lines contribute nothing.
    /// The board renders afterwards via [`render`].
    pub fn apply_chunk(&mut self, chunk: &str) {
        for event in parse_stream_chunk(chunk) {
            self.apply_event(event)
        }
    }

    /// Apply one parsed Event row into the board.
    pub fn apply_event(&mut self, event: EventRow) {
        if let (Some(task_id), Some(state)) = (
            event.task_id.clone(),
            task_state_of_event(&event.event_type),
        ) {
            self.update_task_row(task_id, state);
        }
        self.tail.push(event);
        let excess = self.tail.len().saturating_sub(TAIL_CAP);
        self.tail.drain(0..excess);
    }

    fn update_task_row(&mut self, task_id: String, state: &str) {
        if let Some(row) = self.tasks.iter_mut().find(|row| row.id == task_id) {
            row.state = state.to_string();
        } else {
            // A Task first seen through the stream: only the *observed* facts —
            // the id and the state. Title/instruction are never invented here; the
            // next Dashboard fetch fills them (`ATH-API-001`, the API's face only).
            self.tasks.push(TaskRow {
                id: task_id,
                state: state.to_string(),
                title: String::new(),
                instruction: String::new(),
            });
        }
    }
}

/// Render the board as the `#live` section: one row per Task (`data-task-id`,
/// `data-state`) and the tail's name/stamp rows. All faces are inert text —
/// `<`/`>`/`&` escape before they touch the page (`P7-03`'s render contract),
/// and no Event payload content exists anywhere in the shape.
#[must_use]
pub fn render(board: &LiveBoard) -> String {
    let mut out = String::from("<section id=\"live\">");
    for task in &board.tasks {
        out.push_str(&format!(
            "<div class=\"live-task\" data-task-id=\"{}\" data-state=\"{}\"><span class=\"id\">{}</span> <span class=\"state\">{}</span></div>",
            escape(&task.id),
            escape(&task.state),
            escape(&task.id),
            escape(&task.state),
        ));
    }
    out.push_str("<ol class=\"live-tail\">");
    for event in &board.tail {
        let task = event.task_id.as_deref().unwrap_or("");
        out.push_str(&format!(
            "<li data-type=\"{}\" data-task-id=\"{}\"><span class=\"type\">{}</span> <span class=\"at\">{}</span></li>",
            escape(&event.event_type),
            escape(task),
            escape(&event.event_type),
            escape(&event.occurred_at),
        ));
    }
    out.push_str("</ol></section>");
    out
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod live_tests;
