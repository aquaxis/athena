//! Phase-7 Studio group (P7-07): the §25.2 screen set is fully covered, the UI rides
//! **only** the published Core API surface (`ATH-API-001`, `ATH-STUDIO-001`), and the
//! Studio and the CLI draw the same API faces for every shared control flow — the
//! same-API contract (`ATH-NFR-009`). Passing closes Phase 7.

use athena_studio::actions::{SkillActions, approval_actions, approval_screen, skill_actions};
use athena_studio::client::{
    ApprovalRow, EventRow, PlanRow, PlanStepRow, TaskRow, View, requests_for_view,
};
use athena_studio::detail_views::{
    AgentRow, MessageRow, ResultRow, ToolRow, agent_view, event_detail_view, message_view,
    result_view, tool_view,
};
use athena_studio::live::{LiveBoard, render as live_render};
use athena_studio::views::{
    approval_view, dashboard, event_view, hierarchy_view, plan_view, task_view,
};
use std::path::Path;

// --- The §22 published faces the UI may ride ------------------------------------------

const PUBLISHED: &[&str] = &[
    "/api/v1/healthz",
    "/api/v1/status",
    "/api/v1/metrics",
    "/api/v1/events",
    "/api/v1/events/stream",
    "/api/v1/tasks",
    "/api/v1/tasks/{id}",
    "/api/v1/tasks/{id}/run",
    "/api/v1/tasks/{id}/cancel",
    "/api/v1/tasks/{id}/results",
    "/api/v1/tasks/{id}/events",
    "/api/v1/tasks/{id}/plan",
    "/api/v1/tasks/{id}/messages",
    "/api/v1/tasks/{id}/answer",
    "/api/v1/conversations",
    "/api/v1/conversations/{id}/messages",
    "/api/v1/plans/{id}",
    "/api/v1/plans/{id}/revisions",
    "/api/v1/plans/{id}/steps",
    "/api/v1/agents",
    "/api/v1/agents/{id}",
    "/api/v1/instances",
    "/api/v1/instances/{id}",
    "/api/v1/instances/{id}/messages",
    "/api/v1/instances/{id}/stop",
    "/api/v1/hierarchies/{id}",
    "/api/v1/tool-executions",
    "/api/v1/evaluations",
    "/api/v1/runtimes",
    "/api/v1/skills",
    "/api/v1/skills/{id}",
    "/api/v1/skills/{id}/decision",
    "/api/v1/approvals",
    "/api/v1/approvals/{id}",
    "/api/v1/approvals/{id}/approve",
    "/api/v1/approvals/{id}/reject",
];

/// A planned path is published when some §22 template matches it segment-wise, with
/// `{id}` matching exactly one segment and a shorter template matching as a prefix.
fn published(path: &str) -> bool {
    PUBLISHED.iter().any(|template| {
        let pattern: Vec<&str> = template.trim_start_matches('/').split('/').collect();
        let planned: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        planned.len() >= pattern.len()
            && pattern
                .iter()
                .enumerate()
                .all(|(index, segment)| *segment == "{id}" || planned[index] == *segment)
    })
}

fn assert_published(path: &str, where_: &str) {
    assert!(
        path.starts_with("/api/v1/") && !path.contains(".."),
        "{where_}: the UI rides the Core API only (ATH-API-001): {path}"
    );
    assert!(published(path), "{where_}: no §22 face covers {path}");
}

fn all_screens() -> Vec<(&'static str, View)> {
    vec![
        ("Dashboard", View::Dashboard),
        ("Task View", View::Task("task-01JA".into())),
        ("Plan View", View::Plan("task-01JA".into())),
        (
            "Agent Hierarchy View",
            View::AgentHierarchy("task-01JA".into()),
        ),
        ("Agent View", View::Agent),
        ("Message View", View::Message("task-01JA".into())),
        ("Tool Execution View", View::ToolExecution),
        ("Skill View", View::Skill),
        ("Approval View", View::Approvals),
        ("Event View", View::Event),
        ("Result View", View::Result("task-01JA".into())),
        ("Event Stream (the live face)", View::EventStream),
    ]
}

fn approval_row(id: &str, state: &str) -> ApprovalRow {
    ApprovalRow {
        id: id.into(),
        canonical_descriptor: "shell(rm -rf /tmp/build)".into(),
        risk_level: "high".into(),
        state: state.into(),
        agent_reason: Some("it is fine".into()),
    }
}

// --- row 1: the §25.2 screens are all covered -------------------------------------------

#[test]
fn every_section_25_screen_has_a_plan_and_a_render() {
    for (name, view) in all_screens() {
        let plans = requests_for_view(&view);
        assert!(!plans.is_empty(), "{name}: the view rides a request plan");
        for plan in &plans {
            assert_eq!(plan.method, "GET", "{name}: reads are GETs");
            assert_published(&plan.path, name);
        }
    }
    // Every screen's render face renders from real row shapes (names + stamps in,
    // payload-free HTML out — the render contract the per-view unit tests hold).
    let task = TaskRow {
        id: "task-01JA".into(),
        state: "RUNNING".into(),
        title: "T".into(),
        instruction: "I".into(),
    };
    assert!(dashboard(std::slice::from_ref(&task)).contains("task-01JA"));
    assert!(task_view(&task).contains("RUNNING"));
    let plan = PlanRow {
        id: "plan-01JA".into(),
        revision: 1,
        steps: vec![PlanStepRow {
            id: "ps-01JA".into(),
            intent: "step".into(),
            state: "READY".into(),
            assigned_agent: Some("worker".into()),
        }],
    };
    assert!(plan_view(&plan).contains("ps-01JA"));
    assert!(
        hierarchy_view(&[("producer".into(), "worker".into())]).contains("worker"),
        "the Agent Hierarchy View renders the delegate edges"
    );
    assert!(agent_view(&[AgentRow::default()]).len() > 10);
    assert!(message_view(&[MessageRow::default()]).len() > 10);
    assert!(tool_view(&[ToolRow::default()]).len() > 10);
    assert!(result_view(&[ResultRow::default()]).len() > 10);
    let event = EventRow {
        event_type: "TaskStarted".into(),
        task_id: Some("task-01JA".into()),
        occurred_at: "2026-10-18T00:00:00Z".into(),
    };
    assert!(event_view(std::slice::from_ref(&event)).contains("TaskStarted"));
    assert!(event_detail_view(std::slice::from_ref(&event), &[]).len() > 10);
    let approval = approval_row("apl-01JA", "REQUESTED");
    assert!(approval_screen(std::slice::from_ref(&approval)).contains("shell"));
    assert!(approval_view(std::slice::from_ref(&approval)).len() > 10);
    let mut board = LiveBoard::from_tasks(vec![task.clone()]);
    board.apply_event(event.clone());
    assert!(live_render(&board).contains("data-state=\"RUNNING\""));
}

