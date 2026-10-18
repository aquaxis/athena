//! The remaining §24.3 commands (P6-03): the arg parse + the request plan each CLI
//! verb carries, over the same deterministic "planned request" face as the `skill`
//! layer (the HTTP transport arrives with the chat client build — P6-02/03).
//!
//! `--json` (`ATH-CLI-007`): the outcome carries the structured payload.
//!
//! The fixed verbs (spec §24.3's table, minus the ones already implemented):
//!
//! ```text
//! athena run <instruction>          — POST /api/v1/tasks       (auto_run: true)
//! athena status                     — GET  /api/v1/status
//! athena task list                  — GET  /api/v1/tasks
//! athena task show <task-id>        — GET  /api/v1/tasks/{id}
//! athena task cancel <task-id>      — POST /api/v1/tasks/{id}/cancel
//! athena plan show <task-id>        — GET  /api/v1/tasks/{id}/plan
//! athena agent list | show          — GET  /api/v1/agents[/{id}]
//! athena instance list | stop       — GET  /api/v1/instances | POST …/{id}/stop (admin)
//! athena runtime list               — GET  /api/v1/runtimes
//! athena event tail                 — GET  /api/v1/events/stream
//! athena approval list|approve|reject — GET / …/approvals[/{id}/{verb}]
//! ```

use crate::Outcome;
use std::path::Path;

/// The planned request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The HTTP method.
    pub method: &'static str,
    /// The endpoint path.
    pub path: String,
    /// The JSON body, when the plan carries one.
    pub body_json: Option<String>,
}

/// Run one §24.3 verb (`args` begins after the verb name).
#[must_use]
pub fn run_command(command: &str, args: &[String], _cwd: &Path) -> Outcome {
    let json_wanted = args.iter().any(|arg| arg == "--json");
    let args: Vec<String> = args
        .iter()
        .filter(|arg| *arg != "--json")
        .cloned()
        .collect();
    match command {
        "run" => match args.first() {
            Some(instruction) if !instruction.trim().is_empty() => plan(
                Plan {
                    method: "POST",
                    path: "/api/v1/tasks".into(),
                    body_json: Some(
                        r#"{"instruction":"INSTRUCTION_TEXT","workspace":"CWD","auto_run":true}"#
                            .to_string(),
                    ),
                },
                json_wanted,
            ),
            _ => usage("usage: athena run <instruction>"),
        },
        "status" => plan(
            Plan {
                method: "GET",
                path: "/api/v1/status".into(),
                body_json: None,
            },
            json_wanted,
        ),
        "task" => run_task_verb(&args, json_wanted),
        "plan" => match args.first().map(String::as_str) {
            Some("show") => match args.get(1) {
                Some(task) => plan(
                    Plan {
                        method: "GET",
                        path: format!("/api/v1/tasks/{task}/plan"),
                        body_json: None,
                    },
                    json_wanted,
                ),
                _ => usage("usage: athena plan show <task-id>"),
            },
            _ => usage("usage: athena plan show <task-id>"),
        },
        "agent" => run_agent_verb(&args, json_wanted),
        "instance" => run_instance_verb(&args, json_wanted),
        "runtime" => match args.first().map(String::as_str) {
            Some("list") => plan(
                Plan {
                    method: "GET",
                    path: "/api/v1/runtimes".into(),
                    body_json: None,
                },
                json_wanted,
            ),
            _ => usage("usage: athena runtime list"),
        },
        "event" => match args.first().map(String::as_str) {
            Some("tail") => plan(
                Plan {
                    method: "GET",
                    path: "/api/v1/events/stream".into(),
                    body_json: None,
                },
                json_wanted,
            ),
            _ => usage("usage: athena event tail"),
        },
        "approval" => run_approval_verb(&args, json_wanted),
        _ => usage("usage: athena <run|status|task|plan|agent|instance|runtime|event|approval> …"),
    }
}

fn run_task_verb(args: &[String], json_wanted: bool) -> Outcome {
    match args.first().map(String::as_str) {
        Some("list") => plan(
            Plan {
                method: "GET",
                path: "/api/v1/tasks".into(),
                body_json: None,
            },
            json_wanted,
        ),
        Some("show") => match args.get(1) {
            Some(task) => plan(
                Plan {
                    method: "GET",
                    path: format!("/api/v1/tasks/{task}"),
                    body_json: None,
                },
                json_wanted,
            ),
            _ => usage("usage: athena task show <task-id>"),
        },
        Some("cancel") => match args.get(1) {
            Some(task) => plan(
                Plan {
                    method: "POST",
                    path: format!("/api/v1/tasks/{task}/cancel"),
                    body_json: None,
                },
                json_wanted,
            ),
            _ => usage("usage: athena task cancel <task-id>"),
        },
        _ => usage("usage: athena task <list|show|cancel> …"),
    }
}

fn run_agent_verb(args: &[String], json_wanted: bool) -> Outcome {
    match args.first().map(String::as_str) {
        Some("list") => plan(
            Plan {
                method: "GET",
                path: "/api/v1/agents".into(),
                body_json: None,
            },
            json_wanted,
        ),
        Some("show") => match args.get(1) {
            Some(agent) => plan(
                Plan {
                    method: "GET",
                    path: format!("/api/v1/agents/{agent}"),
                    body_json: None,
                },
                json_wanted,
            ),
            _ => usage("usage: athena agent show <agent>"),
        },
        _ => usage("usage: athena agent <list|show> …"),
    }
}

fn run_instance_verb(args: &[String], json_wanted: bool) -> Outcome {
    match args.first().map(String::as_str) {
        Some("list") => plan(
            Plan {
                method: "GET",
                path: "/api/v1/instances".into(),
                body_json: None,
            },
            json_wanted,
        ),
        Some("stop") => match args.get(1) {
            Some(instance) => plan(
                Plan {
                    method: "POST",
                    path: format!("/api/v1/instances/{instance}/stop"),
                    body_json: None,
                },
                json_wanted,
            ),
            _ => usage("usage: athena instance stop <id> (admin)"),
        },
        _ => usage("usage: athena instance <list|stop> …"),
    }
}

fn run_approval_verb(args: &[String], json_wanted: bool) -> Outcome {
    match args.first().map(String::as_str) {
        Some("list") => plan(
            Plan {
                method: "GET",
                path: "/api/v1/approvals".into(),
                body_json: None,
            },
            json_wanted,
        ),
        Some("approve") | Some("reject") => match args.get(1) {
            Some(id) => plan(
                Plan {
                    method: "POST",
                    path: format!("/api/v1/approvals/{id}/{}", args.first().unwrap()),
                    body_json: None,
                },
                json_wanted,
            ),
            _ => usage("usage: athena approval <approve|reject> <id>"),
        },
        _ => usage("usage: athena approval <list|approve|reject> …"),
    }
}

fn usage(message: &str) -> Outcome {
    Outcome::fail(2, message)
}

fn plan(plan: Plan, json_wanted: bool) -> Outcome {
    let rendered = format!(
        "planned: {} {}{}",
        plan.method,
        plan.path,
        plan.body_json
            .as_ref()
            .map(|body| format!(" --data '{body}'"))
            .unwrap_or_default()
    );
    if json_wanted {
        Outcome::ok_json(
            rendered,
            format!(
                r#"{{"method":"{}","path":"{}","body":{}}}"#,
                plan.method,
                plan.path,
                plan.body_json.as_deref().unwrap_or("null")
            ),
        )
    } else {
        Outcome::ok(rendered)
    }
}
