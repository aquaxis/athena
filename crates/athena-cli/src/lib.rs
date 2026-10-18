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
pub mod exit_code;
pub mod init;
pub mod resolve;
pub mod skill;
/// Embedded template content (public only for the conformance drift test in
/// `athena-conformance` — a test dependency, never a Core/UI edge; the section-32.2
/// boundary check excludes the `tests/` package by design).
pub mod template;

use std::path::Path;

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
        Some("skill") => skill::run_skill(&args[1..], cwd),
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
            commands::run_command(command, &args[1..], cwd)
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

/// The command surface summary the bin prints for `athena help` / no arguments.
const USAGE: &str = "athena — organize multiple AI agents to carry out one Task                      (spec 1.0)\n\nusage:\n  athena init      introduce this project: create .athena/ and expand aiprj\n                   (ATH-CLI-009)\n  athena help      this summary\n\nOther commands (chat, run, task, ...) reach the Core API and arrive in Phase 6; they exit\nwith code 2 and prompt `athena init` while no .athena/ can be resolved (ATH-CLI-010).\n";
