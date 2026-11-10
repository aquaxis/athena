//! The §24.3 commands: the arg parse, the request each verb makes, and — since X-27 —
//! the request it actually performs.
//!
//! Two faces, one source of truth:
//!
//! - [`run_command`] renders the **planned** request (`planned: POST /api/v1/tasks …`).
//!   It touches no network and no store, which is what makes the verb→endpoint mapping
//!   assertable in a unit test, and it stays the face `--dry-run` reports.
//! - [`execute`] performs that same plan against the resolved Core API
//!   ([`crate::endpoint`], [`crate::http`]) and renders the answer. This is the face the
//!   `athena` binary dispatches to (`R-IMPL-03`: a verb that only prints its plan is the
//!   pre-implementation state, not the implemented one).
//!
//! `--json` (`ATH-CLI-007`) carries the structured payload on both faces.
//!
//! The fixed verbs (spec §24.3's table):
//!
//! ```text
//! athena run <instruction>          — POST /api/v1/tasks       (auto_run: true)
//! athena status                     — GET  /api/v1/status
//! athena task list                  — GET  /api/v1/tasks
//! athena task show <task-id>        — GET  /api/v1/tasks/{id}
//! athena task create                 — POST /api/v1/tasks        (auto_run: false)
//! athena task cancel <task-id>      — POST /api/v1/tasks/{id}/cancel
//! athena plan show <task-id>        — GET  /api/v1/tasks/{id}/plan
//! athena agent list | show          — GET  /api/v1/agents[/{id}]
//! athena instance list | stop       — GET  /api/v1/instances | POST …/{id}/stop (admin)
//! athena runtime list               — GET  /api/v1/runtimes
//! athena event tail                 — GET  /api/v1/events/stream
//! athena approval list|approve|reject — GET / …/approvals[/{id}/{verb}]
//! ```

use crate::Outcome;
use crate::endpoint::Endpoint;
use crate::exit_code::ExitCode;
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

/// Parse one §24.3 verb into the request it makes (`args` begins after the verb name).
///
/// `cwd` supplies the `workspace` a `run` creates its Task in (`ATH-DATA-009`: the
/// workspace is an absolute path, so it is the invocation directory, never a relative
/// spelling).
///
/// # Errors
/// The `Outcome` of a usage fault (exit 2, `ATH-CLI-008`) when the arguments cannot form a
/// request — a missing instruction, a missing identifier, an unknown sub-verb.
pub fn plan_for(command: &str, args: &[String], cwd: &Path) -> Result<Plan, Outcome> {
    match command {
        "run" => match args.first() {
            Some(instruction) if !instruction.trim().is_empty() => {
                // The body is built here rather than pasted as a placeholder, so the
                // instruction and the workspace are the caller's own values.
                let workspace = cwd.display().to_string();
                Ok(Plan {
                    method: "POST",
                    path: "/api/v1/tasks".into(),
                    body_json: Some(
                        serde_json::json!({
                            "instruction": instruction,
                            "workspace": workspace,
                            "auto_run": true,
                        })
                        .to_string(),
                    ),
                })
            }
            _ => Err(usage("usage: athena run <instruction>")),
        },
        "status" => Ok(Plan {
            method: "GET",
            path: "/api/v1/status".into(),
            body_json: None,
        }),
        "task" => task_plan(args, cwd),
        "plan" => match args.first().map(String::as_str) {
            Some("show") => match args.get(1) {
                Some(task) => Ok(Plan {
                    method: "GET",
                    path: format!("/api/v1/tasks/{task}/plan"),
                    body_json: None,
                }),
                _ => Err(usage("usage: athena plan show <task-id>")),
            },
            _ => Err(usage("usage: athena plan show <task-id>")),
        },
        "agent" => match args.first().map(String::as_str) {
            Some("list") => Ok(Plan {
                method: "GET",
                path: "/api/v1/agents".into(),
                body_json: None,
            }),
            Some("show") => match args.get(1) {
                Some(agent) => Ok(Plan {
                    method: "GET",
                    path: format!("/api/v1/agents/{agent}"),
                    body_json: None,
                }),
                _ => Err(usage("usage: athena agent show <agent>")),
            },
            _ => Err(usage("usage: athena agent <list|show> …")),
        },
        "instance" => match args.first().map(String::as_str) {
            Some("list") => Ok(Plan {
                method: "GET",
                path: "/api/v1/instances".into(),
                body_json: None,
            }),
            Some("stop") => match args.get(1) {
                Some(instance) => Ok(Plan {
                    method: "POST",
                    path: format!("/api/v1/instances/{instance}/stop"),
                    body_json: None,
                }),
                _ => Err(usage("usage: athena instance stop <id> (admin)")),
            },
            _ => Err(usage("usage: athena instance <list|stop> …")),
        },
        "runtime" => match args.first().map(String::as_str) {
            Some("list") => Ok(Plan {
                method: "GET",
                path: "/api/v1/runtimes".into(),
                body_json: None,
            }),
            _ => Err(usage("usage: athena runtime list")),
        },
        "event" => match args.first().map(String::as_str) {
            Some("tail") => Ok(Plan {
                method: "GET",
                path: "/api/v1/events/stream".into(),
                body_json: None,
            }),
            _ => Err(usage("usage: athena event tail")),
        },
        "approval" => approval_plan(args),
        _ => Err(usage(
            "usage: athena <run|status|task|plan|agent|instance|runtime|event|approval> …",
        )),
    }
}

