//! Tests for the action faces (P7-05).

use super::*;

#[test]
fn the_skill_actions_demand_the_pin_and_carry_the_decision_face() {
    let faces = skill_actions(&SkillActions {
        id: "the-skill".into(),
        version: "1.0.0".into(),
    })
    .expect("faces");
    assert_eq!(faces[0].label, "Enable the-skill@1.0.0");
    assert!(
        faces[0]
            .body_json
            .as_deref()
            .is_some_and(|body| body.contains("enable") && body.contains("1.0.0"))
    );
    assert_eq!(faces[1].label, "Disable the-skill@1.0.0");
    assert!(
        faces[1]
            .body_json
            .as_deref()
            .is_some_and(|body| body.contains("disable"))
    );
    // The pin-less face refuses (the view renders the control disabled):
    let refuse = skill_actions(&SkillActions::default());
    assert!(refuse.is_err());
}

#[test]
fn only_requested_rows_offer_approval_actions() {
    let rows = [
        ApprovalRow {
            id: "ap-pending".into(),
            canonical_descriptor: "git push to origin (main)".into(),
            risk_level: "high".into(),
            state: "REQUESTED".into(),
            agent_reason: None,
        },
        ApprovalRow {
            id: "ap-done".into(),
            canonical_descriptor: "fs_write".into(),
            risk_level: "medium".into(),
            state: "CONSUMED".into(),
            agent_reason: None,
        },
    ];
    let faces = approval_actions(&rows);
    assert_eq!(faces.len(), 1, "only the REQUESTED row carries actions");
    assert_eq!(faces[0].0, "ap-pending");
    assert!(faces[0].1[0].path.ends_with("/approve"));
    assert!(faces[0].1[1].path.ends_with("/reject"));
    assert!(faces[0].1[1].label.contains("Reject"));
}

#[test]
fn the_approval_screen_escapes_the_prose_and_renders_the_controls() {
    let rows = [ApprovalRow {
        id: "ap-03".into(),
        canonical_descriptor: "write <prod> & deploy".into(),
        risk_level: "high".into(),
        state: "REQUESTED".into(),
        agent_reason: Some("looks <ready> &amp; fine".into()),
    }];
    let face = approval_screen(&rows);
    assert!(face.contains("write &lt;prod&gt; &amp; deploy"));
    assert!(face.contains("&lt;ready&gt;"));
    assert!(face.contains("data-endpoint=\"/api/v1/approvals/ap-03/approve\""));
    assert!(face.contains("data-endpoint=\"/api/v1/approvals/ap-03/reject\""));
}

#[test]
fn the_verb_face_names_the_last_segment() {
    assert_eq!(verb_face("/api/v1/approvals/ap-01/approve"), "approve");
    assert_eq!(verb_face("/"), "");
}
