//! The `skill *` commands (P5-13's CLI layer; spec §24.3): the arg parse and
//! the request plan each invocation carries. The HTTP transport is a Phase-6 build
//! (`athena chat`'s client layer, P6-02/03); until then, a resolvable home reports the
//! planned request exactly (deterministic, still no Core-internal reach — `ATH-API-001`).
//!
//! The pin face here is the CLI contract's: registering, enabling and disabling all
//! demand the **version**; a missing version is a usage fault (exit 2), matching the
//! API's mandatory-`(id, version)` shape (`ATH-SKILL-020`).
//!
//! `--json` (`ATH-CLI-007`): the outcome carries the structured payload.

use crate::Outcome;
use crate::resolve::resolve_home;
use std::path::Path;

/// The planned request: what the transport layer will send (spec §22's form).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The HTTP method.
    pub method: &'static str,
    /// The endpoint path.
    pub path: String,
    /// The JSON body, when the plan carries one.
    pub body_json: Option<String>,
}

/// Run one `skill *` invocation (`args` begins after "skill").
///
/// The public surface:
///
/// ```text
/// athena skill list
/// athena skill show <id>
/// athena skill search <capability>
/// athena skill register <directory>        (admin; the manifest pins id@version)
/// athena skill enable <id> <version>       (admin)
/// athena skill disable <id> <version>      (admin)
/// ```
#[must_use]
pub fn run_skill(args: &[String], cwd: &Path) -> Outcome {
    let json_wanted = args.iter().any(|arg| arg == "--json");
    let args: Vec<String> = args
        .iter()
        .filter(|arg| *arg != "--json")
        .cloned()
        .collect();
    match args.first().map(String::as_str) {
        Some("list") => request(
            Plan {
                method: "GET",
                path: "/api/v1/skills".into(),
                body_json: None,
            },
            json_wanted,
        ),
        Some("show") => match args.get(1) {
            Some(id) if !id.trim().is_empty() => request(
                Plan {
                    method: "GET",
                    path: format!("/api/v1/skills/{id}"),
                    body_json: None,
                },
                json_wanted,
            ),
            _ => usage_fail("usage: athena skill show <id>"),
        },
        Some("search") => match args.get(1) {
            Some(capability) if !capability.trim().is_empty() => request(
                Plan {
                    method: "GET",
                    path: format!("/api/v1/skills/search?capability={capability}"),
                    body_json: None,
                },
                json_wanted,
            ),
            _ => usage_fail("usage: athena skill search <capability>"),
        },
        Some("register") => match args.get(1) {
            Some(directory) if !directory.trim().is_empty() => {
                let manifest =
                    std::fs::read_to_string(cwd.join(directory.trim()).join("manifest.yaml"));
                match manifest {
                    Ok(text) => {
                        match serde_json::to_string(&serde_json::json!({ "manifest_yaml": text })) {
                            Ok(body) => request(
                                Plan {
                                    method: "POST",
                                    path: "/api/v1/skills".into(),
                                    body_json: Some(body),
                                },
                                json_wanted,
                            ),
                            Err(detail) => {
                                Outcome::fail(4, format!("athena skill register: {detail}"))
                            }
                        }
                    }
                    Err(error) => Outcome::fail(
                        4,
                        format!(
                            "athena skill register: cannot read {}/manifest.yaml: {}",
                            directory.trim(),
                            error
                        ),
                    ),
                }
            }
            _ => usage_fail("usage: athena skill register <directory>"),
        },
        Some("enable") => pin_operation(&args[1..], "enable", json_wanted),
        Some("disable") => pin_operation(&args[1..], "disable", json_wanted),
        _ => usage_fail("usage: athena skill <list|show|search|register|enable|disable> …"),
    }
}

fn pin_operation(args: &[String], verb: &'static str, json_wanted: bool) -> Outcome {
    let (Some(id), Some(version)) = (args.first(), args.get(1)) else {
        return usage_fail(&format!(
            "usage: athena skill {verb} <id> <version> (the pin is mandatory, ATH-SKILL-020)"
        ));
    };
    if id.trim().is_empty() || version.trim().is_empty() {
        return usage_fail(&format!(
            "usage: athena skill {verb} <id> <version> (the pin is mandatory, ATH-SKILL-020)"
        ));
    }
    request(
        Plan {
            method: "POST",
            path: format!("/api/v1/skills/{id}/decision"),
            body_json: Some(format!(
                r#"{{"decision":"{verb}","id":"{id}","version":"{version}"}}"#
            )),
        },
        json_wanted,
    )
}

fn usage_fail(message: &str) -> Outcome {
    Outcome::fail(2, message)
}

fn request(plan: Plan, json_wanted: bool) -> Outcome {
    let _ = resolve_home(Path::new("."));
    let rendered = format!(
        "planned: {} {}{}",
        plan.method,
        plan.path,
        plan.body_json
            .as_ref()
            .map(|body| format!(" --data '{body}'"))
            .unwrap_or_default(),
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