fn task_plan(args: &[String], cwd: &Path) -> Result<Plan, Outcome> {
    match args.first().map(String::as_str) {
        Some("list") => Ok(Plan {
            method: "GET",
            path: "/api/v1/tasks".into(),
            body_json: None,
        }),
        Some("show") => match args.get(1) {
            Some(task) => Ok(Plan {
                method: "GET",
                path: format!("/api/v1/tasks/{task}"),
                body_json: None,
            }),
            _ => Err(usage("usage: athena task show <task-id>")),
        },
        Some("create") => match args.get(1) {
            // The spec's create face: the instruction is the caller's own text, the
            // workspace the invocation directory (`ATH-DATA-009`); unlike `run`, the
            // Task is created without queueing (`auto_run: false`) — §24.3's `run`
            // is the queueing face, `task create` the bare registration.
            Some(instruction) if !instruction.trim().is_empty() => Ok(Plan {
                method: "POST",
                path: "/api/v1/tasks".into(),
                body_json: Some(
                    serde_json::json!({
                        "instruction": instruction,
                        "workspace": cwd.display().to_string(),
                        "auto_run": false,
                    })
                    .to_string(),
                ),
            }),
            _ => Err(usage("usage: athena task create <instruction>")),
        },
        Some("cancel") => match args.get(1) {
            Some(task) => Ok(Plan {
                method: "POST",
                path: format!("/api/v1/tasks/{task}/cancel"),
                body_json: None,
            }),
            _ => Err(usage("usage: athena task cancel <task-id>")),
        },
        _ => Err(usage("usage: athena task <create|list|show|cancel> …")),
    }
}

fn approval_plan(args: &[String]) -> Result<Plan, Outcome> {
    match args.first().map(String::as_str) {
        Some("list") => Ok(Plan {
            method: "GET",
            path: "/api/v1/approvals".into(),
            body_json: None,
        }),
        Some(verb @ ("approve" | "reject")) => match args.get(1) {
            Some(id) => Ok(Plan {
                method: "POST",
                path: format!("/api/v1/approvals/{id}/{verb}"),
                body_json: None,
            }),
            _ => Err(usage("usage: athena approval <approve|reject> <id>")),
        },
        _ => Err(usage("usage: athena approval <list|approve|reject> …")),
    }
}

/// Run one §24.3 verb as a **plan** — the request rendered, nothing performed.
///
/// This is the deterministic face the verb→endpoint tests assert and `--dry-run` reports;
/// the live face is [`execute`].
#[must_use]
pub fn run_command(command: &str, args: &[String], cwd: &Path) -> Outcome {
    let json_wanted = args.iter().any(|arg| arg == "--json");
    let args = strip_flags(args);
    match plan_for(command, &args, cwd) {
        Ok(plan) => render_plan(&plan, json_wanted),
        Err(outcome) => outcome,
    }
}

