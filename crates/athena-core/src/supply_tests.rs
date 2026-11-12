//! X-44(a)'s tests: the definition supply — the translation, the gate and the walk.
//!
//! The persona faces run against real files in a temp tree (the front matter is read, not
//! simulated), the Core Agent refusal pins risk 8, and the walk's own test proves the
//! shipped `agents = []` writes nothing while a listed entry lands exactly one row whose
//! fields are the configured ones (the drift the converged `AgentEntry` closes).

use crate::supply::{AgentEntryWide, SupplyContext, SupplyFault, supply_definitions, translate};
use athena_persistence::AthenaStore;
use athena_persistence::repository::AgentRepository;
use athena_types::{AgentLevel, RuntimeType, ToolName};

/// `<ATHENA_HOME>/personas/` in the test's temp tree, carrying one persona file.
fn fixture(name: &str, body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("temp");
    let root = dir.path().join("personas");
    std::fs::create_dir_all(&root).expect("personas root");
    std::fs::write(root.join(name), body).expect("persona file");
    (dir, root)
}

/// A worker persona with the measured `personas/` front-matter shape.
const WORKER_PERSONA: &str =
    "---\nname: sup-worker\nrole: worker\nlevel: worker\nallowed_tools:\n  - fs_read\n---\nbody\n";

/// One entry complete enough to translate; each test breaks one field.
fn entry(id: &str) -> AgentEntryWide {
    AgentEntryWide {
        id: id.into(),
        runtime: "agent-cli".into(),
        provider: Some("claude".into()),
        model: Some("claude-opus-5".into()),
        level: Some("worker".into()),
        ..AgentEntryWide::default()
    }
}

/// The configured counterpart of [`entry`].
fn config_entry(id: &str) -> athena_config::AgentEntry {
    athena_config::AgentEntry {
        id: id.into(),
        runtime: "agent-cli".into(),
        name: "Supplied Worker".into(),
        provider: "claude".into(),
        model: "claude-opus-5".into(),
        persona: "sup-worker.md".into(),
        persona_version: Some("2.1.0".into()),
        level: "worker".into(),
        capabilities: vec!["rust".into()],
        allowed_tools: vec![ToolName::FsRead],
        delegable_tools: vec![],
        skills: vec![athena_config::SkillEntry {
            id: "code-review".into(),
            version: "1.2.0".into(),
        }],
        max_delegation_depth: Some(2),
    }
}

#[test]
fn the_core_agent_slug_is_never_taken_from_the_list() {
    let dir = tempfile::tempdir().expect("temp");
    let wide = AgentEntryWide {
        id: "athena-core-agent".into(),
        ..entry("x")
    };
    let fault = translate(&wide, dir.path()).expect_err("refused (ATH-CORE-022)");
    assert!(matches!(fault, SupplyFault::CoreAgentEntry(_)), "{fault}");
}

#[test]
fn the_level_and_tools_come_from_the_persona_front_matter() {
    let (_dir, root) = fixture("sup-worker.md", WORKER_PERSONA);
    let wide = AgentEntryWide {
        persona: Some("sup-worker.md".into()),
        level: None,
        ..entry("sup-worker")
    };
    let definition = translate(&wide, &root).expect("translated");
    assert_eq!(definition.level, AgentLevel::Worker);
    assert_eq!(definition.allowed_tools, vec![ToolName::FsRead]);
    assert_eq!(definition.persona_ref, "sup-worker.md");
}

#[test]
fn a_persona_level_disagreeing_with_the_entry_is_refused() {
    let (_dir, root) = fixture("sup-worker.md", WORKER_PERSONA);
    let wide = AgentEntryWide {
        persona: Some("sup-worker.md".into()),
        level: Some("lead".into()),
        ..entry("sup-worker")
    };
    let fault = translate(&wide, &root).expect_err("refused (R-PER-03)");
    assert!(
        matches!(fault, SupplyFault::PersonaUnreadable { .. }),
        "{fault}"
    );
}

#[test]
fn an_entry_stating_no_level_and_naming_no_readable_persona_is_refused() {
    let dir = tempfile::tempdir().expect("temp");
    let wide = AgentEntryWide {
        level: None,
        ..entry("sup-levelless")
    };
    let fault = translate(&wide, dir.path()).expect_err("refused (ATH-AGENT-001)");
    assert!(matches!(fault, SupplyFault::LevelUnstated(_)), "{fault}");
}

