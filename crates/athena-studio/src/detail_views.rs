//! The detail views (P7-04; spec §25's row set): Agent / Message / Tool Execution /
//! Result / Event — the same pure-render contract P7-03's views hold: rows in, HTML
//! out, names and stamps over payload bodies, the untrusted prose escaped.

use crate::client::{ApprovalRow, EventRow};
use serde::Deserialize;

/// One Agent definition's render row (`GET /api/v1/agents/{id}`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct AgentRow {
    /// The agent slug.
    pub id: String,
    /// The level (producer / director / lead / worker).
    pub level: String,
    /// The persona reference.
    pub persona_ref: String,
    /// The model.
    pub model: String,
}

/// One Message's render row (`GET /api/v1/instances/{id}/messages`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct MessageRow {
    /// The message id.
    pub id: String,
    /// The thread it rides.
    pub thread_id: String,
    /// The sender's slug.
    pub from: String,
    /// The body (escaped at the render).
    pub body: String,
}

/// One Tool execution's render row (`GET /api/v1/tool-executions`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct ToolRow {
    /// The execution id.
    pub id: String,
    /// The tool's name.
    pub tool: String,
    /// The canonical descriptor (`ATH-HITL-010`; escaped).
    pub operation_descriptor: String,
    /// The risk.
    pub risk_level: String,
    /// The status.
    pub status: String,
}

/// One Result's render row (`GET /api/v1/tasks/{id}/results`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct ResultRow {
    /// The Result id.
    pub id: String,
    /// The agent that delivered.
    pub agent_id: String,
    /// The self-declared status (never the acceptance basis, `ATH-EVAL-005` — marked).
    pub status: String,
    /// The output's face (escaped; readings not the acceptance).
    pub output: String,
}

/// The Agent view (the definitions; the instance face rode the hierarchy render).
#[must_use]
pub fn agent_view(agents: &[AgentRow]) -> String {
    let mut buffer = to_section("agents", "Agent definitions");
    buffer.push_str("<ul>\n");
    for agent in agents {
        buffer.push_str(&format!(
            "<li id=\"agent-{id}\">{id} — level {level}, persona {persona}, model {model}</li>\n",
            id = agent.id,
            level = agent.level,
            persona = agent.persona_ref,
            model = agent.model
        ));
    }
    buffer.push_str("</ul>\n</section>\n");
    buffer
}

/// The Message view (the instance's conversation; the bodies escape).
#[must_use]
pub fn message_view(messages: &[MessageRow]) -> String {
    let mut buffer = to_section("messages", "Messages");
    buffer.push_str("<ul>\n");
    for message in messages {
        buffer.push_str(&format!(
            "<li id=\"msg-{id}\"><em>{from}</em> [{thread}]: {body}</li>\n",
            id = message.id,
            from = message.from,
            thread = message.thread_id,
            body = prost_line(&message.body)
        ));
    }
    buffer.push_str("</ul>\n</section>\n");
    buffer
}

/// The Tool Execution view (the canonical descriptor foremost — `ATH-HITL-010`).
#[must_use]
pub fn tool_view(rows: &[ToolRow]) -> String {
    let mut buffer = to_section("tool-executions", "Tool executions");
    buffer.push_str("<ul>\n");
    for row in rows {
        buffer.push_str(&format!(
            "<li id=\"tx-{id}\">{tool} — {descriptor} (risk {risk}, {status})</li>\n",
            id = row.id,
            tool = row.tool,
            descriptor = prost_line(&row.operation_descriptor),
            risk = row.risk_level,
            status = row.status
        ));
    }
    buffer.push_str("</ul>\n</section>\n");
    buffer
}

/// The Result view (the self-declared status marked never-the-acceptance-basis).
#[must_use]
pub fn result_view(rows: &[ResultRow]) -> String {
    let mut buffer = to_section("results", "Results");
    buffer.push_str("<ul>\n");
    for row in rows {
        buffer.push_str(&format!(
            "<li id=\"res-{id}\">{agent} reported <span class=\"self-declared\">{status}</span>: {output}</li>\n",
            id = row.id,
            agent = row.agent_id,
            status = row.status,
            output = prost_line(&row.output)
        ));
    }
    buffer.push_str("<li class=\"note\">the self-declared status is never the acceptance basis (ATH-EVAL-005); the Evaluation rows decide.</li>\n</ul>\n</section>\n");
    buffer
}

/// The Event view's *detail* variant — payload rows render names + stamps
/// (the Event view's list face rides P7-03's render; this section carries the
/// approvals' faces too, since the events' approval face is the run's own).
#[must_use]
pub fn event_detail_view(events: &[EventRow], approvals: &[ApprovalRow]) -> String {
    let mut buffer = to_section("event-details", "Events (with approval faces)");
    buffer.push_str("<ul>\n");
    for event in events {
        buffer.push_str(&format!(
            "<li>{} — task {} — {}</li>\n", //
            event.occurred_at,
            event.task_id.as_deref().unwrap_or("-"),
            event.event_type
        ));
    }
    buffer.push_str("</ul>\n");
    buffer.push_str(&prost_line(&crate::views::approval_view(approvals)));
    buffer
}

fn to_section(id: &str, title: &str) -> String {
    format!("<section id=\"{id}\" data-tab=\"{id}\">\n<h1>{title}</h1>\n")
}

/// Escape one untrusted prose line (the same face P7-03 carries).
fn prost_line(body: &str) -> String {
    body.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod detail_views_tests;