/// Run one §24.3 verb against the live Core API: perform the request, render the answer.
///
/// `token` is the `--token` override; otherwise the resolution consults `ATHENA_TOKEN`.
/// A non-2xx answer maps its `class` field onto the CLI's exit codes (`ATH-CLI-008`), so a
/// refusal is distinguishable from a success by the shell as well as by eye.
#[must_use]
pub fn execute(command: &str, args: &[String], cwd: &Path, token: Option<&str>) -> Outcome {
    let json_wanted = args.iter().any(|arg| arg == "--json");
    let args = strip_flags(args);
    let plan = match plan_for(command, &args, cwd) {
        Ok(plan) => plan,
        Err(outcome) => return outcome,
    };
    let Some(home) = crate::resolve::resolve_home(cwd) else {
        // The address lives in the project's configuration, so without a project there is
        // nothing to talk to (`ATH-CLI-010`).
        return Outcome::fail(
            2,
            format!(
                "no .athena/ resolved from {}; run `athena init` to introduce this \
                 project first (ATH-CLI-010)",
                cwd.display()
            ),
        );
    };
    let endpoint = match crate::endpoint::resolve(&home, token) {
        Ok(endpoint) => endpoint,
        Err(fault) => return Outcome::fail(2, format!("athena {command}: {fault}")),
    };
    perform(&endpoint, &plan, json_wanted)
}

/// Send an already-planned request against the resolved Core API (the transport face
/// `skill::run_skill` rides after its own parse — one plan, one source of truth).
pub fn execute_plan(endpoint: &Endpoint, plan: &Plan, json_wanted: bool) -> Outcome {
    perform(endpoint, plan, json_wanted)
}

/// Send the plan and render what came back.
fn perform(endpoint: &Endpoint, plan: &Plan, json_wanted: bool) -> Outcome {
    let response =
        match crate::http::request(endpoint, plan.method, &plan.path, plan.body_json.as_deref()) {
            Ok(response) => response,
            Err(fault) => {
                return Outcome::fail(
                    ExitCode::InternalError.as_i32(),
                    format!("athena: the Core API is unreachable: {fault}"),
                );
            }
        };

    // The API answers the §22.2 envelope on a refusal; its `class` picks the exit code
    // (`ATH-CLI-008`), so a shell script can branch on the outcome without parsing text.
    if !response.is_success() {
        let class = response
            .json()
            .as_ref()
            .and_then(|value| value.get("error"))
            .and_then(|error| error.get("class"))
            .and_then(serde_json::Value::as_str)
            .and_then(crate::exit_code::ErrorClass::parse);
        let code = class
            .map_or(ExitCode::ExecutionFailure, |class| {
                ExitCode::from_error_class(&class)
            })
            .as_i32();
        return Outcome::fail(
            code,
            format!(
                "athena {}: {} — {}",
                plan.method,
                response.summary(),
                response.body.trim()
            ),
        );
    }

    if json_wanted {
        // The API's own JSON body is the payload — no reshaping, so `--json` stays a
        // faithful projection of what Core answered.
        return Outcome::ok_json(response.body.clone(), response.body.clone());
    }
    Outcome::ok(render_body(&response.body))
}

/// Render a successful body for a terminal: JSON pretty-printed (it is what every Core
/// endpoint answers), anything else verbatim.
fn render_body(body: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|_| body.to_string()),
        Err(_) => body.trim().to_string(),
    }
}

/// Drop the CLI's own flags from a verb's argument list.
pub(crate) fn strip_flags(args: &[String]) -> Vec<String> {
    args.iter()
        .filter(|arg| {
            !matches!(arg.as_str(), "--json" | "--dry-run") && !arg.starts_with("--answer=")
        })
        .cloned()
        .collect()
}

/// A usage fault: exit 2, the request-shape code (`ATH-CLI-008`); the §24.3 verbs'
/// parse faults share this face (`skill::skill_plan` includes).
pub(crate) fn usage(message: &str) -> Outcome {
    Outcome::fail(ExitCode::RequestShapeViolation.as_i32(), message)
}

