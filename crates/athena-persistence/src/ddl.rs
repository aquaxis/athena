//! Logical schema (spec section 27.3).
//!
//! All 25 tables, their keys and their indexes. Migrations are forward-only and numbered; a
//! schema version is recorded so startup can verify it and either migrate or abort rather than
//! proceeding against a schema it does not understand (ATH-PERS-009, ATH-PERS-010).

/// The schema version this build expects.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// One forward migration.
pub struct Migration {
    /// The version this migration produces.
    pub version: u32,
    /// Human-readable description, recorded in `schema_migrations`.
    pub description: &'static str,
    /// Statements, applied in order inside one transaction.
    pub sql: &'static str,
}

/// Every migration, in ascending version order.
pub const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    description: "initial schema (spec section 27.3)",
    sql: SCHEMA_V1,
}];

/// Version 1 of the schema.
///
/// Conventions throughout:
/// - Identifiers are `TEXT` holding prefixed ULIDs (ATH-DATA-001).
/// - Times are `TEXT` holding RFC 3339 UTC (ATH-DATA-002).
/// - Enumerations are `TEXT` holding the wire form from `athena-types`, so a value read back
///   is the same token the API and Events use.
/// - `strict` is on, so a type mismatch is rejected by SQLite rather than silently coerced.
const SCHEMA_V1: &str = r"
-- Tasks form a tree; a SubTask references its parent (ATH-TASK-001).
CREATE TABLE tasks (
    id                  TEXT PRIMARY KEY,
    parent_id           TEXT REFERENCES tasks(id),
    title               TEXT NOT NULL,
    instruction         TEXT NOT NULL,
    workspace           TEXT NOT NULL,
    goal                TEXT,
    constraints_json    TEXT NOT NULL DEFAULT '[]',
    completion_criteria_json TEXT NOT NULL DEFAULT '[]',
    state               TEXT NOT NULL,
    priority            TEXT NOT NULL,
    plan_id             TEXT,
    risk_level          TEXT NOT NULL,
    attempt_count       INTEGER NOT NULL DEFAULT 0,
    max_attempts        INTEGER NOT NULL,
    iteration_count     INTEGER NOT NULL DEFAULT 0,
    max_iterations      INTEGER NOT NULL,
    deadline            TEXT,
    created_by          TEXT NOT NULL,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    finished_at         TEXT
) STRICT;
CREATE INDEX tasks_state_idx      ON tasks(state);
CREATE INDEX tasks_parent_idx     ON tasks(parent_id);
CREATE INDEX tasks_created_at_idx ON tasks(created_at);

-- A plan revision is never overwritten; (task_id, revision) is unique (ATH-PLAN-004).
CREATE TABLE plans (
    id             TEXT PRIMARY KEY,
    task_id        TEXT NOT NULL REFERENCES tasks(id),
    revision       INTEGER NOT NULL,
    goal           TEXT NOT NULL,
    created_at     TEXT NOT NULL,
    superseded_by  TEXT REFERENCES plans(id),
    UNIQUE (task_id, revision)
) STRICT;
CREATE INDEX plans_task_idx ON plans(task_id);

CREATE TABLE plan_steps (
    id              TEXT PRIMARY KEY,
    plan_id         TEXT NOT NULL REFERENCES plans(id),
    step_order      INTEGER NOT NULL,
    intent          TEXT NOT NULL,
    assigned_agent  TEXT,
    skill_set_json  TEXT NOT NULL DEFAULT '[]',
    state           TEXT NOT NULL,
    result_id       TEXT
) STRICT;
CREATE INDEX plan_steps_plan_idx  ON plan_steps(plan_id);
CREATE INDEX plan_steps_state_idx ON plan_steps(state);

-- Dependencies live in their own table so a cycle check can run as a graph query
-- (ATH-PLAN-006).
CREATE TABLE plan_step_deps (
    step_id     TEXT NOT NULL REFERENCES plan_steps(id),
    depends_on  TEXT NOT NULL REFERENCES plan_steps(id),
    PRIMARY KEY (step_id, depends_on)
) STRICT;

