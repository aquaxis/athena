//! The `athena` binary — the library [`athena_cli::run`] owns the behaviour; this thin
//! wrapper only prints and maps the outcome to a process exit code (ATH-CLI-008).

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    // `-h/--help` are spelled help in the summary.
    args.retain(|a| !(a == "-h" || a == "--help"));
    let outcome = athena_cli::run(&args, &cwd);
    if outcome.code == 0 {
        println!("{}", outcome.message);
    } else {
        eprintln!("{}", outcome.message);
    }
    ExitCode::from(u8::try_from(outcome.code).unwrap_or(70))
}
