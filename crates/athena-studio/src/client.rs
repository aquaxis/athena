//! The Core API + SSE client layer (P7-02; `ATH-API-001`, `ATH-RT-007` — the UI
//! reaches the Core through the API surface only, never a Core internal):
//!
//! The requests the Studio's views ride, as typed request/response shapes — each one
//! names the endpoint the API publishes (§22's routes), and the responses are the
//! deserialized JSON faces.
//!
//! The transport itself is a browser `fetch` call in the wasm build; this layer is
//! the *client contract*: `Request {method, path, body}` → the typed response — what
//! both the wasm face and the unit tests resolve against (deterministic; the socket
//! is the caller's).

/// The request the Studio sends (the Core API's route face).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The HTTP method.
    pub method: &'static str,
    /// The endpoint path (relative to the API root).
    pub path: String,
    /// The JSON body, when the request carries one.
    pub body_json: Option<String>,
}

/// The response's typed faces the Studio's views deserialize.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    /// The Task rows the Dashboard/Task View render.
    Tasks(Vec<TaskRow>),
    /// One Task's Plan (the Plan View: DAG + revisions).
    Plan(PlanRow),
    /// The Event Stream's messages (the SseClient's input).
    Events(Vec<EventRow>),
    /// The Approvals the Approval View acts on.
    Approvals(Vec<ApprovalRow>),
    /// A write outcome (the 201/202 face).
    Accepted { status: u16 },
    /// No Task addressed the request (the UI renders the empty face).
    Nothing,
}

/// One Task's render row (`GET /api/v1/tasks/{id}`'s face).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRow {
    /// The Task's identifier.
    pub id: String,
    /// The state.
    pub state: String,
    /// The title.
    pub title: String,
    /// The instruction.
    pub instruction: String,
}

/// One Plan's render row (`GET /api/v1/tasks/{id}/plan`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRow {
    /// The Plan id.
    pub id: String,
    /// The revision.
    pub revision: u64,
    /// The steps (the DAG's nodes; their `depends_on` are the edges the view draws).
    pub steps: Vec<PlanStepRow>,
}

/// One PlanStep's render row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanStepRow {
    /// The step id.
    pub id: String,
    /// The intent.
    pub intent: String,
    /// The state.
    pub state: String,
    /// The assigned agent.
    pub assigned_agent: Option<String>,
}

/// One Event's render row (the SSE payload's deserialized face).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRow {
    /// The Event's type name.
    pub event_type: String,
    /// The owning Task.
    pub task_id: Option<String>,
    /// The occurrence stamp.
    pub occurred_at: String,
}

/// One Approval's render row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRow {
    /// The approval id.
    pub id: String,
    /// The canonical descriptor (foremost on the screen, `ATH-HITL-010`).
    pub canonical_descriptor: String,
    /// The risk.
    pub risk_level: String,
    /// The state.
    pub state: String,
    /// The untrusted agent prose (marked on the screen, `ATH-HITL-011`).
    pub agent_reason: Option<String>,
}