-- Each attempt maps to its own instance; history is never overwritten (ATH-FAIL-007).
CREATE TABLE plan_step_attempts (
    step_id      TEXT NOT NULL REFERENCES plan_steps(id),
    attempt      INTEGER NOT NULL,
    instance_id  TEXT,
    started_at   TEXT NOT NULL,
    PRIMARY KEY (step_id, attempt)
) STRICT;
CREATE INDEX plan_step_attempts_step_idx ON plan_step_attempts(step_id);

-- Dynamically generated definitions are recorded too (ATH-API-025).
CREATE TABLE agent_definitions (
    id                   TEXT PRIMARY KEY,
    name                 TEXT NOT NULL,
    level                TEXT NOT NULL,
    description          TEXT NOT NULL,
    capabilities_json    TEXT NOT NULL DEFAULT '[]',
    persona_ref          TEXT NOT NULL,
    persona_version      TEXT NOT NULL,
    allowed_tools_json   TEXT NOT NULL DEFAULT '[]',
    delegable_tools_json TEXT NOT NULL DEFAULT '[]',
    default_skills_json  TEXT NOT NULL DEFAULT '[]',
    runtime_type         TEXT NOT NULL,
    model_provider       TEXT NOT NULL,
    model_name           TEXT NOT NULL,
    max_delegation_depth INTEGER NOT NULL,
    dynamic              INTEGER NOT NULL DEFAULT 0,
    created_at           TEXT NOT NULL
) STRICT;
CREATE INDEX agent_definitions_level_idx ON agent_definitions(level);

-- task_id is nullable: the Core Agent Instance spans Tasks (ATH-STATE-012).
CREATE TABLE agent_instances (
    id                    TEXT PRIMARY KEY,
    agent_id              TEXT NOT NULL,
    task_id               TEXT REFERENCES tasks(id),
    plan_step_id          TEXT REFERENCES plan_steps(id),
    superior_instance_id  TEXT REFERENCES agent_instances(id),
    delegation_depth      INTEGER NOT NULL DEFAULT 0,
    state                 TEXT NOT NULL,
    runtime_instance_id   TEXT,
    workspace             TEXT NOT NULL,
    skill_set_json        TEXT NOT NULL DEFAULT '[]',
    aiprj_log_ref         TEXT,
    attempt               INTEGER NOT NULL DEFAULT 0,
    started_at            TEXT,
    finished_at           TEXT
) STRICT;
CREATE INDEX agent_instances_task_idx  ON agent_instances(task_id);
CREATE INDEX agent_instances_state_idx ON agent_instances(state);
CREATE INDEX agent_instances_agent_idx ON agent_instances(agent_id);

CREATE TABLE messages (
    message_id        TEXT PRIMARY KEY,
    message_type      TEXT NOT NULL,
    from_agent        TEXT NOT NULL,
    to_agent          TEXT NOT NULL,
    to_instance_id    TEXT,
    from_instance_id  TEXT NOT NULL,
    task_id           TEXT,
    thread_id         TEXT NOT NULL,
    reply_to          TEXT,
    priority          TEXT NOT NULL,
    body              TEXT NOT NULL,
    payload_json      TEXT,
    deadline          TEXT,
    created_at        TEXT NOT NULL
) STRICT;
CREATE INDEX messages_task_idx       ON messages(task_id);
CREATE INDEX messages_thread_idx     ON messages(thread_id);
CREATE INDEX messages_created_at_idx ON messages(created_at);

-- Deduplication is the Broker's responsibility, not the agent's (ATH-MSG-014).
CREATE TABLE message_deliveries (
    to_instance_id  TEXT NOT NULL,
    message_id      TEXT NOT NULL REFERENCES messages(message_id),
    delivered_at    TEXT NOT NULL,
    accepted_at     TEXT,
    PRIMARY KEY (to_instance_id, message_id)
) STRICT;
CREATE INDEX message_deliveries_message_idx ON message_deliveries(message_id);

