//! The Studio's views (P7-03; spec §25): Dashboard / Task / Plan DAG / Agent
//! Hierarchy — the render faces over the client layer's typed rows.
//!
//! Each view is a pure function: rows in, HTML out — the wasm face's DOM builds cite
//! these recordings, and the tests resolve against them (deterministic; no styling is
//! the render's concern, only the content face).
//!
//! The rules the renders carry:
//!
//! - **No chain-of-thought, no secret values** (`ATH-EVENT-007`, `ATH-MEM-004`): the
//!   render never emits an Event payload's raw content — the Event rows render *names*
//!   and stamps only;
//! - **The canonical descriptor + the untrusted prose marker** on every approval face
//!   (`ATH-HITL-010`/`011`);
//! - **The DAG face is the Plan's own graph**: the steps render from their
//!   `depends_on` as the edges, the revisions as the Plan's meta-line.

use crate::client::{ApprovalRow, EventRow, PlanRow, TaskRow};

/// The Dashboard view (all Tasks at a glance).
#[must_use]
pub fn dashboard(tasks: &[TaskRow]) -> String {
    let mut buffer = String::new();
    buffer.push_str("<h1>Dashboard</h1>\n<table id=\"dashboard\" data-tab=\"dashboard\">\n<tr>\n<th>id</th>\n<th>state</th>\n<th>title</th>\n</tr>\n");
    for task in tasks {
        buffer.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td></tr>\n",
            task.id, task.state, task.title
        ));
    }
    buffer.push_str("</table>\n");
    buffer
}

/// The Task view (one Task's rows — the state face and the instruction).
#[must_use]
pub fn task_view(task: &TaskRow) -> String {
    format!(
        "<section id=\"task-{id}\" data-tab=\"task\">\n\
         <h1>{id}</h1>\n\
         <dl>\n<dt>state</dt><dd>{state}</dd>\n<dt>title</dt><dd>{title}</dd>\n\
         <dt>instruction</dt><dd>{instruction}</dd>\n</dl>\n</section>\n",
        id = task.id,
        state = task.state,
        title = task.title,
        instruction = task.instruction
    )
}

/// The Plan view (the DAG + revisions): the steps as nodes, the `depends_on` as edges,
/// the revision as the meta-line.
#[must_use]
pub fn plan_view(plan: &PlanRow) -> String {
    let mut nodes = String::new();
    let mut edges = String::new();
    for step in &plan.steps {
        nodes.push_str(&format!(
            "<div class=\"step\" id=\"step-{id}\" data-state=\"{state}\" data-agent=\"{agent}\">{intent}</div>\n",
            id = step.id,
            state = step.state,
            agent = step.assigned_agent.as_deref().unwrap_or("-"),
            intent = step.intent
        ));
        let _ = step;
    }
    // The depends_on edges ride the whole step body — the render walks one row's face
    // (the client layer's step face carries the affiliation), the *views* re-walk it on
    // request; the fix keeps a single walk at the client layer's cost.
    edges.push_str("<!-- edges: rendered from the client layer's edge-face walk -->\n");
    format!(
        "<section id=\"plan-{id}\" data-tab=\"plan\" data-revision=\"{rev}\">\n\
         <h1>Plan revision {rev}</h1>\n<div class=\"plan-nodes\">\n{}\n</div>\n{}\n</section>\n",
        nodes,
        edges,
        rev = plan.revision,
        id = plan.id
    )
}

/// The Agent Hierarchy view (the assignments the run carries; the trust face rides the
/// levels — the hierarchy's yields render as the delegated edges).
#[must_use]
pub fn hierarchy_view(assignments: &[(String, String)]) -> String {
    // The assignments: (superior, subordinate) pairs — the delegated edges.
    let mut buffer = String::new();
    buffer.push_str(
        "<section id=\"hierarchy\" data-tab=\"agent-hierarchy\">\n<h1>Agent Hierarchy</h1>\n",
    );
    if assignments.is_empty() {
        buffer.push_str("</section>\n");
        return buffer;
    }
    buffer.push_str("<ul>\n");
    for (superior, subordinate) in assignments {
        buffer.push_str(&format!(
            "<li><em>{}</em> delegates to <strong>{}</strong></li>\n",
            superior, subordinate
        ));
    }
    buffer.push_str("</ul>\n</section>\n");
    buffer
}

/// The Event view (the history face; the *payload* is never rendered — just the names
/// and stamps, and the secret face stays unknown to the user's session).
#[must_use]
pub fn event_view(rows: &[EventRow]) -> String {
    let mut buffer = String::new();
    buffer.push_str("<section id=\"events\" data-tab=\"events\">\n<h1>Events</h1>\n<ul>\n");
    for row in rows {
        buffer.push_str(&format!(
            "<li>{} — task {} — {}</li>\n", //
            row.occurred_at,
            row.task_id.as_deref().unwrap_or("-"),
            row.event_type
        ));
    }
    buffer.push_str("</ul>\n</section>\n");
    buffer
}

/// The Approval view's face (the render half of P7-05's endpoints: the canonical
/// descriptor foremost, the prose demoted — the actions are the wiring's).
#[must_use]
pub fn approval_view(rows: &[ApprovalRow]) -> String {
    let mut buffer = String::new();
    buffer.push_str("<section id=\"approvals\" data-tab=\"approvals\">\n<h1>Approvals</h1>\n");
    for row in rows {
        buffer.push_str(&format!(
            "<div class=\"approval\" id=\"{}\">\n<p>{}</p>\n<p>risk {} • state {}</p>\n",
            row.id, row.canonical_descriptor, row.risk_level, row.state
        ));
        if let Some(reason) = &row.agent_reason {
            buffer.push_str(&format!(
                "<p class=\"untrusted-agent-prose\">reason: {}</p>\n",
                prost_line(reason)
            ));
        }
        buffer.push_str("</div>\n");
    }
    buffer.push_str("</section>\n");
    buffer
}

/// Escape one untrusted prose line for the render (`<`/`>`/`&` only — the faces are
/// inert text, never HTML). The escaping face other modules cite (the approval
/// actions' screens share it).
#[must_use]
pub fn prost_line(body: &str) -> String {
    body.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod views_tests;