/// The client layer's request mapping: which request delivers which view's rows.
///
/// # Errors
/// The paths here are static; nothing refuses.
#[must_use]
pub fn requests_for_view(view: &View) -> Vec<Request> {
    match view {
        View::Dashboard => vec![Request {
            method: "GET",
            path: "/api/v1/tasks".into(),
            body_json: None,
        }],
        View::Task(id) | View::Plan(id) => vec![
            Request {
                method: "GET",
                path: format!("/api/v1/tasks/{id}"),
                body_json: None,
            },
            Request {
                method: "GET",
                path: format!("/api/v1/tasks/{id}/plan"),
                body_json: None,
            },
        ],
        View::Approvals => vec![Request {
            method: "GET",
            path: "/api/v1/approvals".into(),
            body_json: None,
        }],
        View::AgentHierarchy(id) => vec![Request {
            method: "GET",
            path: format!("/api/v1/hierarchies/{id}"),
            body_json: None,
        }],
        View::Agent => vec![
            Request {
                method: "GET",
                path: "/api/v1/agents".into(),
                body_json: None,
            },
            Request {
                method: "GET",
                path: "/api/v1/instances".into(),
                body_json: None,
            },
        ],
        View::Message(id) => vec![Request {
            method: "GET",
            path: format!("/api/v1/tasks/{id}/messages"),
            body_json: None,
        }],
        View::ToolExecution => vec![Request {
            method: "GET",
            path: "/api/v1/tool-executions".into(),
            body_json: None,
        }],
        View::Skill => vec![Request {
            method: "GET",
            path: "/api/v1/skills".into(),
            body_json: None,
        }],
        View::Event => vec![Request {
            method: "GET",
            path: "/api/v1/events".into(),
            body_json: None,
        }],
        View::Result(id) => vec![
            Request {
                method: "GET",
                path: format!("/api/v1/tasks/{id}/results"),
                body_json: None,
            },
            Request {
                method: "GET",
                path: "/api/v1/evaluations".into(),
                body_json: None,
            },
        ],
        View::EventStream => vec![Request {
            method: "GET",
            path: "/api/v1/events/stream".into(),
            body_json: None,
        }],
    }
}

/// The views the Studio renders (the §25.2 screen set — Dashboard, Task, Plan,
/// Agent Hierarchy, Agent, Message, Tool Execution, Skill, Approval, Event, Result —
/// 11 screens; `EventStream` is not a screen but the real-time face `P7-06`'s board
/// is driven from, `ATH-STUDIO-003`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    /// The Dashboard (all Tasks at a glance).
    Dashboard,
    /// One Task's view.
    Task(String),
    /// One Task's Plan view (the DAG + revisions).
    Plan(String),
    /// One Task's Agent Hierarchy view (`GET /api/v1/hierarchies/{task_id}`).
    AgentHierarchy(String),
    /// The Agent View: definitions **and** instances, never conflated
    /// (`ATH-API-013`).
    Agent,
    /// One Task's Message view.
    Message(String),
    /// The Tool Execution View.
    ToolExecution,
    /// The Skill View (+ its `P7-05` enable/disable actions).
    Skill,
    /// The Approvals view (+ its `P7-05` approve/reject actions).
    Approvals,
    /// The Event View (the replayed rows behind the live tail).
    Event,
    /// One Task's Result View (+ the Evaluations that decide acceptance,
    /// `ATH-EVAL-005`).
    Result(String),
    /// The Event Stream view (the SSE channel the live board reduces).
    EventStream,
}

/// The SSE client's kernel face: one streamed chunk's rows — the `data:` payloads the
/// API's event stream publishes; the Studio slashes its own decoration (`no chain of
/// thought, no secret display` — `ATH-MEM-004` / `ATH-EVENT-007`).
///
/// # Errors
/// A malformed chunk's rows are skipped, not invented; an absent chunk renders empty.
#[must_use]
pub fn parse_stream_chunk(chunk: &str) -> Vec<EventRow> {
    chunk
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|payload| serde_json::from_str::<serde_json::Value>(payload).ok())
        .map(|face| EventRow {
            event_type: face
                .get("event_type")
                .and_then(|row| row.as_str())
                .unwrap_or_default()
                .to_string(),
            task_id: face
                .get("task_id")
                .and_then(|row| row.as_str())
                .map(str::to_string),
            occurred_at: face
                .get("occurred_at")
                .and_then(|row| row.as_str())
                .unwrap_or_default()
                .to_string(),
        })
        .collect()
}

/// The path helper the wasm face's fetches ride (an asset-path guard — no `..` ever
/// resolves into a Core internal; the module-path helper keeps a `PathBuf` face for
/// the static server's asset emission).
#[must_use]
pub fn safe_api_path(aid: &str) -> Option<String> {
    if aid.contains("..") {
        return None;
    }
    Some(aid.to_string())
}

#[cfg(test)]
mod client_tests;