-- Results are immutable; a correction is a new row (ATH-RESULT-003, ATH-PERS-008).
CREATE TABLE results (
    id                TEXT PRIMARY KEY,
    task_id           TEXT NOT NULL REFERENCES tasks(id),
    plan_step_id      TEXT REFERENCES plan_steps(id),
    agent_id          TEXT NOT NULL,
    instance_id       TEXT NOT NULL,
    status            TEXT NOT NULL,
    output            TEXT NOT NULL,
    logs_ref          TEXT,
    skills_used_json  TEXT NOT NULL DEFAULT '[]',
    model_provider    TEXT,
    model_name        TEXT,
    started_at        TEXT NOT NULL,
    finished_at       TEXT NOT NULL,
    execution_time_ms INTEGER NOT NULL,
    metadata_json     TEXT NOT NULL DEFAULT '{}'
) STRICT;
CREATE INDEX results_task_idx ON results(task_id);
CREATE INDEX results_step_idx ON results(plan_step_id);

-- base is 'work_target' or 'store' only; a Workspace-relative path is unrepresentable
-- (ATH-RESULT-002).
CREATE TABLE artifacts (
    result_id   TEXT NOT NULL REFERENCES results(id),
    base        TEXT NOT NULL CHECK (base IN ('work_target', 'store')),
    path        TEXT NOT NULL,
    kind        TEXT NOT NULL,
    bytes       INTEGER,
    checksum    TEXT,
    created_at  TEXT NOT NULL,
    PRIMARY KEY (result_id, base, path)
) STRICT;
CREATE INDEX artifacts_result_idx ON artifacts(result_id);

-- Skill state is per (id, version) (ATH-STATE-026).
CREATE TABLE skills (
    id                        TEXT NOT NULL,
    version                   TEXT NOT NULL,
    name                      TEXT NOT NULL,
    description               TEXT NOT NULL,
    capabilities_json         TEXT NOT NULL DEFAULT '[]',
    required_tools_json       TEXT NOT NULL DEFAULT '[]',
    required_permissions_json TEXT NOT NULL DEFAULT '[]',
    supported_agents_json     TEXT NOT NULL DEFAULT '[]',
    dependencies_json         TEXT NOT NULL DEFAULT '[]',
    runtime_provider          TEXT NOT NULL,
    source                    TEXT NOT NULL,
    checksum                  TEXT NOT NULL,
    publisher                 TEXT NOT NULL,
    risk_level                TEXT NOT NULL,
    state                     TEXT NOT NULL,
    hot_reload_safe           INTEGER NOT NULL DEFAULT 0,
    registered_at             TEXT NOT NULL,
    updated_at                TEXT NOT NULL,
    PRIMARY KEY (id, version)
) STRICT;
CREATE INDEX skills_state_idx ON skills(state);

CREATE TABLE runtime_instances (
    id                 TEXT PRIMARY KEY,
    runtime_type       TEXT NOT NULL,
    instance_id        TEXT NOT NULL UNIQUE,
    state              TEXT NOT NULL,
    pid                INTEGER,
    last_heartbeat_at  TEXT,
    started_at         TEXT NOT NULL,
    stopped_at         TEXT,
    stop_reason        TEXT,
    exit_code          INTEGER,
    side_effects       TEXT NOT NULL DEFAULT 'none'
) STRICT;
CREATE INDEX runtime_instances_state_idx ON runtime_instances(state);

CREATE TABLE approvals (
    id                        TEXT PRIMARY KEY,
    task_id                   TEXT NOT NULL REFERENCES tasks(id),
    requested_by              TEXT NOT NULL,
    operation                 TEXT NOT NULL,
    reason                    TEXT NOT NULL,
    impact                    TEXT NOT NULL,
    risk_level                TEXT NOT NULL,
    state                     TEXT NOT NULL,
    operation_descriptor      TEXT NOT NULL,
    requested_by_instance_id  TEXT NOT NULL,
    uses_remaining            INTEGER NOT NULL DEFAULT 1,
    expires_at                TEXT NOT NULL,
    responded_by              TEXT,
    responded_at              TEXT,
    scope_json                TEXT NOT NULL,
    created_at                TEXT NOT NULL
) STRICT;
CREATE INDEX approvals_task_idx    ON approvals(task_id);
CREATE INDEX approvals_state_idx   ON approvals(state);
CREATE INDEX approvals_expires_idx ON approvals(expires_at);

