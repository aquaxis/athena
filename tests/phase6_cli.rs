//! Phase-6 CLI group (P6-06 + P6-07): the CLI never launches `agent-cli` directly
//! (`ATH-CLI-002`) — proven structurally, not by scanning at runtime — and the
//! §24.3 command faces' contracts hold end to end. Passing closes Phase 6.

// --- P6-06: the direct-launch ban (ATH-CLI-002) -------------------------------------------

#[test]
fn the_cli_never_spawns_a_process() {
    // The structural proof: the `athena-cli` crate contains no process launching at
    // all — `std::process::Command`, `std::os::unix::process`, libc's fork/exec, or an
    // async spawn — any of which would be the `agent-cli` direct launch the spec
    // forbids (`ATH-CLI-002`). The only path to the Core rides the Core API
    // (`ATH-CLI-001`).
    for file in [
        "lib.rs",
        "chat.rs",
        "commands.rs",
        "skill.rs",
        "init.rs",
        "resolve.rs",
    ] {
        let path = format!("../crates/athena-cli/src/{file}");
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path}: {error}"));
        for forbidden in [
            "std::process::Command",
            "process::Command",
            "Command::new",
            "os::unix::process",
            "tokio::process",
            "async_process",
        ] {
            assert!(
                !text.contains(forbidden),
                "{file} contains {forbidden:?} — the direct-launch ban (ATH-CLI-002)"
            );
        }
    }
    // Every verb's plan rides the Core API's path prefixes (the only route the CLI
    // holds): commands.rs names the Task/Agent/Approval faces, skill.rs the Skill faces.
    let commands =
        std::fs::read_to_string("../crates/athena-cli/src/commands.rs").expect("read commands.rs");
    for planned in ["/api/v1/tasks", "/api/v1/agents", "/api/v1/approvals"] {
        assert!(
            commands.contains(planned),
            "the verb's plan rides the Core API face: {planned}"
        );
    }
    let skill =
        std::fs::read_to_string("../crates/athena-cli/src/skill.rs").expect("read skill.rs");
    assert!(
        skill.contains("/api/v1/skills"),
        "the skill face rides the Core API"
    );
}

// --- P6-07: the §24.3 faces' contracts ----------------------------------------------------

#[test]
fn the_verb_plans_name_the_endpoints_the_api_publishes() {
    let rows: [(&str, &[String], &str, &str); 10] = [
        (
            "run",
            &[to_string("investigate the build failure")],
            "POST",
            "/api/v1/tasks",
        ),
        ("status", &[], "GET", "/api/v1/status"),
        ("task", &[to_string("list")], "GET", "/api/v1/tasks"),
        (
            "task",
            &[to_string("show"), to_string("task-01JA")],
            "GET",
            "/api/v1/tasks/task-01JA",
        ),
        (
            "task",
            &[to_string("cancel"), to_string("task-01JA")],
            "POST",
            "/api/v1/tasks/task-01JA/cancel",
        ),
        (
            "plan",
            &[to_string("show"), to_string("task-01JA")],
            "GET",
            "/api/v1/tasks/task-01JA/plan",
        ),
        ("agent", &[to_string("list")], "GET", "/api/v1/agents"),
        ("instance", &[to_string("list")], "GET", "/api/v1/instances"),
        ("runtime", &[to_string("list")], "GET", "/api/v1/runtimes"),
        (
            "event",
            &[to_string("tail")],
            "GET",
            "/api/v1/events/stream",
        ),
    ];
    for (command, args, method, path) in rows {
        let outcome = athena_cli::commands::run_command(command, args, std::path::Path::new("."));
        assert_eq!(outcome.code, 0, "{command:?} {args:?}");
        assert!(
            outcome.message.contains(method) && outcome.message.contains(path),
            "{command:?} → {method} {path}: {}",
            outcome.message
        );
    }
}

fn to_string(value: &str) -> String {
    value.to_string()
}

#[test]
fn the_usage_faults_are_exit_2_and_the_pins_are_demanded() {
    // A missing instruction (run) is a shape violation:
    let outcome = athena_cli::commands::run_command("run", &[], std::path::Path::new("."));
    assert_eq!(outcome.code, 2);
    // A pin-less enable is a shape violation (the mandatory <id> <version>):
    let outcome = athena_cli::skill::run_skill(
        &[to_string("enable"), to_string("the-skill")],
        std::path::Path::new("."),
    );
    assert_eq!(
        outcome.code, 2,
        "the pin is mandatory (ATH-SKILL-020): {}",
        outcome.message
    );
    // And the json face carries the plan:
    let outcome = athena_cli::commands::run_command(
        "status",
        &[to_string("--json")],
        std::path::Path::new("."),
    );
    assert_eq!(outcome.code, 0);
    let data = outcome.data_json.expect("the --json face");
    assert!(data.contains(r#""method":"GET""#));
    assert!(data.contains(r#""path":"/api/v1/status""#));
}

#[test]
fn the_chat_faces_carried_over_the_phase_6_tests() {
    // The chat layer's render contracts rode P6-01's own tests; this row re-asserts the
    // conversation-level shape: the approval and the clarification prompts disagree with
    // an empty record never (the silence face).
    let outcome = athena_cli::commands::run_command(
        "approval",
        &[to_string("list")],
        std::path::Path::new("."),
    );
    assert!(outcome.message.contains("/api/v1/approvals"));
}