#[test]
fn an_unknown_runtime_spelling_is_refused() {
    let dir = tempfile::tempdir().expect("temp");
    let wide = AgentEntryWide {
        runtime: "quantum".into(),
        ..entry("sup-rt")
    };
    let fault = translate(&wide, dir.path()).expect_err("refused");
    assert!(
        matches!(fault, SupplyFault::UnknownRuntime { .. }),
        "{fault}"
    );
}

#[test]
fn an_unknown_tool_spelling_is_refused() {
    let dir = tempfile::tempdir().expect("temp");
    let wide = AgentEntryWide {
        allowed_tools: vec!["telepathy".into()],
        ..entry("sup-tool")
    };
    let fault = translate(&wide, dir.path()).expect_err("refused");
    assert!(matches!(fault, SupplyFault::UnknownTool { .. }), "{fault}");
}

#[test]
fn an_entry_without_model_facts_is_refused_by_the_gate() {
    let dir = tempfile::tempdir().expect("temp");
    let wide = AgentEntryWide {
        provider: None,
        model: None,
        ..entry("sup-model")
    };
    let fault = translate(&wide, dir.path()).expect_err("refused (ATH-AGENT-008)");
    assert!(
        matches!(fault, SupplyFault::ModelUnspecified { .. }),
        "{fault}"
    );
}

#[test]
fn the_shipped_empty_list_supplies_nothing() {
    let mut store = AthenaStore::in_memory().expect("store");
    let dir = tempfile::tempdir().expect("temp");
    let config = athena_config::Config::default();
    let context = SupplyContext {
        personas_root: dir.path(),
        marker: (),
    };
    let walked = supply_definitions(&mut store, &config, &context).expect("empty walk");
    assert_eq!(walked, 0, "`agents = []` supplies nothing");
    assert!(
        AgentRepository::list_definitions(&store)
            .expect("read")
            .is_empty()
    );
}

#[test]
fn a_listed_agent_lands_one_row_carrying_its_configured_fields() {
    let (_dir, root) = fixture("sup-worker.md", WORKER_PERSONA);
    let mut store = AthenaStore::in_memory().expect("store");
    let mut config = athena_config::Config::default();
    config.agents.push(config_entry("sup-worker"));
    let context = SupplyContext {
        personas_root: &root,
        marker: (),
    };

    let walked = supply_definitions(&mut store, &config, &context).expect("supplied");
    assert_eq!(walked, 1);
    let rows = AgentRepository::list_definitions(&store).expect("read");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    // Every configured field reached the row — the convergence this unit closes.
    assert_eq!(row.id.as_str(), "sup-worker");
    assert_eq!(row.name, "Supplied Worker");
    assert_eq!(row.level, AgentLevel::Worker);
    assert_eq!(row.capabilities, vec!["rust".to_string()]);
    assert_eq!(row.persona_ref, "sup-worker.md");
    assert_eq!(row.persona_version, "2.1.0");
    assert_eq!(row.allowed_tools, vec![ToolName::FsRead]);
    assert_eq!(row.runtime, RuntimeType::AgentCli);
    assert_eq!(row.model.provider, "claude");
    assert_eq!(row.model.name, "claude-opus-5");
    assert_eq!(row.max_delegation_depth, 2);
    assert!(!row.dynamic, "a configured definition is static");
    assert_eq!(row.default_skills.len(), 1);
    assert_eq!(row.default_skills[0].id.as_str(), "code-review");
    assert_eq!(row.default_skills[0].version, "1.2.0");
}

#[test]
fn the_supply_walk_is_idempotent() {
    let (_dir, root) = fixture("sup-worker.md", WORKER_PERSONA);
    let mut store = AthenaStore::in_memory().expect("store");
    let mut config = athena_config::Config::default();
    config.agents.push(config_entry("sup-worker"));
    let context = SupplyContext {
        personas_root: &root,
        marker: (),
    };

    supply_definitions(&mut store, &config, &context).expect("first walk");
    supply_definitions(&mut store, &config, &context).expect("second walk");
    // Upsert by id: the second walk replaces the equal row rather than adding one, so the
    // boot step can run unconditionally.
    assert_eq!(
        AgentRepository::list_definitions(&store)
            .expect("read")
            .len(),
        1
    );
}