CREATE TABLE memory_records (
    id              TEXT PRIMARY KEY,
    tier            TEXT NOT NULL,
    scope_id        TEXT NOT NULL,
    content         TEXT NOT NULL,
    summary         TEXT,
    origin_task_id  TEXT,
    origin_event_id TEXT,
    created_at      TEXT NOT NULL,
    expires_at      TEXT
) STRICT;
CREATE INDEX memory_records_tier_idx  ON memory_records(tier);
CREATE INDEX memory_records_scope_idx ON memory_records(scope_id);

CREATE TABLE hierarchies (
    id          TEXT PRIMARY KEY,
    task_id     TEXT NOT NULL REFERENCES tasks(id),
    revision    INTEGER NOT NULL,
    created_at  TEXT NOT NULL,
    UNIQUE (task_id, revision)
) STRICT;
CREATE INDEX hierarchies_task_idx ON hierarchies(task_id);

-- sibling_acl is checked by Core Host at delivery time (ATH-MSG-018).
CREATE TABLE hierarchy_nodes (
    hierarchy_id       TEXT NOT NULL REFERENCES hierarchies(id),
    agent_id           TEXT NOT NULL,
    level              TEXT NOT NULL,
    superior_agent_id  TEXT,
    scope_ref_json     TEXT,
    sibling_acl_json   TEXT NOT NULL DEFAULT '[]',
    PRIMARY KEY (hierarchy_id, agent_id)
) STRICT;
CREATE INDEX hierarchy_nodes_hierarchy_idx ON hierarchy_nodes(hierarchy_id);

CREATE TABLE tool_executions (
    id                    TEXT PRIMARY KEY,
    instance_id           TEXT NOT NULL,
    task_id               TEXT REFERENCES tasks(id),
    tool                  TEXT NOT NULL,
    operation_descriptor  TEXT NOT NULL,
    risk_level            TEXT NOT NULL,
    approval_id           TEXT REFERENCES approvals(id),
    status                TEXT NOT NULL,
    started_at            TEXT NOT NULL,
    finished_at           TEXT
) STRICT;
CREATE INDEX tool_executions_instance_idx ON tool_executions(instance_id);
CREATE INDEX tool_executions_task_idx     ON tool_executions(task_id);
CREATE INDEX tool_executions_status_idx   ON tool_executions(status);

CREATE TABLE evaluations (
    id              TEXT PRIMARY KEY,
    task_id         TEXT NOT NULL REFERENCES tasks(id),
    result_id       TEXT NOT NULL REFERENCES results(id),
    evaluator_json  TEXT NOT NULL,
    method          TEXT NOT NULL,
    verdict         TEXT NOT NULL,
    rationale       TEXT NOT NULL,
    created_at      TEXT NOT NULL
) STRICT;
CREATE INDEX evaluations_task_idx   ON evaluations(task_id);
CREATE INDEX evaluations_result_idx ON evaluations(result_id);

-- A conversation may precede its Task (ATH-API-021).
CREATE TABLE conversations (
    id          TEXT PRIMARY KEY,
    task_id     TEXT REFERENCES tasks(id),
    created_at  TEXT NOT NULL
) STRICT;
CREATE INDEX conversations_task_idx ON conversations(task_id);

CREATE TABLE utterances (
    id                TEXT PRIMARY KEY,
    conversation_id   TEXT NOT NULL REFERENCES conversations(id),
    role              TEXT NOT NULL,
    kind              TEXT NOT NULL,
    body              TEXT NOT NULL,
    clarification_id  TEXT,
    created_at        TEXT NOT NULL
) STRICT;
CREATE INDEX utterances_conversation_idx ON utterances(conversation_id);
CREATE INDEX utterances_created_at_idx   ON utterances(created_at);

CREATE TABLE clarifications (
    id          TEXT PRIMARY KEY,
    task_id     TEXT NOT NULL REFERENCES tasks(id),
    state       TEXT NOT NULL,
    question    TEXT NOT NULL,
    answer      TEXT,
    expires_at  TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    answered_at TEXT
) STRICT;
CREATE INDEX clarifications_task_idx  ON clarifications(task_id);
CREATE INDEX clarifications_state_idx ON clarifications(state);