/// Render a plan without performing it (the `--dry-run` face; `skill::run_skill` rides
/// the same deterministic face).
pub(crate) fn render_plan(plan: &Plan, json_wanted: bool) -> Outcome {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn a_run_plan_carries_the_instruction_and_the_invocation_directory() {
        let plan = plan_for("run", &args(&["fix the build"]), Path::new("/tmp/project"))
            .expect("a well-formed run");
        assert_eq!(plan.method, "POST");
        assert_eq!(plan.path, "/api/v1/tasks");
        let body: serde_json::Value =
            serde_json::from_str(plan.body_json.as_deref().expect("body")).expect("json");
        assert_eq!(body["instruction"], "fix the build");
        assert_eq!(body["workspace"], "/tmp/project");
        assert_eq!(body["auto_run"], true);
    }

    #[test]
    fn a_task_create_plan_carries_the_bare_registration_body() {
        // §24.3's `task create` (X-32): the caller's own instruction, the invocation
        // directory as the workspace (`ATH-DATA-009`), created without queueing —
        // `run` remains the queueing face.
        let plan = plan_for(
            "task",
            &args(&["create", "ship the release"]),
            Path::new("/tmp/project"),
        )
        .expect("a well-formed create");
        assert_eq!(plan.method, "POST");
        assert_eq!(plan.path, "/api/v1/tasks");
        let body: serde_json::Value =
            serde_json::from_str(plan.body_json.as_deref().expect("body")).expect("json");
        assert_eq!(body["instruction"], "ship the release");
        assert_eq!(body["workspace"], "/tmp/project");
        assert_eq!(body["auto_run"], false);
        // The instruction-less create is a usage fault (exit 2):
        let outcome = plan_for("task", &args(&["create"]), Path::new("/tmp/project"))
            .expect_err("a shape fault");
        assert_eq!(outcome.code, 2);
    }

    #[test]
    fn every_verb_keeps_its_endpoint() {
        let rows: [(&str, &[&str], &str, &str); 11] = [
            ("status", &[], "GET", "/api/v1/status"),
            ("task", &["list"], "GET", "/api/v1/tasks"),
            ("task", &["show", "task-1"], "GET", "/api/v1/tasks/task-1"),
            (
                "task",
                &["cancel", "task-1"],
                "POST",
                "/api/v1/tasks/task-1/cancel",
            ),
            (
                "plan",
                &["show", "task-1"],
                "GET",
                "/api/v1/tasks/task-1/plan",
            ),
            ("agent", &["list"], "GET", "/api/v1/agents"),
            (
                "agent",
                &["show", "agent-1"],
                "GET",
                "/api/v1/agents/agent-1",
            ),
            ("instance", &["list"], "GET", "/api/v1/instances"),
            ("runtime", &["list"], "GET", "/api/v1/runtimes"),
            ("event", &["tail"], "GET", "/api/v1/events/stream"),
            ("approval", &["list"], "GET", "/api/v1/approvals"),
        ];
        for (command, values, method, path) in rows {
            let plan = plan_for(command, &args(values), Path::new("/tmp/p"))
                .unwrap_or_else(|_| panic!("{command} {values:?} plans"));
            assert_eq!(plan.method, method, "{command} {values:?}");
            assert_eq!(plan.path, path, "{command} {values:?}");
        }
    }

    #[test]
    fn the_approval_verbs_ride_the_decision_endpoints() {
        let plan = plan_for(
            "approval",
            &args(&["approve", "apr-1"]),
            Path::new("/tmp/p"),
        )
        .expect("approve plans");
        assert_eq!(plan.method, "POST");
        assert_eq!(plan.path, "/api/v1/approvals/apr-1/approve");
        let plan = plan_for("approval", &args(&["reject", "apr-1"]), Path::new("/tmp/p"))
            .expect("reject plans");
        assert_eq!(plan.path, "/api/v1/approvals/apr-1/reject");
    }

    #[test]
    fn an_empty_instruction_or_a_missing_identifier_is_a_shape_fault() {
        for (command, values) in [
            ("run", args(&[])),
            ("run", args(&["   "])),
            ("task", args(&["show"])),
            ("task", args(&["cancel"])),
            ("task", args(&["create"])),
            ("task", args(&["create", "   "])),
            ("plan", args(&["show"])),
            ("agent", args(&["show"])),
            ("instance", args(&["stop"])),
            ("approval", args(&["approve"])),
        ] {
            let outcome =
                plan_for(command, &values, Path::new("/tmp/p")).expect_err("a shape fault");
            assert_eq!(outcome.code, 2, "{command} {values:?}");
        }
    }

    #[test]
    fn a_successful_body_renders_pretty_json_and_anything_else_verbatim() {
        assert_eq!(
            render_body(r#"{"status":"ok"}"#),
            "{\n  \"status\": \"ok\"\n}"
        );
        assert_eq!(render_body("  plain text \n"), "plain text");
    }

    #[test]
    fn the_json_face_carries_the_plan_without_performing_it() {
        let outcome = run_command("status", &args(&["--json"]), Path::new("/tmp/p"));
        assert_eq!(outcome.code, 0);
        let data = outcome.data_json.expect("--json");
        assert!(data.contains(r#""method":"GET""#), "{data}");
        assert!(data.contains(r#""path":"/api/v1/status""#), "{data}");
    }
}
