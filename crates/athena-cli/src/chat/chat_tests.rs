//! Tests for the chat interaction layer (P6-01).

use super::*;

// --- ATH-CLI-005: the progress display -----------------------------------------------------

#[test]
fn the_progress_render_names_the_state_step_and_agents() {
    let progress = Progress {
        task_id: "task-01JA".into(),
        task_state: ChatTaskState::Running,
        current_step: Some("analyze the failing build".into()),
        running_agents: vec!["builder".into(), "linter".into()],
    };
    let text = progress.rendered();
    assert!(text.contains("[RUNNING] task task-01JA"), "{text}");
    assert!(text.contains("analyze the failing build"));
    assert!(text.contains("builder, linter"));
    // No step and no agents: nothing invented.
    let bare = Progress::default();
    assert!(!bare.rendered().contains("now:") && !bare.rendered().contains("running:"));
}

// --- ATH-CLI-004: the approval presentation -----------------------------------------------

#[test]
fn the_approval_prompt_puts_the_canonical_descriptor_first_and_marks_the_prose() {
    let prompt = ApprovalPrompt {
        canonical_descriptor: "git push to origin (main)".into(),
        risk_level: "high".into(),
        expires_at: "2026-10-18T11:00:00Z".into(),
        agent_reason: Some("the release looks ready".into()),
        agent_impact: Some("pushes main".into()),
        approval_id: "ap-01".into(),
    };
    let text = prompt.rendered();
    // The descriptor is the first line of content.
    assert!(text.starts_with("APPROVAL REQUIRED [ap-01]"), "{text}");
    assert!(text.contains("git push to origin (main)"));
    // The prose is demoted with the visible untrusted marker — both prose lines.
    assert_eq!(
        text.matches("[untrusted agent prose]").count(),
        2,
        "both agent prose lines carry the marker: {text}"
    );
}

#[test]
fn the_approval_decision_plan_maps_the_phrasing() {
    let prompt = ApprovalPrompt {
        approval_id: "ap-01".into(),
        ..ApprovalPrompt::default()
    };
    let plan = approval_decision_plan(&prompt, "approve").expect("approve");
    assert_eq!(plan.path, "/api/v1/approvals/ap-01/approve");
    let plan = approval_decision_plan(&prompt, "no").expect("reject");
    assert_eq!(plan.path, "/api/v1/approvals/ap-01/reject");
    // Neither approve nor reject: silence — refused, never read as consent.
    assert!(approval_decision_plan(&prompt, "maybe later").is_err());
}

// --- The clarification answering (ATH-HITL-020) --------------------------------------------

#[test]
fn the_clarification_prompt_and_answer_plan_shape_the_answers() {
    let prompt = ClarificationPrompt {
        clarification_id: "cl-01".into(),
        task_id: "task-01JA".into(),
        question: "which branch holds the failing build?".into(),
        expires_at: "2026-10-19T00:00:00Z".into(),
    };
    let text = prompt.rendered();
    assert!(text.contains("which branch holds the failing build?"));
    assert!(text.contains("silence fails the Task"));

    let plan = answer_plan(&prompt, "branch release/v2").expect("answered");
    assert_eq!(plan.method, "POST");
    assert_eq!(plan.path, "/api/v1/tasks/task-01JA/answer");
    assert!(plan.body_json.contains(r#""clarification_id":"cl-01""#));
    assert!(plan.body_json.contains("branch release/v"));

    // Silence refuses:
    assert!(matches!(
        answer_plan(&prompt, "  "),
        Err(ChatTurnFault::EmptyAnswer)
    ));
    // A prompt with no question is not an answerable record:
    let empty = ClarificationPrompt::default();
    assert!(matches!(
        answer_plan(&empty, "anything"),
        Err(ChatTurnFault::UnknownPending(_))
    ));
}