-- One Task may hold a Workspace at a time; the primary key is the exclusivity
-- (ATH-DATA-009).
CREATE TABLE workspace_leases (
    workspace   TEXT PRIMARY KEY,
    task_id     TEXT NOT NULL REFERENCES tasks(id),
    acquired_at TEXT NOT NULL
) STRICT;
CREATE INDEX workspace_leases_task_idx ON workspace_leases(task_id);

-- Prevents a delegation or tool execution running twice, notably after a Core Agent restart
-- (ATH-CORE-035).
CREATE TABLE idempotency_keys (
    key         TEXT PRIMARY KEY,
    outcome     TEXT,
    created_at  TEXT NOT NULL
) STRICT;
CREATE INDEX idempotency_keys_created_at_idx ON idempotency_keys(created_at);

-- Append-only. sequence is monotonic per Core (ATH-EVENT-003).
CREATE TABLE events (
    event_id        TEXT PRIMARY KEY,
    schema_version  INTEGER NOT NULL,
    event_type      TEXT NOT NULL,
    occurred_at     TEXT NOT NULL,
    sequence        INTEGER NOT NULL UNIQUE,
    task_id         TEXT,
    plan_step_id    TEXT,
    agent_id        TEXT,
    instance_id     TEXT,
    skill_id        TEXT,
    correlation_id  TEXT NOT NULL,
    causation_id    TEXT,
    actor           TEXT NOT NULL,
    payload_json    TEXT NOT NULL DEFAULT '{}'
) STRICT;
CREATE INDEX events_task_idx        ON events(task_id);
CREATE INDEX events_sequence_idx    ON events(sequence);
CREATE INDEX events_type_idx        ON events(event_type);
CREATE INDEX events_occurred_at_idx ON events(occurred_at);

-- Append-only enforcement (ATH-PERS-008). UPDATE is refused outright at the database level,
-- so no code path can rewrite history even by mistake. DELETE stays permitted because
-- retention expiry must be able to remove aged rows (ATH-EVENT-011); the repository exposes
-- deletion only through a retention-scoped call, and the final result and Results of a Task
-- must survive that sweep (ATH-EVENT-014).
CREATE TRIGGER events_are_append_only
BEFORE UPDATE ON events
BEGIN
    SELECT RAISE(ABORT, 'events is append-only (ATH-PERS-008): correct by appending a new event');
END;

CREATE TRIGGER results_are_immutable
BEFORE UPDATE ON results
BEGIN
    SELECT RAISE(ABORT, 'results is immutable (ATH-RESULT-003): record a correction as a new result');
END;

-- Artifacts belong to an immutable Result, so they are immutable with it.
CREATE TRIGGER artifacts_are_immutable
BEFORE UPDATE ON artifacts
BEGIN
    SELECT RAISE(ABORT, 'artifacts is immutable (ATH-RESULT-003)');
END;
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_ascending_and_contiguous_from_one() {
        // A gap or a repeat would make the applied-version check ambiguous.
        for (index, migration) in MIGRATIONS.iter().enumerate() {
            assert_eq!(
                migration.version,
                u32::try_from(index + 1).expect("small"),
                "migration {index} is out of order"
            );
        }
    }

    #[test]
    fn the_last_migration_produces_the_current_version() {
        let last = MIGRATIONS.last().expect("at least one migration");
        assert_eq!(last.version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn every_table_of_specification_section_27_3_is_declared() {
        // The specification's table list is restated here so a missing table fails the build's
        // tests rather than surfacing as a runtime error.
        const EXPECTED: &[&str] = &[
            "tasks",
            "plans",
            "plan_steps",
            "plan_step_deps",
            "agent_definitions",
            "agent_instances",
            "messages",
            "results",
            "artifacts",
            "skills",
            "runtime_instances",
            "approvals",
            "memory_records",
            "plan_step_attempts",
            "hierarchies",
            "hierarchy_nodes",
            "tool_executions",
            "evaluations",
            "conversations",
            "utterances",
            "clarifications",
            "workspace_leases",
            "idempotency_keys",
            "message_deliveries",
            "events",
        ];
        assert_eq!(EXPECTED.len(), 25);
        for table in EXPECTED {
            assert!(
                SCHEMA_V1.contains(&format!("CREATE TABLE {table} (")),
                "table {table} is missing from the schema"
            );
        }
    }
}