// --- row 2: the action plans ride the published surface too ------------------------------

#[test]
fn every_action_plan_rides_the_published_surface() {
    let skill = skill_actions(&SkillActions {
        id: "the-skill".into(),
        version: "1.2.0".into(),
    })
    .expect("the pin is stated");
    let approval = approval_row("apl-01JA", "REQUESTED");
    for (_id, [approve, reject]) in approval_actions(std::slice::from_ref(&approval)) {
        assert_published(&approve.path, "the approval approve action");
        assert_published(&reject.path, "the approval reject action");
    }
    for action in skill {
        assert_published(&action.path, "the skill decision action");
    }
    // A pin-less skill action refuses rather than plans (the (id, version) rule).
    assert!(skill_actions(&SkillActions::default()).is_err());
}

// --- row 3: Studio and CLI draw the same faces for the shared control flows ---------------

#[test]
fn studio_and_cli_ride_the_same_api_for_the_shared_control_flows() {
    let args =
        |values: &[&str]| -> Vec<String> { values.iter().map(|v| (*v).to_string()).collect() };

    // (flow name, CLI verb, CLI verb args, the CLI's expected "METHOD PATH", and the
    // Studio-side planned paths that must carry the same face):
    type FlowRow = (
        &'static str,
        &'static str,
        Vec<String>,
        (&'static str, &'static str),
        Vec<String>,
    );
    let flows: [FlowRow; 7] = [
        (
            "task inspection",
            "task",
            args(&["show", "task-01JA"]),
            ("GET", "/api/v1/tasks/task-01JA"),
            requests_for_view(&View::Task("task-01JA".into()))
                .into_iter()
                .map(|r| r.path.clone())
                .collect(),
        ),
        (
            "plan inspection",
            "plan",
            args(&["show", "task-01JA"]),
            ("GET", "/api/v1/tasks/task-01JA/plan"),
            requests_for_view(&View::Plan("task-01JA".into()))
                .into_iter()
                .map(|r| r.path.clone())
                .collect(),
        ),
        (
            "approval decision",
            "approval",
            args(&["approve", "apl-01JA"]),
            ("POST", "/api/v1/approvals/apl-01JA/approve"),
            approval_actions(&[approval_row("apl-01JA", "REQUESTED")])
                .into_iter()
                .flat_map(|(_id, pair)| pair.map(|plan| plan.path))
                .collect(),
        ),
        (
            "skill decision",
            "skill",
            args(&["enable", "the-skill", "1.2.0"]),
            ("POST", "/api/v1/skills/the-skill/decision"),
            skill_actions(&SkillActions {
                id: "the-skill".into(),
                version: "1.2.0".into(),
            })
            .expect("the pin is stated")
            .map(|plan| plan.path)
            .to_vec(),
        ),
        (
            "event stream",
            "event",
            args(&["tail"]),
            ("GET", "/api/v1/events/stream"),
            requests_for_view(&View::EventStream)
                .into_iter()
                .map(|r| r.path.clone())
                .collect(),
        ),
        (
            "agent inspection",
            "agent",
            args(&["list"]),
            ("GET", "/api/v1/agents"),
            requests_for_view(&View::Agent)
                .into_iter()
                .map(|r| r.path.clone())
                .collect(),
        ),
        (
            "instance inspection",
            "instance",
            args(&["list"]),
            ("GET", "/api/v1/instances"),
            requests_for_view(&View::Agent)
                .into_iter()
                .map(|r| r.path.clone())
                .collect(),
        ),
    ];

    for (name, command, verb_args, expected, studio_paths) in flows {
        let outcome = if command == "skill" {
            athena_cli::skill::run_skill(&verb_args, Path::new("."))
        } else {
            athena_cli::commands::run_command(command, &verb_args, Path::new("."))
        };
        assert_eq!(outcome.code, 0, "{name}: CLI {command} plans cleanly");
        assert!(
            outcome.message.contains(expected.0) && outcome.message.contains(expected.1),
            "{name}: CLI plans {} {} but rendered: {}",
            expected.0,
            expected.1,
            outcome.message
        );
        assert!(
            studio_paths.contains(&expected.1.to_string()),
            "{name}: the Studio carries the same face {} — got {studio_paths:?}",
            expected.1
        );
        assert_published(expected.1, name);
    }

    // The pin-less skill verb still refuses with exit 2 (the CLI's own face).
    let outcome =
        athena_cli::skill::run_skill(&["enable".into(), "the-skill".into()], Path::new("."));
    assert_eq!(outcome.code, 2);
}
