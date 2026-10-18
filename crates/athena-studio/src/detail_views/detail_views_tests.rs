//! Tests for the detail views (P7-04).

use super::*;
use crate::client::ApprovalRow;

#[test]
fn the_agent_view_lists_the_definitions() {
    let agents = [AgentRow {
        id: "timing-lead".into(),
        level: "lead".into(),
        persona_ref: "lead".into(),
        model: "claude-opus-5".into(),
    }];
    let face = agent_view(&agents);
    assert!(face.contains("<li id=\"agent-timing-lead\">timing-lead — level lead"));
    assert!(face.contains("claude-opus-5"));
}

#[test]
fn the_message_view_escapes_the_bodies() {
    let messages = [MessageRow {
        id: "m-01".into(),
        thread_id: "th-1".into(),
        from: "producer".into(),
        body: "run <tests> & citations".into(),
    }];
    let face = message_view(&messages);
    assert!(face.contains("run &lt;tests&gt; &amp; citations"));
    assert!(!face.contains("<tests>"));
}

#[test]
fn the_tool_view_carries_the_descriptor_foremost_and_the_risk() {
    let rows = [ToolRow {
        id: "tx-01".into(),
        tool: "fs_write".into(),
        operation_descriptor: "write /workspace/report.md".into(),
        risk_level: "medium".into(),
        status: "succeeded".into(),
    }];
    let face = tool_view(&rows);
    assert!(face.contains("write /workspace/report.md (risk medium, succeeded)"));
}

#[test]
fn the_result_view_marks_the_self_declared_status() {
    let rows = [ResultRow {
        id: "res-01".into(),
        agent_id: "builder".into(),
        status: "success".into(),
        output: "the report exists".into(),
    }];
    let face = result_view(&rows);
    assert!(face.contains("self-declared"));
    assert!(face.contains("never the acceptance basis (ATH-EVAL-005)"));
    assert!(face.contains("the Evaluation rows decide"));
}

#[test]
fn the_event_detail_view_couples_the_approval_faces() {
    let events = [EventRow {
        event_type: "ApprovalRequested".into(),
        task_id: Some("task-01JA".into()),
        occurred_at: "2026-10-18T03:00:00Z".into(),
    }];
    let approvals = [ApprovalRow {
        id: "ap-02".into(),
        canonical_descriptor: "fs_write to prod".into(),
        risk_level: "high".into(),
        state: "REQUESTED".into(),
        agent_reason: None,
    }];
    let face = event_detail_view(&events, &approvals);
    assert!(face.contains("fs_write to prod"));
    assert!(face.contains("ApprovalRequested"));
}
