//! The `athena` command-line interface (spec section 24).
//!
//! v1 scope in this crate: `athena init` (tasks P3-07/P3-08). The dispatch contracts that
//! the specification fixes together with it are implemented around the init path:
//!
//! - `.athena/` is resolved by an upward search from the current directory and never derived
//!   from the executable or a configuration location (ATH-CFG-010).
//! - Every non-init command fails with exit code 2 and prompts for `athena init` when
//!   `.athena/` cannot be resolved (ATH-CLI-010). Commands other than `init` are a Phase 6
//!   build; until then a resolvable home reports the command as not implemented (exit 70,
//!   ATH-CLI-008's internal-error code).

pub mod chat;
pub mod commands;
pub mod endpoint;
pub mod exit_code;
pub mod http;
pub mod init;
pub mod resolve;
pub mod skill;
/// Embedded template content (public only for the conformance drift test in
/// `athena-conformance` — a test dependency, never a Core/UI edge; the section-32.2
/// boundary check excludes the `tests/` package by design).
pub mod template;
pub mod token;
pub mod update;

use std::path::Path;

use crate::commands::strip_flags;

/// The outcome of one CLI invocation: process exit code plus the message the bin prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Process exit code (ATH-CLI-008).
    pub code: i32,
    /// The message printed by the bin (stdout for success, stderr for failures).
    pub message: String,
    /// The `--json` face (`ATH-CLI-007`): the structured payload, when the invocation
    /// carries `--json` and the verb has one.
    pub data_json: Option<String>,
}

impl Outcome {
    fn ok(message: impl Into<String>) -> Self {
        Self {
            code: 0,
            message: message.into(),
            data_json: None,
        }
    }

    fn ok_json(message: impl Into<String>, data_json: impl Into<String>) -> Self {
        Self {
            code: 0,
            message: message.into(),
            data_json: Some(data_json.into()),
        }
    }

    fn fail(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data_json: None,
        }
    }
}

/// Run one invocation with `args` (without the program name) and `cwd` as the working
/// directory. No process state is touched here; `main.rs` owns printing and exiting.
#[must_use]
pub fn run(args: &[String], cwd: &Path) -> Outcome {
    match args.first().map(String::as_str) {
        Some("init") => match init::run_init(cwd) {
            Ok(report) => Outcome::ok(report.summary()),
            Err(fault) => Outcome::fail(4, format!("athena init failed: {fault}")),
        },
        Some("help") | None => Outcome::ok(USAGE),
        // The update verb is a *tool-side* operation (R-OPS-11/R-CLI-08, design §2.9/D-21):
        // installer-equivalent tool refresh always; a project-side refresh of exactly
        // `personas/` and `skills/` when `.athena/` resolves. It is not a Core-API client face
        // and not subject to the resolvable-home prompt (D-21) — a non-project cwd still gets
        // the tool refresh.
        Some("update") => {
            let json_wanted = args.iter().any(|arg| arg == "--json");
            match update::run_update(cwd) {
                Ok(report) => update::update_outcome(&report, json_wanted),
                Err(fault) => Outcome::fail(4, format!("athena update failed: {fault}")),
            }
        }
        Some("skill") => skill::run_skill(&args[1..], cwd),
        // The token verb issues the Bearer credentials the API authenticates (R-SEC-03 /
        // R-SEC-04). It is a *tool-side* face writing the registry under `<ATHENA_HOME>/var/`,
        // so it needs a resolved home but no resident API — the same standing as `athena
        // update`'s project face.
        Some("token") => {
            let json_wanted = args.iter().any(|arg| arg == "--json");
            run_token(&args[1..], cwd, json_wanted)
        }
        Some(command)
            if matches!(
                command,
                "run"
                    | "status"
                    | "task"
                    | "plan"
                    | "agent"
                    | "instance"
                    | "runtime"
                    | "event"
                    | "approval"
            ) =>
        {
            // The verbs *perform* their requests (X-27): a verb that only printed
            // `planned: …` was the pre-implementation state, not the implemented one
            // (`R-IMPL-03`). `--dry-run` keeps the plan face for inspection and for the
            // endpoint-shape tests; `--token` supplies the bearer when the project
            // authenticates with tokens (`ATH-SEC-004`).
            let json_wanted = args.iter().any(|arg| arg == "--json");
            let _ = json_wanted;
            if args.iter().any(|arg| arg == "--dry-run") {
                commands::run_command(command, &args[1..], cwd)
            } else {
                let token = token_override(&args[1..]);
                commands::execute(command, &args[1..], cwd, token.as_deref())
            }
        }
        Some("chat") => {
            // The dialogue loop (X-27): one instruction, the Task driven to a terminal
            // state, the pending records presented and answered inline. `--token`
            // supplies the bearer like the other Core-API verbs (ATH-SEC-004).
            let words = &args[1..];
            let instruction = strip_flags(words).join(" ").trim().to_string();
            if instruction.is_empty() {
                return Outcome::fail(2, "usage: athena chat <instruction>");
            }
            let Some(home) = resolve::resolve_home(cwd) else {
                return Outcome::fail(
                    2,
                    format!(
                        "no .athena/ resolved from {}; run `athena init` to introduce this \
                         project first (ATH-CLI-010)",
                        cwd.display()
                    ),
                );
            };
            let token = token_override(words);
            let endpoint = match crate::endpoint::resolve(&home, token.as_deref()) {
                Ok(endpoint) => endpoint,
                Err(fault) => return Outcome::fail(2, format!("athena chat: {fault}")),
            };
            let stdin_canned: Vec<String> = words
                .iter()
                .filter(|w| w.starts_with("--answer"))
                .map(|w| w.trim_start_matches("--answer=").to_string())
                .collect();
            let mut slot = stdin_canned.into_iter();
            let mut answer_for = |_prompt: &chat::session::PendingPrompt| -> String {
                slot.next().unwrap_or_default()
            };
            match chat::session::run_session(&endpoint, &instruction, &mut answer_for) {
                Ok(transcript) => Outcome {
                    code: transcript.code,
                    message: chat::session::rendered(&transcript.lines),
                    data_json: None,
                },
                Err(fault) => Outcome::fail(70, format!("athena chat: {fault}")),
            }
        }
        Some(command) => match resolve::resolve_home(cwd) {
            Some(home) => Outcome::fail(
                70,
                format!(
                    "athena {command} is not implemented yet (Phase 6). ATHENA_HOME: {}",
                    home.display()
                ),
            ),
            None => Outcome::fail(
                2,
                format!(
                    "no .athena/ resolved from {}; run `athena init` to introduce this \
                         project first (ATH-CLI-010)",
                    cwd.display()
                ),
            ),
        },
    }
}

