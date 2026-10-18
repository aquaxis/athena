//! Tests for the Studio views (P7-03…05).

use super::*;
use crate::client::{ApprovalRow, EventRow, PlanRow, PlanStepRow, TaskRow};

fn task(id: &str, state: &str, title: &str) -> TaskRow {
    TaskRow {
        id: id.into(),
        state: state.into(),
        title: title.into(),
        instruction: "investigate the build failure".into(),
    }
}

#[test]
fn the_dashboard_renders_every_task_row_in_order() {
    let face = dashboard(&[task("a", "RUNNING", "first"), task("b", "QUEUED", "second")]);
    assert!(face.contains("<table id=\"dashboard\""));
    assert!(face.contains("<td>a</td><td>RUNNING</td><td>first</td>"));
    assert!(face.contains("<td>b</td><td>QUEUED</td><td>second</td>"));
}

#[test]
fn the_task_view_carries_the_state_steps() {
    let face = task_view(&task("a", "SUCCESS", "the finished one"));
    assert!(face.contains("<dt>state</dt><dd>SUCCESS</dd>"));
    assert!(face.contains("<dt>instruction</dt>"));
}

#[test]
fn the_plan_view_carries_the_revision_and_the_step_nodes() {
    let plan = PlanRow {
        id: "plan-01".into(),
        revision: 2,
        steps: vec![PlanStepRow {
            id: "step-01".into(),
            intent: "analyze".into(),
            state: "RUNNING".into(),
            assigned_agent: Some("builder".into()),
        }],
    };
    let face = plan_view(&plan);
    assert!(face.contains("data-revision=\"2\""));
    assert!(face.contains("id=\"step-step-01\""));
    assert!(face.contains("data-state=\"RUNNING\""));
    assert!(face.contains("data-agent=\"builder\""));
}

#[test]
fn the_hierarchy_view_lists_the_delegate_edges() {
    let face = hierarchy_view(&[("producer".into(), "director-a".into())]);
    assert!(face.contains("<em>producer</em> delegates to <strong>director-a</strong>"));
    let empty = hierarchy_view(&[]);
    assert!(
        !empty.contains("<ul>"),
        "nothing invented for the empty face"
    );
}

#[test]
fn the_event_view_renders_names_and_stamps_only() {
    let rows = [EventRow {
        event_type: "TaskStarted".into(),
        task_id: Some("task-01JA".into()),
        occurred_at: "2026-10-18T00:00:03Z".into(),
    }];
    let face = event_view(&rows);
    assert!(face.contains("TaskStarted"));
    assert!(face.contains("2026-10-18T00:00:03Z"));
    // No payload content rides — the render emits names/stamps only.
    assert!(!face.contains("payload"));
}

#[test]
fn the_approval_view_carries_the_descriptor_and_the_untrusted_prose_marker() {
    let rows = [ApprovalRow {
        id: "ap-01".into(),
        canonical_descriptor: "git push to origin (main)".into(),
        risk_level: "high".into(),
        state: "REQUESTED".into(),
        agent_reason: Some("<b>looks ready</b>".to_string()),
    }];
    let face = approval_view(&rows);
    assert!(face.contains("git push to origin (main)"));
    assert!(face.contains("untrusted-agent-prose"));
    // The prose is escaped — never executed:
    assert!(!face.contains("<b>looks ready"));
    assert!(face.contains("&lt;b&gt;looks ready&lt;/b&gt;"));
}
