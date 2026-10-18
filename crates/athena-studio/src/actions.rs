//! The action faces (P7-05; spec §25's action rows): the Skill View's enable/disable
//! and the Approval View's approve/reject — each action is a **plan** the transport
//! posts, carrying the administered `(id, version)` (the pin never floats) and riding
//! the core machinery's own gates (the API refuses the off-table transitions).
//!
//! The screen faces (`ATH-HITL-010`/`011`): the approval actions show the canonical
//! descriptor + the risk *from configuration*; the Agent's prose stays escaped /
//! demoted (never the decision's basis).

use crate::client::{ApprovalRow, TaskRow};
use serde::Deserialize;

/// The action plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionPlan {
    /// The HTTP method.
    pub method: &'static str,
    /// The endpoint.
    pub path: String,
    /// The JSON body.
    pub body_json: Option<String>,
    /// The button's face (what the view renders on the control).
    pub label: String,
}

/// The Skill actions.
#[derive(Default, Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SkillActions {
    /// The skill's slug.
    pub id: String,
    /// The administered version (**mandatory** — the pin rides).
    pub version: String,
}

/// The two Skill action plans (admin-only; the pin is mandatory).
///
/// # Errors
/// An empty pin refuses (the view renders the control disabled).
pub fn skill_actions(actions: &SkillActions) -> Result<[ActionPlan; 2], String> {
    if actions.id.trim().is_empty() || actions.version.trim().is_empty() {
        return Err("the (id, version) pin is mandatory (ATH-SKILL-020)".into());
    }
    Ok([
        ActionPlan {
            method: "POST",
            path: format!("/api/v1/skills/{}/decision", actions.id),
            body_json: Some(
                r#"{"decision":"enable","version":"VERSION"}"#.replace("VERSION", &actions.version),
            ),
            label: format!("Enable {}@{}", actions.id, actions.version),
        },
        ActionPlan {
            method: "POST",
            path: format!("/api/v1/skills/{}/decision", actions.id),
            body_json: Some(
                r#"{"decision":"disable","version":"VERSION"}"#
                    .replace("VERSION", &actions.version),
            ),
            label: format!("Disable {}@{}", actions.id, actions.version),
        },
    ])
}

/// The Approval actions (approve / reject): the plans ride the approval rows.
///
/// # Errors
/// An approval in a terminal state offers no actions (the view renders none).
pub fn approval_actions(rows: &[ApprovalRow]) -> Vec<(String, [ActionPlan; 2])> {
    rows.iter()
        .filter(|row| row.state == "REQUESTED")
        .map(|row| {
            (
                row.id.clone(),
                [
                    ActionPlan {
                        method: "POST",
                        path: format!("/api/v1/approvals/{}/approve", row.id),
                        body_json: None,
                        label: format!("Approve: {}", prost_line(&row.canonical_descriptor)),
                    },
                    ActionPlan {
                        method: "POST",
                        path: format!("/api/v1/approvals/{}/reject", row.id),
                        body_json: None,
                        label: format!("Reject: {}", prost_line(&row.canonical_descriptor)),
                    },
                ],
            )
        })
        .collect()
}

/// The Approval View face (the render half; the actions' faces ride above).
#[must_use]
pub fn approval_screen(rows: &[ApprovalRow]) -> String {
    let mut buffer = String::new();
    buffer.push_str("<h1>Approvals</h1>\n");
    for row in rows {
        buffer.push_str(&format!(
            "<div id=\"{}\"><p>{}</p>\n<p>risk {} • state {}</p>\n",
            row.id, row.canonical_descriptor, row.risk_level, row.state
        ));
        if let Some(reason) = &row.agent_reason {
            buffer.push_str(&format!(
                "<p class=\"untrusted-agent-prose\">reason: {}</p>\n",
                crate::views::prost_line(reason)
            ));
        }
        for plan in approval_actions(std::slice::from_ref(row))
            .into_iter()
            .flat_map(|(_, plans)| plans.into_iter())
        {
            buffer.push_str(&format!(
                "<button data-endpoint=\"{}\" data-verb=\"{}\">{}</button>\n",
                plan.path,
                verb_face(&plan.path),
                plan.label
            ));
        }
        buffer.push_str("</div>\n");
    }
    buffer
}

fn prost_line(body: &str) -> String {
    body.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The action's verb face for the button's `data-verb` attribute (the plan above
/// carries the endpoint's *last segment*).
pub(crate) fn verb_face(path: &str) -> String {
    path.rsplit('/').next().unwrap_or_default().to_string()
}

/// The Task strip the approval actions sit on (the Approval View knows its Task —
/// for the run's face and the event stream's own area).
#[must_use]
pub fn approval_task_strip(tasks: &[TaskRow]) -> String {
    let mut buffer = String::new();
    buffer.push_str("<div id=\"approval-tasks\">\n");
    for task in tasks {
        buffer.push_str(&format!(
            "<span data-task=\"{}\">{}</span>\n",
            task.id, task.id
        ));
    }
    buffer.push_str("</div>\n");
    buffer
}

#[cfg(test)]
mod actions_tests;