/// The `--token <value>` override of a verb's argument list, when present.
///
/// The secret is never stored anywhere the CLI reads back (the registry holds hashes,
/// `ATH-SEC-005`), so the holder passes it per invocation — through `ATHENA_TOKEN` or
/// this flag. The flag wins.
fn token_override(args: &[String]) -> Option<String> {
    let mut words = args.iter();
    while let Some(word) = words.next() {
        if word == "--token" {
            return words.next().cloned();
        }
    }
    None
}

/// `athena token <issue|list>`: the Bearer credentials the API authenticates.
///
/// Issuance prints the plaintext **once** (`ATH-SEC-005`: only the hash is stored) and
/// names the file it landed in, so an operator can hand the token to its holder and
/// forget it. Nothing here talks to a running API — the registry file is the contract
/// both sides read.
#[must_use]
fn run_token(args: &[String], cwd: &Path, json_wanted: bool) -> Outcome {
    let Some(home) = resolve::resolve_home(cwd) else {
        return Outcome::fail(
            2,
            format!(
                "no .athena/ resolved from {}; run `athena init` to introduce this \
                 project first (ATH-CLI-010)",
                cwd.display()
            ),
        );
    };
    let command = args.first().map(String::as_str);
    match command {
        Some("issue") => {
            let mut name: Option<&str> = None;
            let mut role: Option<&str> = None;
            let mut words = args[1..].iter();
            while let Some(word) = words.next() {
                match word.as_str() {
                    "--name" => name = words.next().map(String::as_str),
                    "--role" => role = words.next().map(String::as_str),
                    _ => {}
                }
            }
            match token::run_issue(home.as_path(), name, role) {
                Ok(issued) => {
                    let path = token::registry_path(home.as_path());
                    let message = format!(
                        "token issued for {:?} (role {}, recorded in {}).\n\
                         This is the only time the token is shown; store it now:\n  {}\n",
                        issued.record.name,
                        issued.record.role,
                        path.display(),
                        issued.plaintext
                    );
                    if json_wanted {
                        Outcome::ok_json(
                            message,
                            serde_json::json!({
                                "name": issued.record.name,
                                "role": issued.record.role,
                                "token": issued.plaintext,
                                "registry": path.display().to_string(),
                            })
                            .to_string(),
                        )
                    } else {
                        Outcome::ok(message)
                    }
                }
                Err(fault) => Outcome::fail(2, format!("athena token issue: {fault}")),
            }
        }
        Some("list") => match token::run_list(home.as_path()) {
            Ok(records) => {
                if records.is_empty() {
                    return Outcome::ok("no tokens issued yet");
                }
                let mut message = String::from("issued tokens (names, roles; hashes only):\n");
                for record in &records {
                    message.push_str(&format!(
                        "  {} — {} ({})\n",
                        record.name, record.role, record.sha256_hex
                    ));
                }
                if json_wanted {
                    let rows: Vec<serde_json::Value> = records
                        .iter()
                        .map(|record| {
                            serde_json::json!({
                                "name": record.name,
                                "role": record.role,
                                "sha256_hex": record.sha256_hex,
                            })
                        })
                        .collect();
                    Outcome::ok_json(message, serde_json::json!({ "tokens": rows }).to_string())
                } else {
                    Outcome::ok(message)
                }
            }
            Err(fault) => Outcome::fail(4, format!("athena token list: {fault}")),
        },
        _ => Outcome::fail(
            2,
            "usage: athena token <issue --name <name> --role <viewer|operator|admin> | list>",
        ),
    }
}

/// The command surface summary the bin prints for `athena help` / no arguments.
const USAGE: &str = "athena — organize multiple AI agents to carry out one Task                      (spec 1.0)\n\nusage:\n  athena init      introduce this project: create .athena/ and expand aiprj\n                   (ATH-CLI-009)\n  athena update    refresh the tool from the published repository; when a .athena/\n                   project resolves, update only personas/ and skills/ (R-CLI-08)\n  athena token     issue or list the Bearer tokens the API authenticates\n                   (token issue --name <name> --role <viewer|operator|admin>)\n  athena help      this summary\n\nOther commands (chat, run, task, ...) reach the Core API and arrive in Phase 6; they exit\nwith code 2 and prompt `athena init` while no .athena/ can be resolved (ATH-CLI-010).\n";
