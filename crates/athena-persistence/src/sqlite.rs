//! SQLite implementations of the Repository traits.
//!
//! All SQL lives in this module, so no other crate depends on the store's dialect
//! (ATH-PERS-007).

use crate::db::Database;
use crate::error::{PersistenceError, Result};
use crate::repository::MessageRow;
use crate::repository::{
    AgentRepository, ApprovalRepository, ClarificationRepository, EventRepository,
    IdempotencyRepository, MemoryRepository, MessageRepository, NewEvent, PlanRepository,
    ResultRepository, StoredEvent, TaskRepository, ToolExecutionRepository,
    WorkspaceLeaseRepository,
};
use athena_types::{
    AgentDefinition, AgentId, AgentInstance, AgentInstanceState, Approval, ApprovalId,
    ApprovalState, Artifact, ArtifactBase, ArtifactKind, Clarification, ClarificationId,
    ClarificationState, Evaluation, EvaluationId, EventId, InstanceId, MemoryId, MemoryOrigin,
    MemoryRecord, MemoryTier, MessageId, MessageType, ModelRef, Plan, PlanId, PlanStep, PlanStepId,
    PlanStepState, Priority, ResultId, RiskLevel, RuntimeInstanceId, SkillId, SkillRef, Task,
    TaskId, TaskState, ToolExecution, ToolExecutionId, ToolName, Verdict,
};
use rusqlite::{Connection, OptionalExtension, Row, params};

/// Decode a `TEXT` column into an enum by its wire form.
///
/// A stored token that no longer parses is an internal inconsistency, not bad input: the
/// writer and reader of the column disagree.
fn decode_enum<T, F>(
    entity: &'static str,
    column: &'static str,
    raw: &str,
    all: &'static [T],
    as_str: F,
) -> Result<T>
where
    T: Copy,
    F: Fn(&T) -> &'static str,
{
    all.iter()
        .find(|candidate| as_str(candidate) == raw)
        .copied()
        .ok_or_else(|| PersistenceError::Decode {
            entity,
            column,
            detail: format!("unknown value {raw:?}"),
        })
}

/// Decode a JSON array of strings.
fn decode_string_vec(entity: &'static str, column: &'static str, raw: &str) -> Result<Vec<String>> {
    serde_json::from_str(raw).map_err(|e| PersistenceError::Decode {
        entity,
        column,
        detail: e.to_string(),
    })
}

/// Encode a slice of strings as a JSON array.
fn encode_string_vec(values: &[String]) -> String {
    serde_json::to_string(values).unwrap_or_else(|_| "[]".to_string())
}

/// Encode skill references as JSON.
fn encode_skill_refs(refs: &[SkillRef]) -> String {
    serde_json::to_string(refs).unwrap_or_else(|_| "[]".to_string())
}

/// Decode skill references from JSON.
fn decode_skill_refs(
    entity: &'static str,
    column: &'static str,
    raw: &str,
) -> Result<Vec<SkillRef>> {
    serde_json::from_str(raw).map_err(|e| PersistenceError::Decode {
        entity,
        column,
        detail: e.to_string(),
    })
}

/// Whether a `rusqlite` error is a uniqueness violation.
///
/// Distinguishing this from a generic failure is what lets a lease or key collision surface as
/// a conflict (409) instead of an internal error (500).
fn is_unique_violation(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ErrorCode::ConstraintViolation,
                extended_code: 1555 | 2067,
            },
            _
        )
    )
}

/// SQLite-backed repositories over one database handle.
#[derive(Debug)]
pub struct SqliteStore {
    db: Database,
}

impl SqliteStore {
    /// Wrap a database handle.
    #[must_use]
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Open an in-memory store, for tests.
    pub fn in_memory() -> Result<Self> {
        Ok(Self::new(Database::open_in_memory()?))
    }

    /// Finalize one reconciliation row atomically (ATH-PERS-003): the instance state, the
    /// orphaning of its attached `RUNNING` PlanStep, and the Events describing both are
    /// written inside one transaction — the store never disagrees with its own record
    /// after a recovery (ATH-CORE-048). The transitions themselves are already validated
    /// by the §17.2 / §17.5 tables (`athena-state`); this method performs the resolved
    /// write.
    ///
    /// The returned Event is the `AgentFailed` record; the `PlanStepOrphaned` Event rides
    /// the same transaction.
    ///
    /// # Errors
    /// Store failures, surfaced whole.
    pub fn reconcile_instance(
        &mut self,
        instance: &AgentInstance,
        final_state: AgentInstanceState,
        event_type: &str,
        orphan_step: Option<&PlanStepId>,
        occurred_at: &str,
        correlation_id: &str,
    ) -> Result<StoredEvent> {
        self.db.transaction(|tx| {
            let touched = tx.execute(
                "UPDATE agent_instances SET state = ?2 WHERE id = ?1",
                params![instance.id.as_str(), final_state.as_str()],
            )?;
            if touched == 0 {
                return Err(PersistenceError::NotFound {
                    entity: "agent_instance",
                    id: instance.id.as_str().into(),
                });
            }

            let mut step_event: Option<StoredEvent> = None;
            if let Some(step) = orphan_step {
                tx.execute(
                    "UPDATE plan_steps SET state = 'ORPHANED' WHERE id = ?1",
                    params![step.as_str()],
                )?;
                let orphan = NewEvent {
                    event_id: athena_types::EventId::new(),
                    schema_version: 1,
                    event_type: "PlanStepOrphaned".into(),
                    occurred_at: occurred_at.to_string(),
                    task_id: instance.task_id.clone(),
                    plan_step_id: Some(step.clone()),
                    agent_id: Some(instance.agent_id.clone()),
                    instance_id: Some(instance.id.clone()),
                    skill_id: None,
                    correlation_id: correlation_id.to_string(),
                    causation_id: None,
                    actor: "system".into(),
                    payload_json: serde_json::json!({
                        "plan_step_id": step.as_str(),
                        "instance_id": instance.id.as_str(),
                        "side_effects": "unknown"
                    })
                    .to_string(),
                };
                let stored = append_event_tx(tx, &orphan)?;
                step_event = Some(stored);
            }

            let failure = NewEvent {
                event_id: athena_types::EventId::new(),
                schema_version: 1,
                event_type: event_type.to_string(),
                occurred_at: occurred_at.to_string(),
                task_id: instance.task_id.clone(),
                plan_step_id: instance.plan_step_id.clone(),
                agent_id: Some(instance.agent_id.clone()),
                instance_id: Some(instance.id.clone()),
                skill_id: None,
                correlation_id: correlation_id.to_string(),
                causation_id: step_event.as_ref().map(|event| event.event_id.clone()),
                actor: "system".into(),
                payload_json: serde_json::json!({
                    "error_class": "internal_error",
                    "attempt": instance.attempt,
                    "side_effects": "unknown"
                })
                .to_string(),
            };
            append_event_tx(tx, &failure)
        })
    }

    /// Borrow the underlying database.
    #[must_use]
    pub fn database(&self) -> &Database {
        &self.db
    }

    /// Borrow the underlying database mutably.
    pub fn database_mut(&mut self) -> &mut Database {
        &mut self.db
    }

    /// Apply a state transition and its Event atomically (ATH-PERS-003).
    ///
    /// This is the primitive the State Manager is built on, and the reason it exists as one
    /// call: a transition that commits while its Event is lost would leave a state no Event
    /// explains, defeating reconstruction from Events (ATH-NFR-010).
    ///
    /// `expected` implements the optimistic concurrency check of ATH-STATE-014: when the
    /// stored state differs, the transition is refused as a conflict and nothing is written.
    pub fn transition_task(
        &mut self,
        task: &TaskId,
        expected: TaskState,
        to: TaskState,
        event: &NewEvent,
    ) -> Result<StoredEvent> {
        self.db.transaction(|tx| {
            let current: Option<String> = tx
                .query_row(
                    "SELECT state FROM tasks WHERE id = ?1",
                    params![task.as_str()],
                    |row| row.get(0),
                )
                .optional()?;

            let current = current.ok_or_else(|| PersistenceError::NotFound {
                entity: "task",
                id: task.to_string(),
            })?;

            if current != expected.as_str() {
                return Err(PersistenceError::Conflict {
                    detail: format!(
                        "task {task} is in {current}, not the expected {expected}; \
                         transition refused (ATH-STATE-014)"
                    ),
                });
            }

            tx.execute(
                "UPDATE tasks SET state = ?2, updated_at = ?3 WHERE id = ?1",
                params![task.as_str(), to.as_str(), event.occurred_at],
            )?;

            append_event_tx(tx, event)
        })
    }
}

/// Append an Event inside an existing transaction, assigning the next sequence.
///
/// The sequence comes from `MAX(sequence) + 1` read inside the same transaction, so two
/// concurrent appends cannot be assigned the same position; the unique index on `sequence` is
/// the backstop (ATH-EVENT-003).
fn append_event_tx(tx: &Connection, event: &NewEvent) -> Result<StoredEvent> {
    let next: i64 = tx.query_row(
        "SELECT COALESCE(MAX(sequence), 0) + 1 FROM events",
        [],
        |row| row.get(0),
    )?;
    let sequence = u64::try_from(next).unwrap_or(0);

    tx.execute(
        "INSERT INTO events (event_id, schema_version, event_type, occurred_at, sequence,
             task_id, plan_step_id, agent_id, instance_id, skill_id, correlation_id,
             causation_id, actor, payload_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            event.event_id.as_str(),
            i64::try_from(event.schema_version).unwrap_or(1),
            event.event_type,
            event.occurred_at,
            next,
            event.task_id.as_ref().map(TaskId::as_str),
            event.plan_step_id.as_ref().map(PlanStepId::as_str),
            event.agent_id.as_ref().map(AgentId::as_str),
            event.instance_id.as_ref().map(InstanceId::as_str),
            event.skill_id.as_ref().map(SkillId::as_str),
            event.correlation_id,
            event.causation_id.as_ref().map(EventId::as_str),
            event.actor,
            event.payload_json,
        ],
    )?;

    Ok(StoredEvent {
        event_id: event.event_id.clone(),
        schema_version: event.schema_version,
        event_type: event.event_type.clone(),
        occurred_at: event.occurred_at.clone(),
        sequence,
        task_id: event.task_id.clone(),
        plan_step_id: event.plan_step_id.clone(),
        agent_id: event.agent_id.clone(),
        instance_id: event.instance_id.clone(),
        skill_id: event.skill_id.clone(),
        correlation_id: event.correlation_id.clone(),
        causation_id: event.causation_id.clone(),
        actor: event.actor.clone(),
        payload_json: event.payload_json.clone(),
    })
}

/// Build a `Task` from a row.
///
/// The outer `rusqlite::Result` carries column-access failures, which `query_map` requires;
/// the inner `Result` carries decode failures, which are internal inconsistencies rather than
/// SQL errors.
fn task_from_row(row: &Row<'_>) -> rusqlite::Result<Result<Task>> {
    let id: String = row.get("id")?;
    let parent_id: Option<String> = row.get("parent_id")?;
    let title: String = row.get("title")?;
    let instruction: String = row.get("instruction")?;
    let workspace: String = row.get("workspace")?;
    let goal: Option<String> = row.get("goal")?;
    let constraints_raw: String = row.get("constraints_json")?;
    let criteria_raw: String = row.get("completion_criteria_json")?;
    let state_raw: String = row.get("state")?;
    let priority_raw: String = row.get("priority")?;
    let plan_id: Option<String> = row.get("plan_id")?;
    let risk_raw: String = row.get("risk_level")?;
    let attempt_count: i64 = row.get("attempt_count")?;
    let max_attempts: i64 = row.get("max_attempts")?;
    let iteration_count: i64 = row.get("iteration_count")?;
    let max_iterations: i64 = row.get("max_iterations")?;
    let deadline: Option<String> = row.get("deadline")?;
    let created_by: String = row.get("created_by")?;
    let created_at: String = row.get("created_at")?;
    let updated_at: String = row.get("updated_at")?;
    let finished_at: Option<String> = row.get("finished_at")?;

    Ok((|| {
        Ok(Task {
            id: TaskId::parse(id.clone()).ok_or_else(|| PersistenceError::Decode {
                entity: "task",
                column: "id",
                detail: format!("malformed identifier {id:?}"),
            })?,
            parent_id: parent_id.and_then(TaskId::parse),
            title,
            instruction,
            workspace,
            goal,
            constraints: decode_string_vec("task", "constraints_json", &constraints_raw)?,
            completion_criteria: decode_string_vec(
                "task",
                "completion_criteria_json",
                &criteria_raw,
            )?,
            state: decode_enum("task", "state", &state_raw, TaskState::ALL, |s| s.as_str())?,
            priority: decode_enum("task", "priority", &priority_raw, Priority::ALL, |p| {
                p.as_str()
            })?,
            plan_id: plan_id.and_then(PlanId::parse),
            risk_level: decode_enum("task", "risk_level", &risk_raw, RiskLevel::ALL, |r| {
                r.as_str()
            })?,
            attempt_count: u64::try_from(attempt_count).unwrap_or(0),
            max_attempts: u64::try_from(max_attempts).unwrap_or(0),
            iteration_count: u64::try_from(iteration_count).unwrap_or(0),
            max_iterations: u64::try_from(max_iterations).unwrap_or(0),
            deadline,
            created_by,
            created_at,
            updated_at,
            finished_at,
        })
    })())
}

const TASK_COLUMNS: &str = "id, parent_id, title, instruction, workspace, goal, \
     constraints_json, completion_criteria_json, state, priority, plan_id, risk_level, \
     attempt_count, max_attempts, iteration_count, max_iterations, deadline, created_by, \
     created_at, updated_at, finished_at";

impl TaskRepository for SqliteStore {
    fn insert(&mut self, task: &Task) -> Result<()> {
        self.db
            .connection()
            .execute(
                "INSERT INTO tasks (id, parent_id, title, instruction, workspace, goal,
                     constraints_json, completion_criteria_json, state, priority, plan_id,
                     risk_level, attempt_count, max_attempts, iteration_count, max_iterations,
                     deadline, created_by, created_at, updated_at, finished_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                     ?16, ?17, ?18, ?19, ?20, ?21)",
                params![
                    task.id.as_str(),
                    task.parent_id.as_ref().map(TaskId::as_str),
                    task.title,
                    task.instruction,
                    task.workspace,
                    task.goal,
                    encode_string_vec(&task.constraints),
                    encode_string_vec(&task.completion_criteria),
                    task.state.as_str(),
                    task.priority.as_str(),
                    task.plan_id.as_ref().map(PlanId::as_str),
                    task.risk_level.as_str(),
                    i64::try_from(task.attempt_count).unwrap_or(0),
                    i64::try_from(task.max_attempts).unwrap_or(0),
                    i64::try_from(task.iteration_count).unwrap_or(0),
                    i64::try_from(task.max_iterations).unwrap_or(0),
                    task.deadline,
                    task.created_by,
                    task.created_at,
                    task.updated_at,
                    task.finished_at,
                ],
            )
            .map_err(|e| {
                if is_unique_violation(&e) {
                    PersistenceError::Conflict {
                        detail: format!("task {} already exists", task.id),
                    }
                } else {
                    PersistenceError::Backend(e)
                }
            })?;
        Ok(())
    }

    fn get(&self, id: &TaskId) -> Result<Option<Task>> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1");
        self.db
            .connection()
            .query_row(&sql, params![id.as_str()], task_from_row)
            .optional()?
            .transpose()
    }

    fn list_by_state(&self, state: TaskState, limit: u64) -> Result<Vec<Task>> {
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM tasks WHERE state = ?1 ORDER BY created_at DESC LIMIT ?2"
        );
        let conn = self.db.connection();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![state.as_str(), i64::try_from(limit).unwrap_or(i64::MAX)],
            task_from_row,
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    fn list_non_terminal(&self) -> Result<Vec<Task>> {
        // Terminal states are excluded by name rather than by a stored flag, so the set stays
        // defined in one place: `TaskState::TERMINAL`.
        let terminal: Vec<&str> = TaskState::TERMINAL.iter().map(TaskState::as_str).collect();
        let placeholders = terminal
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM tasks WHERE state NOT IN ({placeholders}) \
             ORDER BY created_at ASC"
        );
        let conn = self.db.connection();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(terminal), task_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    fn list_children(&self, parent: &TaskId) -> Result<Vec<Task>> {
        let sql =
            format!("SELECT {TASK_COLUMNS} FROM tasks WHERE parent_id = ?1 ORDER BY created_at");
        let conn = self.db.connection();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![parent.as_str()], task_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    fn list_all(&self) -> Result<Vec<Task>> {
        let conn = self.db.connection();
        let mut stmt = conn.prepare(&format!(
            "SELECT {TASK_COLUMNS} FROM tasks ORDER BY created_at DESC"
        ))?;
        let rows = stmt.query_map([], task_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    fn list_page(&self, offset: u64, limit: u64) -> Result<Vec<Task>> {
        // Keyset-free ordinal page, orders by insertion rowid (ties in `created_at`
        // beside a millisecond stay stable): the deep page costs LIMIT/OFFSET only.
        let conn = self.db.connection();
        let mut stmt = conn.prepare(&format!(
            "SELECT {TASK_COLUMNS} FROM tasks ORDER BY rowid DESC LIMIT ? OFFSET ?"
        ))?;
        let rows = stmt.query_map(params![limit, offset], task_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    fn update(&mut self, task: &Task) -> Result<()> {
        // `instruction` is deliberately absent: the human's original text is preserved and
        // never rewritten (ATH-DATA-004).
        let changed = self.db.connection().execute(
            "UPDATE tasks SET title = ?2, workspace = ?3, goal = ?4, constraints_json = ?5,
                 completion_criteria_json = ?6, state = ?7, priority = ?8, plan_id = ?9,
                 risk_level = ?10, attempt_count = ?11, max_attempts = ?12,
                 iteration_count = ?13, max_iterations = ?14, deadline = ?15, updated_at = ?16,
                 finished_at = ?17
             WHERE id = ?1",
            params![
                task.id.as_str(),
                task.title,
                task.workspace,
                task.goal,
                encode_string_vec(&task.constraints),
                encode_string_vec(&task.completion_criteria),
                task.state.as_str(),
                task.priority.as_str(),
                task.plan_id.as_ref().map(PlanId::as_str),
                task.risk_level.as_str(),
                i64::try_from(task.attempt_count).unwrap_or(0),
                i64::try_from(task.max_attempts).unwrap_or(0),
                i64::try_from(task.iteration_count).unwrap_or(0),
                i64::try_from(task.max_iterations).unwrap_or(0),
                task.deadline,
                task.updated_at,
                task.finished_at,
            ],
        )?;
        if changed == 0 {
            return Err(PersistenceError::NotFound {
                entity: "task",
                id: task.id.to_string(),
            });
        }
        Ok(())
    }
}

impl SqliteStore {
    /// Create a Task together with its `TaskCreated` Event in one transaction
    /// (ATH-PERS-003; spec §26.2, Task → State Manager → `Task*` Event).
    ///
    /// # Errors
    /// Store faults, surfaced whole.
    pub fn create_task_with_event(
        &mut self,
        task: &Task,
        event_id: &EventId,
        occurred_at: &str,
        correlation_id: &str,
    ) -> Result<StoredEvent> {
        self.db.transaction(|tx| {
            tx.execute(
                "INSERT INTO tasks (id, parent_id, title, instruction, workspace, goal,
                     constraints_json, completion_criteria_json, state, priority, plan_id,
                     risk_level, attempt_count, max_attempts, iteration_count,
                     max_iterations, deadline, created_by, created_at, updated_at,
                     finished_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                     ?16, ?17, ?18, ?19, ?20, ?21)",
                params![
                    task.id.as_str(),
                    task.parent_id.as_ref().map(TaskId::as_str),
                    task.title,
                    task.instruction,
                    task.workspace,
                    task.goal,
                    serde_json::to_string(&task.constraints).expect("list serializes"),
                    serde_json::to_string(&task.completion_criteria).expect("list serializes"),
                    task.state.as_str(),
                    task.priority.as_str(),
                    task.plan_id.as_ref().map(PlanId::as_str),
                    task.risk_level.as_str(),
                    i64::try_from(task.attempt_count).unwrap_or(0),
                    i64::try_from(task.max_attempts).unwrap_or(3),
                    i64::try_from(task.iteration_count).unwrap_or(0),
                    i64::try_from(task.max_iterations).unwrap_or(50),
                    task.deadline,
                    task.created_by,
                    task.created_at,
                    task.updated_at,
                    task.finished_at,
                ],
            )?;

            let event = NewEvent {
                event_id: event_id.clone(),
                schema_version: 1,
                event_type: "TaskCreated".into(),
                occurred_at: occurred_at.to_string(),
                task_id: Some(task.id.clone()),
                plan_step_id: None,
                agent_id: None,
                instance_id: None,
                skill_id: None,
                correlation_id: correlation_id.to_string(),
                causation_id: None,
                actor: "human".into(),
                payload_json: serde_json::json!({
                    "instruction_digest": digest_of(&task.instruction),
                    "workspace": task.workspace,
                    "priority": task.priority.as_str(),
                    "created_by": task.created_by,
                })
                .to_string(),
            };
            append_event_tx(tx, &event)
        })
    }

    /// The insert path the API reads after `create_task_with_event`.
    pub fn trans_queue_task(
        &mut self,
        task: &TaskId,
        from: TaskState,
        to: TaskState,
        event: &NewEvent,
    ) -> Result<StoredEvent> {
        self.transition_task(task, from, to, event)
    }
}

impl EventRepository for SqliteStore {
    fn append(&mut self, event: &NewEvent) -> Result<StoredEvent> {
        self.db.transaction(|tx| append_event_tx(tx, event))
    }

    fn get(&self, id: &EventId) -> Result<Option<StoredEvent>> {
        self.db
            .connection()
            .query_row(
                "SELECT event_id, schema_version, event_type, occurred_at, sequence, task_id,
                     plan_step_id, agent_id, instance_id, skill_id, correlation_id,
                     causation_id, actor, payload_json
                 FROM events WHERE event_id = ?1",
                params![id.as_str()],
                event_from_row,
            )
            .optional()?
            .transpose()
    }

    fn list_for_task(&self, task: &TaskId) -> Result<Vec<StoredEvent>> {
        // Ordered by sequence, which is what makes replay reproduce the real order
        // (ATH-EVENT-004, ATH-EVENT-012).
        let conn = self.db.connection();
        let mut stmt = conn.prepare(
            "SELECT event_id, schema_version, event_type, occurred_at, sequence, task_id,
                 plan_step_id, agent_id, instance_id, skill_id, correlation_id, causation_id,
                 actor, payload_json
             FROM events WHERE task_id = ?1 ORDER BY sequence ASC",
        )?;
        let rows = stmt.query_map(params![task.as_str()], event_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    fn list_since(&self, sequence: u64, limit: u64) -> Result<Vec<StoredEvent>> {
        let conn = self.db.connection();
        let mut stmt = conn.prepare(
            "SELECT event_id, schema_version, event_type, occurred_at, sequence, task_id,
                 plan_step_id, agent_id, instance_id, skill_id, correlation_id, causation_id,
                 actor, payload_json
             FROM events WHERE sequence > ?1 ORDER BY sequence ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![
                i64::try_from(sequence).unwrap_or(0),
                i64::try_from(limit).unwrap_or(i64::MAX)
            ],
            event_from_row,
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    fn max_sequence(&self) -> Result<u64> {
        let max: i64 = self.db.connection().query_row(
            "SELECT COALESCE(MAX(sequence), 0) FROM events",
            [],
            |row| row.get(0),
        )?;
        Ok(u64::try_from(max).unwrap_or(0))
    }

    fn prune_before(&mut self, cutoff_rfc3339: &str) -> Result<u64> {
        // Results live in their own table and are not touched here, which is how the sweep
        // preserves a Task's final result (ATH-EVENT-014).
        let removed = self.db.connection().execute(
            "DELETE FROM events WHERE occurred_at < ?1",
            params![cutoff_rfc3339],
        )?;
        Ok(removed as u64)
    }
}

/// Build a `StoredEvent` from a row.
fn event_from_row(row: &Row<'_>) -> rusqlite::Result<Result<StoredEvent>> {
    let event_id: String = row.get("event_id")?;
    let schema_version: i64 = row.get("schema_version")?;
    let event_type: String = row.get("event_type")?;
    let occurred_at: String = row.get("occurred_at")?;
    let sequence: i64 = row.get("sequence")?;
    let task_id: Option<String> = row.get("task_id")?;
    let plan_step_id: Option<String> = row.get("plan_step_id")?;
    let agent_id: Option<String> = row.get("agent_id")?;
    let instance_id: Option<String> = row.get("instance_id")?;
    let skill_id: Option<String> = row.get("skill_id")?;
    let correlation_id: String = row.get("correlation_id")?;
    let causation_id: Option<String> = row.get("causation_id")?;
    let actor: String = row.get("actor")?;
    let payload_json: String = row.get("payload_json")?;

    Ok((|| {
        Ok(StoredEvent {
            event_id: EventId::parse(event_id.clone()).ok_or_else(|| PersistenceError::Decode {
                entity: "event",
                column: "event_id",
                detail: format!("malformed identifier {event_id:?}"),
            })?,
            schema_version: u64::try_from(schema_version).unwrap_or(1),
            event_type,
            occurred_at,
            sequence: u64::try_from(sequence).unwrap_or(0),
            task_id: task_id.and_then(TaskId::parse),
            plan_step_id: plan_step_id.and_then(PlanStepId::parse),
            agent_id: agent_id.map(AgentId::new),
            instance_id: instance_id.and_then(InstanceId::parse),
            skill_id: skill_id.map(SkillId::new),
            correlation_id,
            causation_id: causation_id.and_then(EventId::parse),
            actor,
            payload_json,
        })
    })())
}

impl IdempotencyRepository for SqliteStore {
    fn claim(&mut self, key: &str) -> Result<bool> {
        // `INSERT ... ON CONFLICT DO NOTHING` makes the claim atomic: two concurrent attempts
        // cannot both believe they won, which is what prevents a double delegation
        // (ATH-CORE-035, ATH-NFR-017).
        let inserted = self.db.connection().execute(
            "INSERT INTO idempotency_keys (key, created_at)
             VALUES (?1, ?2) ON CONFLICT(key) DO NOTHING",
            params![key, crate::db::now_rfc3339()],
        )?;
        Ok(inserted == 1)
    }

    fn is_claimed(&self, key: &str) -> Result<bool> {
        let exists: bool = self.db.connection().query_row(
            "SELECT EXISTS(SELECT 1 FROM idempotency_keys WHERE key = ?1)",
            params![key],
            |row| row.get(0),
        )?;
        Ok(exists)
    }

    fn record_outcome(&mut self, key: &str, outcome: &str) -> Result<()> {
        let changed = self.db.connection().execute(
            "UPDATE idempotency_keys SET outcome = ?2 WHERE key = ?1",
            params![key, outcome],
        )?;
        if changed == 0 {
            return Err(PersistenceError::NotFound {
                entity: "idempotency_key",
                id: key.to_string(),
            });
        }
        Ok(())
    }

    fn outcome(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .connection()
            .query_row(
                "SELECT outcome FROM idempotency_keys WHERE key = ?1",
                params![key],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    fn prune_before(&mut self, cutoff_rfc3339: &str) -> Result<u64> {
        let removed = self.db.connection().execute(
            "DELETE FROM idempotency_keys WHERE created_at < ?1",
            params![cutoff_rfc3339],
        )?;
        Ok(removed as u64)
    }
}

impl WorkspaceLeaseRepository for SqliteStore {
    fn acquire(&mut self, workspace: &str, task: &TaskId) -> Result<bool> {
        let inserted = self.db.connection().execute(
            "INSERT INTO workspace_leases (workspace, task_id, acquired_at)
             VALUES (?1, ?2, ?3) ON CONFLICT(workspace) DO NOTHING",
            params![workspace, task.as_str(), crate::db::now_rfc3339()],
        )?;
        Ok(inserted == 1)
    }

    fn release(&mut self, workspace: &str) -> Result<()> {
        self.db.connection().execute(
            "DELETE FROM workspace_leases WHERE workspace = ?1",
            params![workspace],
        )?;
        Ok(())
    }

    fn holder(&self, workspace: &str) -> Result<Option<TaskId>> {
        Ok(self
            .db
            .connection()
            .query_row(
                "SELECT task_id FROM workspace_leases WHERE workspace = ?1",
                params![workspace],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .and_then(TaskId::parse))
    }
}

impl PlanRepository for SqliteStore {
    fn insert(&mut self, plan: &Plan) -> Result<()> {
        self.db.transaction(|tx| {
            tx.execute(
                "INSERT INTO plans (id, task_id, revision, goal, created_at, superseded_by)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    plan.id.as_str(),
                    plan.task_id.as_str(),
                    i64::try_from(plan.revision).unwrap_or(0),
                    plan.goal,
                    plan.created_at,
                    plan.superseded_by.as_ref().map(PlanId::as_str),
                ],
            )
            .map_err(|e| {
                if is_unique_violation(&e) {
                    PersistenceError::Conflict {
                        detail: format!(
                            "plan revision {} for task {} already exists; a revision is never \
                             overwritten (ATH-PLAN-004)",
                            plan.revision, plan.task_id
                        ),
                    }
                } else {
                    PersistenceError::Backend(e)
                }
            })?;

            for step in &plan.steps {
                tx.execute(
                    "INSERT INTO plan_steps (id, plan_id, step_order, intent, assigned_agent,
                         skill_set_json, state, result_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        step.id.as_str(),
                        step.plan_id.as_str(),
                        i64::try_from(step.order).unwrap_or(0),
                        step.intent,
                        step.assigned_agent.as_ref().map(AgentId::as_str),
                        encode_skill_refs(&step.skill_set),
                        step.state.as_str(),
                        step.result_id.as_ref().map(ResultId::as_str),
                    ],
                )?;
            }

            // Edges are inserted after every step exists, so the foreign keys resolve
            // regardless of the order steps appear in.
            for step in &plan.steps {
                for dep in &step.depends_on {
                    tx.execute(
                        "INSERT INTO plan_step_deps (step_id, depends_on) VALUES (?1, ?2)",
                        params![step.id.as_str(), dep.as_str()],
                    )?;
                }
            }
            Ok(())
        })
    }

    fn get(&self, id: &PlanId) -> Result<Option<Plan>> {
        let Some((task_id, revision, goal, created_at, superseded_by)) = self
            .db
            .connection()
            .query_row(
                "SELECT task_id, revision, goal, created_at, superseded_by FROM plans
                 WHERE id = ?1",
                params![id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()?
        else {
            return Ok(None);
        };

        Ok(Some(Plan {
            id: id.clone(),
            task_id: TaskId::parse(task_id.clone()).ok_or_else(|| PersistenceError::Decode {
                entity: "plan",
                column: "task_id",
                detail: format!("malformed identifier {task_id:?}"),
            })?,
            revision: u64::try_from(revision).unwrap_or(0),
            goal,
            steps: self.steps_for_plan(id)?,
            created_at,
            superseded_by: superseded_by.and_then(PlanId::parse),
        }))
    }

    fn latest_for_task(&self, task: &TaskId) -> Result<Option<Plan>> {
        let id: Option<String> = self
            .db
            .connection()
            .query_row(
                "SELECT id FROM plans WHERE task_id = ?1 ORDER BY revision DESC LIMIT 1",
                params![task.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        match id.and_then(PlanId::parse) {
            Some(plan_id) => PlanRepository::get(self, &plan_id),
            None => Ok(None),
        }
    }

    fn revision_for_task(&self, task: &TaskId, revision: u64) -> Result<Option<Plan>> {
        let id: Option<String> = self
            .db
            .connection()
            .query_row(
                "SELECT id FROM plans WHERE task_id = ?1 AND revision = ?2",
                params![task.as_str(), i64::try_from(revision).unwrap_or(0)],
                |row| row.get(0),
            )
            .optional()?;
        match id.and_then(PlanId::parse) {
            Some(plan_id) => PlanRepository::get(self, &plan_id),
            None => Ok(None),
        }
    }

    fn revisions_for_task(&self, task: &TaskId) -> Result<Vec<u64>> {
        let conn = self.db.connection();
        let mut stmt =
            conn.prepare("SELECT revision FROM plans WHERE task_id = ?1 ORDER BY revision ASC")?;
        let rows = stmt.query_map(params![task.as_str()], |row| row.get::<_, i64>(0))?;
        Ok(rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|r| u64::try_from(r).unwrap_or(0))
            .collect())
    }

    fn update_step_state(&mut self, step: &PlanStepId, state: PlanStepState) -> Result<()> {
        let changed = self.db.connection().execute(
            "UPDATE plan_steps SET state = ?2 WHERE id = ?1",
            params![step.as_str(), state.as_str()],
        )?;
        if changed == 0 {
            return Err(PersistenceError::NotFound {
                entity: "plan_step",
                id: step.to_string(),
            });
        }
        Ok(())
    }

    fn get_step(&self, step: &PlanStepId) -> Result<Option<PlanStep>> {
        let row = self
            .db
            .connection()
            .query_row(
                "SELECT id, plan_id, step_order, intent, assigned_agent, skill_set_json, state,
                     result_id
                 FROM plan_steps WHERE id = ?1",
                params![step.as_str()],
                step_from_row,
            )
            .optional()?;
        match row {
            Some(step_result) => {
                let mut built = step_result?;
                built.depends_on = self.deps_for_step(&built.id)?;
                Ok(Some(built))
            }
            None => Ok(None),
        }
    }

    fn record_attempt(
        &mut self,
        step: &PlanStepId,
        attempt: u64,
        instance: Option<&InstanceId>,
    ) -> Result<()> {
        self.db
            .connection()
            .execute(
                "INSERT INTO plan_step_attempts (step_id, attempt, instance_id, started_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    step.as_str(),
                    i64::try_from(attempt).unwrap_or(0),
                    instance.map(InstanceId::as_str),
                    crate::db::now_rfc3339(),
                ],
            )
            .map_err(|e| {
                if is_unique_violation(&e) {
                    PersistenceError::Conflict {
                        detail: format!(
                            "attempt {attempt} of step {step} is already recorded; attempt \
                             history is never overwritten (ATH-FAIL-007)"
                        ),
                    }
                } else {
                    PersistenceError::Backend(e)
                }
            })?;
        Ok(())
    }
}

impl SqliteStore {
    /// Steps of a plan, with their dependency edges.
    fn steps_for_plan(&self, plan: &PlanId) -> Result<Vec<PlanStep>> {
        let conn = self.db.connection();
        let mut stmt = conn.prepare(
            "SELECT id, plan_id, step_order, intent, assigned_agent, skill_set_json, state,
                 result_id
             FROM plan_steps WHERE plan_id = ?1 ORDER BY step_order ASC",
        )?;
        let rows = stmt.query_map(params![plan.as_str()], step_from_row)?;
        let mut steps: Vec<PlanStep> = rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
        for step in &mut steps {
            step.depends_on = self.deps_for_step(&step.id)?;
        }
        Ok(steps)
    }

    /// Dependency edges of a step.
    fn deps_for_step(&self, step: &PlanStepId) -> Result<Vec<PlanStepId>> {
        let conn = self.db.connection();
        let mut stmt = conn.prepare(
            "SELECT depends_on FROM plan_step_deps WHERE step_id = ?1 ORDER BY depends_on",
        )?;
        let rows = stmt.query_map(params![step.as_str()], |row| row.get::<_, String>(0))?;
        Ok(rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter_map(PlanStepId::parse)
            .collect())
    }
}

/// Build a `PlanStep` from a row. `depends_on` is filled separately.
fn step_from_row(row: &Row<'_>) -> rusqlite::Result<Result<PlanStep>> {
    let id: String = row.get("id")?;
    let plan_id: String = row.get("plan_id")?;
    let step_order: i64 = row.get("step_order")?;
    let intent: String = row.get("intent")?;
    let assigned_agent: Option<String> = row.get("assigned_agent")?;
    let skills_raw: String = row.get("skill_set_json")?;
    let state_raw: String = row.get("state")?;
    let result_id: Option<String> = row.get("result_id")?;

    Ok((|| {
        Ok(PlanStep {
            id: PlanStepId::parse(id.clone()).ok_or_else(|| PersistenceError::Decode {
                entity: "plan_step",
                column: "id",
                detail: format!("malformed identifier {id:?}"),
            })?,
            plan_id: PlanId::parse(plan_id.clone()).ok_or_else(|| PersistenceError::Decode {
                entity: "plan_step",
                column: "plan_id",
                detail: format!("malformed identifier {plan_id:?}"),
            })?,
            order: u64::try_from(step_order).unwrap_or(0),
            intent,
            depends_on: Vec::new(),
            assigned_agent: assigned_agent.map(AgentId::new),
            skill_set: decode_skill_refs("plan_step", "skill_set_json", &skills_raw)?,
            state: decode_enum("plan_step", "state", &state_raw, PlanStepState::ALL, |s| {
                s.as_str()
            })?,
            result_id: result_id.and_then(ResultId::parse),
        })
    })())
}

// --- Agent repository (P1-05 row) --------------------------------------------------------

impl AgentRepository for SqliteStore {
    fn upsert_definition(&mut self, def: &AgentDefinition) -> Result<()> {
        self.db.connection().execute(
            "INSERT INTO agent_definitions (id, name, level, description, capabilities_json,
                 persona_ref, persona_version, allowed_tools_json, delegable_tools_json,
                 default_skills_json, runtime_type, model_provider, model_name,
                 max_delegation_depth, dynamic, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
             ON CONFLICT(id) DO UPDATE SET
                 name = excluded.name, level = excluded.level,
                 description = excluded.description,
                 capabilities_json = excluded.capabilities_json,
                 persona_ref = excluded.persona_ref,
                 persona_version = excluded.persona_version,
                 allowed_tools_json = excluded.allowed_tools_json,
                 delegable_tools_json = excluded.delegable_tools_json,
                 default_skills_json = excluded.default_skills_json,
                 runtime_type = excluded.runtime_type,
                 model_provider = excluded.model_provider,
                 model_name = excluded.model_name,
                 max_delegation_depth = excluded.max_delegation_depth,
                 dynamic = excluded.dynamic",
            params![
                def.id.as_str(),
                def.name,
                def.level.as_str(),
                def.description,
                serde_json::to_string(&def.capabilities).expect("list serializes"),
                def.persona_ref,
                def.persona_version,
                serde_json::to_string(&def.allowed_tools).expect("list serializes"),
                serde_json::to_string(&def.delegable_tools).expect("list serializes"),
                serde_json::to_string(&def.default_skills).expect("list serializes"),
                def.runtime.as_str(),
                def.model.provider,
                def.model.name,
                i64::try_from(def.max_delegation_depth).unwrap_or(0),
                i64::from(def.dynamic),
                crate::db::now_rfc3339(),
            ],
        )?;
        Ok(())
    }

    fn get_definition(&self, id: &AgentId) -> Result<Option<AgentDefinition>> {
        self.db
            .connection()
            .query_row(
                "SELECT id, name, level, description, capabilities_json, persona_ref,
                     persona_version, allowed_tools_json, delegable_tools_json,
                     default_skills_json, runtime_type, model_provider, model_name,
                     max_delegation_depth, dynamic
                 FROM agent_definitions WHERE id = ?1",
                params![id.as_str()],
                definition_from_row,
            )
            .optional()?
            .transpose()
    }

    fn list_definitions(&self) -> Result<Vec<AgentDefinition>> {
        let conn = self.db.connection();
        let mut statement = conn.prepare(
            "SELECT id, name, level, description, capabilities_json, persona_ref,
                 persona_version, allowed_tools_json, delegable_tools_json,
                 default_skills_json, runtime_type, model_provider, model_name,
                 max_delegation_depth, dynamic
             FROM agent_definitions ORDER BY id",
        )?;
        let rows = statement.query_map([], definition_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    fn insert_instance(&mut self, instance: &AgentInstance) -> Result<()> {
        // The definition row is the validated parent record (ATH-AGENT-002): an instance
        // must not outlive a definition absent from the store.
        self.db
            .connection()
            .execute(
                "INSERT INTO agent_instances (id, agent_id, task_id, plan_step_id,
                 superior_instance_id, delegation_depth, state, runtime_instance_id,
                 workspace, skill_set_json, aiprj_log_ref, attempt, started_at, finished_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    instance.id.as_str(),
                    instance.agent_id.as_str(),
                    instance.task_id.as_ref().map(TaskId::as_str),
                    instance.plan_step_id.as_ref().map(PlanStepId::as_str),
                    instance
                        .superior_instance_id
                        .as_ref()
                        .map(InstanceId::as_str),
                    i64::try_from(instance.delegation_depth).unwrap_or(0),
                    instance.state.as_str(),
                    instance
                        .runtime_instance_id
                        .as_ref()
                        .map(RuntimeInstanceId::as_str),
                    instance.workspace,
                    serde_json::to_string(&instance.skill_set).expect("skills serialize"),
                    instance.aiprj_log_ref,
                    i64::try_from(instance.attempt).unwrap_or(0),
                    instance.started_at,
                    instance.finished_at,
                ],
            )
            .map_err(|e| {
                if is_unique_violation(&e) {
                    PersistenceError::Conflict {
                        detail: format!("agent instance {} exists", instance.id.as_str()),
                    }
                } else if !matches!(&e, rusqlite::Error::SqliteFailure(ffi, _)
                if ffi.code == rusqlite::ffi::ErrorCode::ConstraintViolation)
                {
                    PersistenceError::NotFound {
                        entity: "agent_definition",
                        id: instance.agent_id.as_str().into(),
                    }
                } else {
                    PersistenceError::Backend(e)
                }
            })?;
        Ok(())
    }

    fn get_instance(&self, id: &InstanceId) -> Result<Option<AgentInstance>> {
        self.db
            .connection()
            .query_row(
                "SELECT id, agent_id, task_id, plan_step_id, superior_instance_id,
                     delegation_depth, state, runtime_instance_id, workspace, skill_set_json,
                     aiprj_log_ref, attempt, started_at, finished_at
                 FROM agent_instances WHERE id = ?1",
                params![id.as_str()],
                instance_from_row,
            )
            .optional()?
            .transpose()
    }

    fn update_instance_state(&mut self, id: &InstanceId, state: AgentInstanceState) -> Result<()> {
        let touched = self.db.connection().execute(
            "UPDATE agent_instances SET state = ?2 WHERE id = ?1",
            params![id.as_str(), state.as_str()],
        )?;
        if touched == 0 {
            return Err(PersistenceError::NotFound {
                entity: "agent_instance",
                id: id.as_str().into(),
            });
        }
        Ok(())
    }

    fn list_instances_for_task(&self, task: &TaskId) -> Result<Vec<AgentInstance>> {
        let conn = self.db.connection();
        let mut statement = conn.prepare(
            "SELECT id, agent_id, task_id, plan_step_id, superior_instance_id,
                 delegation_depth, state, runtime_instance_id, workspace, skill_set_json,
                 aiprj_log_ref, attempt, started_at, finished_at
             FROM agent_instances WHERE task_id = ?1 ORDER BY attempt, id",
        )?;
        let rows = statement.query_map(params![task.as_str()], instance_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    fn list_non_terminal_instances(&self) -> Result<Vec<AgentInstance>> {
        // The startup reconciliation reads exactly these (ATH-CORE-048); `STOPPED` is
        // terminal, `FAILED` is quasi-terminal and still reclaimable (spec §17.2).
        let conn = self.db.connection();
        let mut statement = conn.prepare(
            "SELECT id, agent_id, task_id, plan_step_id, superior_instance_id,
                 delegation_depth, state, runtime_instance_id, workspace, skill_set_json,
                 aiprj_log_ref, attempt, started_at, finished_at
             FROM agent_instances WHERE state != 'STOPPED' ORDER BY started_at, id",
        )?;
        let rows = statement.query_map([], instance_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    fn count_non_terminal_for_step(&self, step: &PlanStepId) -> Result<u64> {
        // ATH-DATA-010: a step with a live instance refuses a second delegation.
        let count: i64 = self.db.connection().query_row(
            "SELECT COUNT(*) FROM agent_instances
             WHERE plan_step_id = ?1 AND state NOT IN ('STOPPED', 'FAILED')",
            params![step.as_str()],
            |row| row.get(0),
        )?;
        Ok(u64::try_from(count).unwrap_or(0))
    }
    fn list_all_instances(&self) -> Result<Vec<AgentInstance>> {
        let conn = self.db.connection();
        let mut statement = conn.prepare(
            "SELECT id, agent_id, task_id, plan_step_id, superior_instance_id, state,
                 runtime_instance_id, workspace, skill_set_json, aiprj_log_ref, attempt,
                 started_at, finished_at
             FROM agent_instances ORDER BY started_at, id",
        )?;
        let rows = statement.query_map([], instance_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()
    }
}

#[allow(clippy::too_many_lines)]
fn definition_from_row(row: &Row<'_>) -> rusqlite::Result<Result<AgentDefinition>> {
    let id = row.get::<_, String>("id")?;
    let name = row.get::<_, String>("name")?;
    let level_raw = row.get::<_, String>("level")?;
    let description = row.get::<_, String>("description")?;
    let capabilities: Vec<String> =
        serde_json::from_str(&row.get::<_, String>("capabilities_json")?).map_err(|e| {
            rusqlite::Error::ToSqlConversionFailure(Box::new(PersistenceError::Decode {
                entity: "agent_definition",
                column: "capabilities_json",
                detail: e.to_string(),
            }))
        })?;
    let persona_ref = row.get::<_, String>("persona_ref")?;
    let persona_version = row.get::<_, String>("persona_version")?;
    let allowed_tools_raw = row.get::<_, String>("allowed_tools_json")?;
    let delegable_tools_raw = row.get::<_, String>("delegable_tools_json")?;
    let default_skills_raw = row.get::<_, String>("default_skills_json")?;
    let runtime_raw = row.get::<_, String>("runtime_type")?;
    let provider = row.get::<_, String>("model_provider")?;
    let model_name = row.get::<_, String>("model_name")?;
    let depth = row.get::<_, i64>("max_delegation_depth")?;
    let dynamic = row.get::<_, i64>("dynamic")?;

    Ok((|| {
        Ok(AgentDefinition {
            id: AgentId::new(id.clone()),
            name,
            level: decode_enum(
                "agent_definition",
                "level",
                &level_raw,
                athena_types::AgentLevel::ALL,
                athena_types::AgentLevel::as_str,
            )?,
            description,
            capabilities,
            persona_ref,
            persona_version,
            allowed_tools: decode_tool_names(
                "agent_definition",
                "allowed_tools_json",
                &allowed_tools_raw,
            )?,
            delegable_tools: decode_tool_names(
                "agent_definition",
                "delegable_tools_json",
                &delegable_tools_raw,
            )?,
            default_skills: decode_skill_refs(
                "agent_definition",
                "default_skills_json",
                &default_skills_raw,
            )?,
            model: ModelRef {
                provider,
                name: model_name,
            },
            max_delegation_depth: u64::try_from(depth).unwrap_or(0),
            runtime: decode_enum(
                "agent_definition",
                "runtime_type",
                &runtime_raw,
                athena_types::RuntimeType::ALL,
                athena_types::RuntimeType::as_str,
            )?,
            dynamic: dynamic != 0,
        })
    })())
}

/// Decode a JSON list of tool wire names.
fn decode_tool_names(
    entity: &'static str,
    column: &'static str,
    raw: &str,
) -> Result<Vec<ToolName>> {
    let names: Vec<String> = serde_json::from_str(raw).map_err(|e| PersistenceError::Decode {
        entity,
        column,
        detail: e.to_string(),
    })?;
    names
        .iter()
        .map(|name| decode_enum(entity, column, name, ToolName::ALL, ToolName::as_str))
        .collect()
}

fn instance_from_row(row: &Row<'_>) -> rusqlite::Result<Result<AgentInstance>> {
    let id = row.get::<_, String>("id")?;
    let agent_id = row.get::<_, String>("agent_id")?;
    let task_id = row.get::<_, Option<String>>("task_id")?;
    let plan_step_id = row.get::<_, Option<String>>("plan_step_id")?;
    let superior = row.get::<_, Option<String>>("superior_instance_id")?;
    let depth = row.get::<_, i64>("delegation_depth")?;
    let state_raw = row.get::<_, String>("state")?;
    let runtime_instance_id = row.get::<_, Option<String>>("runtime_instance_id")?;
    let workspace = row.get::<_, String>("workspace")?;
    let skills_raw = row.get::<_, String>("skill_set_json")?;
    let log_ref = row.get::<_, Option<String>>("aiprj_log_ref")?;
    let attempt = row.get::<_, i64>("attempt")?;
    let started_at = row.get::<_, Option<String>>("started_at")?;
    let finished_at = row.get::<_, Option<String>>("finished_at")?;

    Ok((|| {
        let state: AgentInstanceState = decode_enum(
            "agent_instance",
            "state",
            &state_raw,
            AgentInstanceState::ALL,
            AgentInstanceState::as_str,
        )?;
        Ok(AgentInstance {
            id: InstanceId::parse(id.clone()).ok_or_else(|| PersistenceError::Decode {
                entity: "agent_instance",
                column: "id",
                detail: format!("malformed identifier {id:?}"),
            })?,
            agent_id: AgentId::new(agent_id),
            task_id: task_id.and_then(TaskId::parse),
            plan_step_id: plan_step_id.and_then(PlanStepId::parse),
            superior_instance_id: superior.and_then(InstanceId::parse),
            delegation_depth: u64::try_from(depth).unwrap_or(0),
            state,
            runtime_instance_id: runtime_instance_id.and_then(RuntimeInstanceId::parse),
            workspace,
            skill_set: decode_skill_refs("agent_instance", "skill_set_json", &skills_raw)?,
            aiprj_log_ref: log_ref,
            attempt: u64::try_from(attempt).unwrap_or(0),
            started_at,
            finished_at,
        })
    })())
}

// --- Approval repository (P1-05 row) -----------------------------------------------------

impl ApprovalRepository for SqliteStore {
    fn insert(&mut self, approval: &Approval) -> Result<()> {
        self.db
            .connection()
            .execute(
                "INSERT INTO approvals (id, task_id, requested_by, operation, reason, impact,
                 risk_level, state, operation_descriptor, requested_by_instance_id,
                 uses_remaining, expires_at, responded_by, responded_at, scope_json,
                 created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    approval.id.as_str(),
                    approval.task_id.as_str(),
                    approval.requested_by.as_str(),
                    approval.operation,
                    approval.reason,
                    approval.impact,
                    approval.risk_level.as_str(),
                    approval.state.as_str(),
                    approval.operation_descriptor,
                    approval.requested_by_instance_id.as_str(),
                    i64::try_from(approval.uses_remaining).unwrap_or(1),
                    approval.expires_at,
                    approval.responded_by,
                    approval.responded_at,
                    serde_json::to_string(&approval.scope).expect("scope serializes"),
                    crate::db::now_rfc3339(),
                ],
            )
            .map(|_| ())
            .map_err(|e| {
                if is_unique_violation(&e) {
                    PersistenceError::Conflict {
                        detail: format!("approval {} exists", approval.id.as_str()),
                    }
                } else {
                    PersistenceError::Backend(e)
                }
            })
    }

    fn get(&self, id: &ApprovalId) -> Result<Option<Approval>> {
        self.db
            .connection()
            .query_row(
                "SELECT id, task_id, requested_by, operation, reason, impact, risk_level,
                     state, operation_descriptor, requested_by_instance_id, uses_remaining,
                     expires_at, responded_by, responded_at, scope_json, created_at
                 FROM approvals WHERE id = ?1",
                params![id.as_str()],
                approval_from_row,
            )
            .optional()?
            .transpose()
    }

    fn list_by_state(&self, state: ApprovalState) -> Result<Vec<Approval>> {
        let conn = self.db.connection();
        let mut statement = conn.prepare(
            "SELECT id, task_id, requested_by, operation, reason, impact, risk_level,
                 state, operation_descriptor, requested_by_instance_id, uses_remaining,
                 expires_at, responded_by, responded_at, scope_json, created_at
             FROM approvals WHERE state = ?1 ORDER BY expires_at, id",
        )?;
        let rows = statement.query_map(params![state.as_str()], approval_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    fn update_state(
        &mut self,
        id: &ApprovalId,
        state: ApprovalState,
        responded_by: Option<&str>,
        uses_remaining: u64,
    ) -> Result<()> {
        let touched = self.db.connection().execute(
            "UPDATE approvals SET state = ?2,
                 responded_by = coalesce(?3, responded_by),
                 responded_at = CASE WHEN ?3 IS NOT NULL
                     THEN strftime('%Y-%m-%dT%H:%M:%fZ', 'now') ELSE responded_at END,
                 uses_remaining = ?4
             WHERE id = ?1",
            params![
                id.as_str(),
                state.as_str(),
                responded_by,
                i64::try_from(uses_remaining).unwrap_or(1),
            ],
        )?;
        if touched == 0 {
            return Err(PersistenceError::NotFound {
                entity: "approval",
                id: id.as_str().into(),
            });
        }
        Ok(())
    }

    fn list_expired(&self, now_rfc3339: &str) -> Result<Vec<Approval>> {
        // ATH-HITL-005: a `REQUESTED` approval past its deadline is a rejection — the
        // expiry sweep reads exactly these.
        let conn = self.db.connection();
        let mut statement = conn.prepare(
            "SELECT id, task_id, requested_by, operation, reason, impact, risk_level,
                 state, operation_descriptor, requested_by_instance_id, uses_remaining,
                 expires_at, responded_by, responded_at, scope_json, created_at
             FROM approvals WHERE state = 'REQUESTED' AND expires_at < ?1
             ORDER BY expires_at, id",
        )?;
        let rows = statement.query_map(params![now_rfc3339], approval_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    fn list_granted_unconsumed(&self) -> Result<Vec<Approval>> {
        // ATH-CORE-046: `GRANTED` and unconsumed must appear in restart context.
        let conn = self.db.connection();
        let mut statement = conn.prepare(
            "SELECT id, task_id, requested_by, operation, reason, impact, risk_level,
                 state, operation_descriptor, requested_by_instance_id, uses_remaining,
                 expires_at, responded_by, responded_at, scope_json, created_at
             FROM approvals WHERE state = 'GRANTED' AND uses_remaining > 0
             ORDER BY expires_at, id",
        )?;
        let rows = statement.query_map([], approval_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }
}

#[allow(clippy::too_many_lines, reason = "one mapping per column")]
fn approval_from_row(row: &Row<'_>) -> rusqlite::Result<Result<Approval>> {
    let id = row.get::<_, String>("id")?;
    let task_id = row.get::<_, String>("task_id")?;
    let requested_by = row.get::<_, String>("requested_by")?;
    let operation = row.get::<_, String>("operation")?;
    let reason = row.get::<_, String>("reason")?;
    let impact = row.get::<_, String>("impact")?;
    let risk_raw = row.get::<_, String>("risk_level")?;
    let state_raw = row.get::<_, String>("state")?;
    let descriptor = row.get::<_, String>("operation_descriptor")?;
    let requested_by_instance_id = row.get::<_, String>("requested_by_instance_id")?;
    let uses = row.get::<_, i64>("uses_remaining")?;
    let expires_at = row.get::<_, String>("expires_at")?;
    let responded_by = row.get::<_, Option<String>>("responded_by")?;
    let responded_at = row.get::<_, Option<String>>("responded_at")?;
    let scope_raw = row.get::<_, String>("scope_json")?;
    // `created_at` is written and never edited; reading it belongs to audit queries, not
    // to the aggregate (the Approval entity carries no creation column). The DDL's
    // `created_at` column remains the audit source (ATH-HITL-006).

    Ok((|| {
        Ok(Approval {
            id: ApprovalId::parse(id.clone()).ok_or_else(|| PersistenceError::Decode {
                entity: "approval",
                column: "id",
                detail: format!("malformed identifier {id:?}"),
            })?,
            task_id: TaskId::parse(task_id.clone()).ok_or_else(|| PersistenceError::Decode {
                entity: "approval",
                column: "task_id",
                detail: format!("malformed identifier {task_id:?}"),
            })?,
            requested_by: AgentId::new(requested_by),
            operation,
            reason,
            impact,
            risk_level: decode_enum(
                "approval",
                "risk_level",
                &risk_raw,
                RiskLevel::ALL,
                RiskLevel::as_str,
            )?,
            state: decode_enum(
                "approval",
                "state",
                &state_raw,
                ApprovalState::ALL,
                ApprovalState::as_str,
            )?,
            operation_descriptor: descriptor,
            requested_by_instance_id: InstanceId::parse(requested_by_instance_id.clone())
                .ok_or_else(|| PersistenceError::Decode {
                    entity: "approval",
                    column: "requested_by_instance_id",
                    detail: format!("malformed identifier {requested_by_instance_id:?}"),
                })?,
            uses_remaining: u64::try_from(uses).unwrap_or(1),
            expires_at,
            responded_by,
            responded_at,
            scope: serde_json::from_str(&scope_raw).map_err(|e| PersistenceError::Decode {
                entity: "approval",
                column: "scope_json",
                detail: e.to_string(),
            })?,
        })
    })())
}

// --- Memory repository (P1-05 row) -------------------------------------------------------

impl MemoryRepository for SqliteStore {
    fn insert(&mut self, record: &MemoryRecord) -> Result<()> {
        self.db
            .connection()
            .execute(
                "INSERT INTO memory_records (id, tier, scope_id, content, summary,
                 origin_task_id, origin_event_id, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    record.id.as_str(),
                    record.tier.as_str(),
                    record.scope_id,
                    record.content,
                    record.summary,
                    record.origin.task_id.as_ref().map(TaskId::as_str),
                    record.origin.event_id.as_ref().map(EventId::as_str),
                    record.created_at,
                    record.expires_at,
                ],
            )
            .map(|_| ())
            .map_err(|e| {
                if is_unique_violation(&e) {
                    PersistenceError::Conflict {
                        detail: format!("memory record {} exists", record.id.as_str()),
                    }
                } else {
                    PersistenceError::Backend(e)
                }
            })
    }

    fn get(&self, id: &MemoryId) -> Result<Option<MemoryRecord>> {
        self.db
            .connection()
            .query_row(
                "SELECT id, tier, scope_id, content, summary, origin_task_id, origin_event_id,
                     created_at, expires_at
                 FROM memory_records WHERE id = ?1",
                params![id.as_str()],
                memory_from_row,
            )
            .optional()?
            .transpose()
    }

    fn list(&self, tier: MemoryTier, scope_id: &str) -> Result<Vec<MemoryRecord>> {
        let conn = self.db.connection();
        let mut statement = conn.prepare(
            "SELECT id, tier, scope_id, content, summary, origin_task_id, origin_event_id,
                 created_at, expires_at
             FROM memory_records WHERE tier = ?1 AND scope_id = ?2
             ORDER BY created_at, id",
        )?;
        let rows = statement.query_map(params![tier.as_str(), scope_id], memory_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    fn list_by_tier(&self, tier: MemoryTier) -> Result<Vec<MemoryRecord>> {
        let conn = self.db.connection();
        let mut statement = conn.prepare(
            "SELECT id, tier, scope_id, content, summary, origin_task_id, origin_event_id,
                 created_at, expires_at
             FROM memory_records WHERE tier = ?1 ORDER BY created_at, id",
        )?;
        let rows = statement.query_map(params![tier.as_str()], memory_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    fn count(&self, tier: MemoryTier, scope_id: &str) -> Result<u64> {
        let count: i64 = self.db.connection().query_row(
            "SELECT COUNT(*) FROM memory_records WHERE tier = ?1 AND scope_id = ?2",
            params![tier.as_str(), scope_id],
            |row| row.get(0),
        )?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    fn delete(&mut self, id: &MemoryId) -> Result<()> {
        // The caller records the deletion as an Event (ATH-MEM-006).
        self.db.connection().execute(
            "DELETE FROM memory_records WHERE id = ?1",
            params![id.as_str()],
        )?;
        Ok(())
    }

    fn summarize(&mut self, id: &MemoryId, summary: &str) -> Result<()> {
        let touched = self.db.connection().execute(
            "UPDATE memory_records SET summary = ?2 WHERE id = ?1",
            params![id.as_str(), summary],
        )?;
        if touched == 0 {
            return Err(PersistenceError::NotFound {
                entity: "memory_record",
                id: id.as_str().into(),
            });
        }
        Ok(())
    }

    fn list_by_provenance(
        &self,
        task: Option<&TaskId>,
        event: Option<&EventId>,
    ) -> Result<Vec<MemoryRecord>> {
        let mut statement = self.db.connection().prepare(
            "SELECT id, tier, scope_id, content, summary, origin_task_id, origin_event_id, \
             created_at, expires_at FROM memory_records
             WHERE (?1 IS NULL OR origin_task_id = ?1)
               AND (?2 IS NULL OR origin_event_id = ?2)
             ORDER BY created_at, id",
        )?;
        let rows = statement
            .query_map(
                params![task.map(|id| id.as_str()), event.map(|id| id.as_str()),],
                memory_from_row,
            )
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())?;
        rows.into_iter().collect()
    }
}

fn memory_from_row(row: &Row<'_>) -> rusqlite::Result<Result<MemoryRecord>> {
    let id = row.get::<_, String>("id")?;
    let tier_raw = row.get::<_, String>("tier")?;
    let scope_id = row.get::<_, String>("scope_id")?;
    let content = row.get::<_, String>("content")?;
    let summary = row.get::<_, Option<String>>("summary")?;
    let origin_task = row.get::<_, Option<String>>("origin_task_id")?;
    let origin_event = row.get::<_, Option<String>>("origin_event_id")?;
    let created_at = row.get::<_, String>("created_at")?;
    let expires_at = row.get::<_, Option<String>>("expires_at")?;

    Ok((|| {
        Ok(MemoryRecord {
            id: MemoryId::parse(id.clone()).ok_or_else(|| PersistenceError::Decode {
                entity: "memory_record",
                column: "id",
                detail: format!("malformed identifier {id:?}"),
            })?,
            tier: decode_enum(
                "memory_record",
                "tier",
                &tier_raw,
                MemoryTier::ALL,
                MemoryTier::as_str,
            )?,
            scope_id,
            content,
            summary,
            origin: MemoryOrigin {
                task_id: origin_task.and_then(TaskId::parse),
                event_id: origin_event.and_then(EventId::parse),
            },
            created_at,
            expires_at,
        })
    })())
}

/// The §23.2 `TaskCreated` payload digest (SHA-256 first 16 bytes hex).
pub(crate) fn digest_of(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(text.trim().as_bytes());
    let fine = hasher.finalize();
    let mut out = String::with_capacity(32);
    for byte in fine.iter().take(16) {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

// --- Message / MessageBroker repository (P2-11/12) --------------------------------------

impl MessageRepository for SqliteStore {
    fn insert(&mut self, message: &MessageRow) -> Result<()> {
        self.db
            .connection()
            .execute(
                "INSERT INTO messages (message_id, message_type, from_agent, to_agent,
                 to_instance_id, from_instance_id, task_id, thread_id, reply_to, priority,
                 body, payload_json, deadline, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    message.message_id.as_str(),
                    message.message_type.as_str(),
                    message.from_agent.as_str(),
                    message.to_agent.as_str(),
                    message.to_instance_id.as_ref().map(InstanceId::as_str),
                    message.from_instance_id.as_str(),
                    message.task_id.as_ref().map(TaskId::as_str),
                    message.thread_id,
                    message
                        .reply_to
                        .as_ref()
                        .map(athena_types::MessageId::as_str),
                    message.priority.as_str(),
                    message.body,
                    message.payload_json,
                    message.deadline,
                    message.created_at,
                ],
            )
            .map(|_| ())
            .map_err(|e| {
                if is_unique_violation(&e) {
                    PersistenceError::Conflict {
                        detail: format!("message {} exists", message.message_id.as_str()),
                    }
                } else {
                    PersistenceError::Backend(e)
                }
            })
    }

    fn get(&self, id: &MessageId) -> Result<Option<MessageRow>> {
        let row = self
            .db
            .connection()
            .query_row(
                "SELECT message_id, message_type, from_agent, to_agent, to_instance_id,
                     from_instance_id, task_id, thread_id, reply_to, priority, body,
                     payload_json, deadline, created_at
                 FROM messages WHERE message_id = ?1",
                params![id.as_str()],
                message_row_from,
            )
            .optional()?;
        row.transpose()
    }

    fn list_for_thread(&self, thread_id: &str) -> Result<Vec<MessageRow>> {
        let conn = self.db.connection();
        let mut statement = conn.prepare(
            "SELECT message_id, message_type, from_agent, to_agent, to_instance_id,
                 from_instance_id, task_id, thread_id, reply_to, priority, body,
                 payload_json, deadline, created_at
             FROM messages WHERE thread_id = ?1 ORDER BY created_at, message_id",
        )?;
        let rows = statement.query_map(params![thread_id], message_row_from)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect()
    }

    fn mark_delivered(&mut self, to: &InstanceId, id: &MessageId) -> Result<bool> {
        let inserted = self.db.connection().execute(
            "INSERT INTO message_deliveries (to_instance_id, message_id, delivered_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT DO NOTHING",
            params![to.as_str(), id.as_str(), crate::db::now_rfc3339()],
        )?;
        Ok(inserted == 1)
    }

    fn mark_accepted(&mut self, to: &InstanceId, id: &MessageId) -> Result<()> {
        self.db.connection().execute(
            "UPDATE message_deliveries SET accepted_at = ?3
             WHERE to_instance_id = ?1 AND message_id = ?2",
            params![to.as_str(), id.as_str(), crate::db::now_rfc3339()],
        )?;
        Ok(())
    }

    fn was_delivered(&self, to: &InstanceId, id: &MessageId) -> Result<bool> {
        let seen: i64 = self.db.connection().query_row(
            "SELECT EXISTS(SELECT 1 FROM message_deliveries
             WHERE to_instance_id = ?1 AND message_id = ?2)",
            params![to.as_str(), id.as_str()],
            |row| row.get(0),
        )?;
        Ok(seen == 1)
    }

    fn prune_deliveries_before(&mut self, cutoff_rfc3339: &str) -> Result<u64> {
        let removed = self.db.connection().execute(
            "DELETE FROM message_deliveries WHERE delivered_at < ?1",
            params![cutoff_rfc3339],
        )?;
        Ok(removed as u64)
    }
}

fn message_row_from(row: &Row<'_>) -> rusqlite::Result<Result<MessageRow>> {
    let id = row.get::<_, String>("message_id")?;
    let type_raw = row.get::<_, String>("message_type")?;
    let from_agent = row.get::<_, String>("from_agent")?;
    let to_agent = row.get::<_, String>("to_agent")?;
    let to_instance = row.get::<_, Option<String>>("to_instance_id")?;
    let from_instance = row.get::<_, String>("from_instance_id")?;
    let task_id = row.get::<_, Option<String>>("task_id")?;
    let thread_id = row.get::<_, String>("thread_id")?;
    let reply_to = row.get::<_, Option<String>>("reply_to")?;
    let priority_raw = row.get::<_, String>("priority")?;
    let body = row.get::<_, String>("body")?;
    let payload = row.get::<_, Option<String>>("payload_json")?;
    let deadline = row.get::<_, Option<String>>("deadline")?;
    let created_at = row.get::<_, String>("created_at")?;

    Ok((|| {
        let message_type: MessageType = decode_enum(
            "message",
            "message_type",
            &type_raw,
            MessageType::ALL,
            MessageType::as_str,
        )?;
        let priority: Priority = decode_enum(
            "message",
            "priority",
            &priority_raw,
            Priority::ALL,
            Priority::as_str,
        )?;
        Ok(MessageRow {
            message_id: MessageId::parse(id.clone()).ok_or_else(|| PersistenceError::Decode {
                entity: "message",
                column: "message_id",
                detail: format!("malformed identifier {id:?}"),
            })?,
            message_type,
            from_agent: AgentId::new(from_agent),
            to_agent: AgentId::new(to_agent),
            to_instance_id: to_instance.and_then(InstanceId::parse),
            from_instance_id: InstanceId::parse(from_instance.clone()).ok_or_else(|| {
                PersistenceError::Decode {
                    entity: "message",
                    column: "from_instance_id",
                    detail: format!("malformed identifier {from_instance:?}"),
                }
            })?,
            task_id: task_id.and_then(TaskId::parse),
            thread_id,
            reply_to: reply_to.and_then(athena_types::MessageId::parse),
            priority,
            body,
            payload_json: payload,
            deadline,
            created_at,
        })
    })())
}

// --- ToolExecutionRepository (P4-02) ------------------------------------------------------

impl ToolExecutionRepository for SqliteStore {
    fn insert(&mut self, execution: &ToolExecution) -> Result<()> {
        self.db
            .connection()
            .execute(
                "INSERT INTO tool_executions (id, instance_id, task_id, tool,
                 operation_descriptor, risk_level, approval_id, status, started_at,
                 finished_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    execution.id.as_str(),
                    execution.instance_id.as_str(),
                    execution.task_id.as_ref().map(|id| id.as_str()),
                    execution.tool.as_str(),
                    execution.operation_descriptor,
                    execution.risk_level.as_str(),
                    execution.approval_id.as_ref().map(|id| id.as_str()),
                    execution.status.as_str(),
                    execution.started_at.clone(),
                    execution.finished_at.clone(),
                ],
            )
            .map(|_| ())
            .map_err(|e| {
                if is_unique_violation(&e) {
                    PersistenceError::Conflict {
                        detail: format!("tool execution {} exists", execution.id.as_str()),
                    }
                } else {
                    PersistenceError::Backend(e)
                }
            })
    }

    fn get(&self, id: &ToolExecutionId) -> Result<Option<ToolExecution>> {
        self.db
            .connection()
            .query_row(
                "SELECT id, instance_id, task_id, tool, operation_descriptor, risk_level,
                     approval_id, status, started_at, finished_at
                 FROM tool_executions WHERE id = ?1",
                params![id.as_str()],
                tool_execution_from_row,
            )
            .optional()?
            .transpose()
    }

    fn update_status(
        &mut self,
        id: &ToolExecutionId,
        status: athena_types::ToolExecutionStatus,
    ) -> Result<()> {
        let finished = match status {
            athena_types::ToolExecutionStatus::Started
            | athena_types::ToolExecutionStatus::Denied => None,
            _ => Some(crate::db::now_rfc3339()),
        };
        let changed = self.db.connection().execute(
            "UPDATE tool_executions SET status = ?2, finished_at = COALESCE(?3, finished_at)
             WHERE id = ?1",
            params![id.as_str(), status.as_str(), finished],
        )?;
        if changed == 0 {
            return Err(PersistenceError::NotFound {
                entity: "tool_execution",
                id: id.as_str().to_string(),
            });
        }
        Ok(())
    }

    fn list_for_instance(&self, instance: &InstanceId) -> Result<Vec<ToolExecution>> {
        let conn = self.db.connection();
        let mut statement = conn.prepare(
            "SELECT id, instance_id, task_id, tool, operation_descriptor, risk_level,
                 approval_id, status, started_at, finished_at
             FROM tool_executions WHERE instance_id = ?1 ORDER BY started_at, id",
        )?;
        let rows = statement.query_map(params![instance.as_str()], tool_execution_from_row)?;
        rows.map(|row| row.map_err(PersistenceError::Backend))
            .map(|decoded| decoded.and_then(|inner| inner))
            .collect::<Result<Vec<_>>>()
    }
}

fn tool_execution_from_row(row: &Row<'_>) -> rusqlite::Result<Result<ToolExecution>> {
    let id = row.get::<_, String>("id")?;
    let instance_id = row.get::<_, String>("instance_id")?;
    let task_id = row.get::<_, Option<String>>("task_id")?;
    let tool_raw = row.get::<_, String>("tool")?;
    let operation_descriptor = row.get::<_, String>("operation_descriptor")?;
    let risk_raw = row.get::<_, String>("risk_level")?;
    let approval_id = row.get::<_, Option<String>>("approval_id")?;
    let status_raw = row.get::<_, String>("status")?;
    let started_at = row.get::<_, String>("started_at")?;
    let finished_at = row.get::<_, Option<String>>("finished_at")?;

    Ok((|| {
        Ok(ToolExecution {
            id: ToolExecutionId::parse(id.clone()).ok_or_else(|| PersistenceError::Decode {
                entity: "tool_execution",
                column: "id",
                detail: format!("malformed identifier {id:?}"),
            })?,
            instance_id: InstanceId::parse(instance_id.clone()).ok_or_else(|| {
                PersistenceError::Decode {
                    entity: "tool_execution",
                    column: "instance_id",
                    detail: format!("malformed identifier {instance_id:?}"),
                }
            })?,
            task_id: task_id
                .map(|value| {
                    TaskId::parse(value.clone()).ok_or_else(|| PersistenceError::Decode {
                        entity: "tool_execution",
                        column: "task_id",
                        detail: format!("malformed identifier {value:?}"),
                    })
                })
                .transpose()?,
            tool: decode_enum(
                "tool_execution",
                "tool",
                &tool_raw,
                ToolName::ALL,
                ToolName::as_str,
            )?,
            operation_descriptor,
            risk_level: decode_enum(
                "tool_execution",
                "risk_level",
                &risk_raw,
                RiskLevel::ALL,
                RiskLevel::as_str,
            )?,
            approval_id: approval_id
                .map(|value| {
                    ApprovalId::parse(value.clone()).ok_or_else(|| PersistenceError::Decode {
                        entity: "tool_execution",
                        column: "approval_id",
                        detail: format!("malformed identifier {value:?}"),
                    })
                })
                .transpose()?,
            status: decode_enum(
                "tool_execution",
                "status",
                &status_raw,
                athena_types::ToolExecutionStatus::ALL,
                athena_types::ToolExecutionStatus::as_str,
            )?,
            started_at,
            finished_at,
        })
    })())
}

// --- Result repository (P4-10) -----------------------------------------------------------

impl ResultRepository for SqliteStore {
    fn insert(&mut self, result: &athena_types::ExecutionResult) -> Result<()> {
        let conn = self.db.connection();
        conn.execute(
            "INSERT INTO results (id, task_id, plan_step_id, agent_id, instance_id, status,
                 output, logs_ref, skills_used_json, model_provider, model_name,
                 started_at, finished_at, execution_time_ms, metadata_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                result.id.as_str(),
                result.task_id.as_str(),
                result.plan_step_id.as_ref().map(PlanStepId::as_str),
                result.agent_id.as_str(),
                result.instance_id.as_str(),
                result.status.as_str(),
                result.output,
                result.logs_ref,
                serde_json::to_string(&result.skills_used).expect("skill refs serialize"),
                result.model.as_ref().map(|m| m.provider.clone()),
                result.model.as_ref().map(|m| m.name.clone()),
                result.started_at,
                result.finished_at,
                i64::try_from(result.execution_time_ms).unwrap_or(0),
                result.metadata.to_string(),
            ],
        )?;
        for artifact in &result.artifacts {
            conn.execute(
                "INSERT INTO artifacts (result_id, base, path, kind, bytes, checksum,
                 created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    result.id.as_str(),
                    artifact.base.as_str(),
                    artifact.path,
                    artifact.kind.as_str(),
                    artifact.bytes.and_then(|b| i64::try_from(b).ok()),
                    artifact.checksum,
                    artifact.created_at,
                ],
            )?;
        }
        Ok(())
    }

    fn get(&self, id: &ResultId) -> Result<Option<athena_types::ExecutionResult>> {
        self.db
            .connection()
            .query_row(
                "SELECT id, task_id, plan_step_id, agent_id, instance_id, status, output,
                     logs_ref, skills_used_json, model_provider, model_name,
                     started_at, finished_at, execution_time_ms, metadata_json
                 FROM results WHERE id = ?1",
                params![id.as_str()],
                execution_result_from_row,
            )
            .optional()?
            .transpose()
    }

    fn list_for_task(&self, task: &TaskId) -> Result<Vec<athena_types::ExecutionResult>> {
        let conn = self.db.connection();
        let mut statement = conn.prepare(
            "SELECT id, task_id, plan_step_id, agent_id, instance_id, status, output,
                 logs_ref, skills_used_json, model_provider, model_name,
                 started_at, finished_at, execution_time_ms, metadata_json
             FROM results WHERE task_id = ?1 ORDER BY started_at, id",
        )?;
        let rows = statement.query_map(params![task.as_str()], execution_result_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()
    }

    fn list_artifacts(&self, result: &ResultId) -> Result<Vec<athena_types::Artifact>> {
        let conn = self.db.connection();
        let mut statement = conn.prepare(
            "SELECT result_id, base, path, kind, bytes, checksum, created_at
             FROM artifacts WHERE result_id = ?1 ORDER BY base, path",
        )?;
        let rows = statement.query_map(params![result.as_str()], artifact_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()
    }

    fn insert_evaluation(&mut self, evaluation: &athena_types::Evaluation) -> Result<()> {
        self.db.connection().execute(
            "INSERT INTO evaluations (id, task_id, result_id, evaluator_json, method,
                 verdict, rationale, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                evaluation.id.as_str(),
                evaluation.task_id.as_str(),
                evaluation.result_id.as_str(),
                serde_json::to_string(&evaluation.evaluator).expect("evaluator serializes"),
                evaluation.method,
                evaluation.verdict.as_str(),
                evaluation.rationale,
                evaluation.created_at,
            ],
        )?;
        Ok(())
    }

    fn get_evaluation(&self, id: &EvaluationId) -> Result<Option<athena_types::Evaluation>> {
        self.db
            .connection()
            .query_row(
                "SELECT id, task_id, result_id, evaluator_json, method, verdict,
                     rationale, created_at
                 FROM evaluations WHERE id = ?1",
                params![id.as_str()],
                evaluation_from_row,
            )
            .optional()?
            .transpose()
    }

    fn list_evaluations_for_result(
        &self,
        result: &ResultId,
    ) -> Result<Vec<athena_types::Evaluation>> {
        let conn = self.db.connection();
        let mut statement = conn.prepare(
            "SELECT id, task_id, result_id, evaluator_json, method, verdict,
                 rationale, created_at
             FROM evaluations WHERE result_id = ?1 ORDER BY created_at, id",
        )?;
        let rows = statement.query_map(params![result.as_str()], evaluation_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()
    }
}

fn artifact_from_row(row: &Row<'_>) -> rusqlite::Result<Result<Artifact>> {
    let result_id = row.get::<_, String>("result_id")?;
    let base_raw = row.get::<_, String>("base")?;
    let kind_raw = row.get::<_, String>("kind")?;
    let bytes = row.get::<_, Option<i64>>("bytes")?;
    Ok((|| {
        Ok(Artifact {
            result_id: ResultId::parse(result_id.clone()).ok_or_else(|| {
                PersistenceError::Decode {
                    entity: "artifact",
                    column: "result_id",
                    detail: format!("malformed identifier {result_id:?}"),
                }
            })?,
            kind: decode_enum(
                "artifact",
                "kind",
                &kind_raw,
                ArtifactKind::ALL,
                ArtifactKind::as_str,
            )?,
            base: decode_enum(
                "artifact",
                "base",
                &base_raw,
                ArtifactBase::ALL,
                ArtifactBase::as_str,
            )?,
            path: row.get("path")?,
            bytes: bytes.map(u64::try_from).and_then(|r| r.ok()),
            checksum: row.get("checksum")?,
            created_at: row.get("created_at")?,
        })
    })())
}

fn evaluation_from_row(row: &Row<'_>) -> rusqlite::Result<Result<Evaluation>> {
    let id = row.get::<_, String>("id")?;
    let task_id = row.get::<_, String>("task_id")?;
    let result_id = row.get::<_, String>("result_id")?;
    let evaluator_raw = row.get::<_, String>("evaluator_json")?;
    let verdict_raw = row.get::<_, String>("verdict")?;
    let created_at = row.get::<_, String>("created_at")?;
    Ok((|| {
        Ok(Evaluation {
            id: EvaluationId::parse(id.clone()).ok_or_else(|| PersistenceError::Decode {
                entity: "evaluation",
                column: "id",
                detail: format!("malformed identifier {id:?}"),
            })?,
            task_id: TaskId::parse(task_id.clone()).ok_or_else(|| PersistenceError::Decode {
                entity: "evaluation",
                column: "task_id",
                detail: format!("malformed identifier {task_id:?}"),
            })?,
            result_id: ResultId::parse(result_id.clone()).ok_or_else(|| {
                PersistenceError::Decode {
                    entity: "evaluation",
                    column: "result_id",
                    detail: format!("malformed identifier {result_id:?}"),
                }
            })?,
            evaluator: serde_json::from_str(&evaluator_raw).map_err(|e| {
                PersistenceError::Decode {
                    entity: "evaluation",
                    column: "evaluator_json",
                    detail: e.to_string(),
                }
            })?,
            method: row.get("method")?,
            verdict: decode_enum(
                "evaluation",
                "verdict",
                &verdict_raw,
                Verdict::ALL,
                Verdict::as_str,
            )?,
            rationale: row.get("rationale")?,
            created_at,
        })
    })())
}

fn execution_result_from_row(
    row: &Row<'_>,
) -> rusqlite::Result<Result<athena_types::ExecutionResult>> {
    let id = row.get::<_, String>("id")?;
    let task_id = row.get::<_, String>("task_id")?;
    let plan_step_id = row.get::<_, Option<String>>("plan_step_id")?;
    let agent_id = row.get::<_, String>("agent_id")?;
    let instance_id = row.get::<_, String>("instance_id")?;
    let status_raw = row.get::<_, String>("status")?;
    let output = row.get::<_, String>("output")?;
    let logs_ref = row.get::<_, Option<String>>("logs_ref")?;
    let skills_raw = row.get::<_, String>("skills_used_json")?;
    let model_provider = row.get::<_, Option<String>>("model_provider")?;
    let model_name = row.get::<_, Option<String>>("model_name")?;
    let started_at = row.get::<_, String>("started_at")?;
    let finished_at = row.get::<_, String>("finished_at")?;
    let ms = row.get::<_, i64>("execution_time_ms")?;
    let metadata_raw = row.get::<_, String>("metadata_json")?;

    Ok((|| {
        Ok(athena_types::ExecutionResult {
            id: ResultId::parse(id.clone()).ok_or_else(|| PersistenceError::Decode {
                entity: "result",
                column: "id",
                detail: format!("malformed identifier {id:?}"),
            })?,
            task_id: TaskId::parse(task_id.clone()).ok_or_else(|| PersistenceError::Decode {
                entity: "result",
                column: "task_id",
                detail: format!("malformed identifier {task_id:?}"),
            })?,
            plan_step_id: plan_step_id
                .map(|value| {
                    PlanStepId::parse(value.clone()).ok_or_else(|| PersistenceError::Decode {
                        entity: "result",
                        column: "plan_step_id",
                        detail: format!("malformed identifier {value:?}"),
                    })
                })
                .transpose()?,
            agent_id: AgentId::new(agent_id),
            instance_id: InstanceId::parse(instance_id.clone()).ok_or_else(|| {
                PersistenceError::Decode {
                    entity: "result",
                    column: "instance_id",
                    detail: format!("malformed identifier {instance_id:?}"),
                }
            })?,
            status: decode_enum(
                "result",
                "status",
                &status_raw,
                athena_types::ResultStatus::ALL,
                athena_types::ResultStatus::as_str,
            )?,
            output,
            artifacts: Vec::new(),
            logs_ref,
            skills_used: serde_json::from_str(&skills_raw).map_err(|e| {
                PersistenceError::Decode {
                    entity: "result",
                    column: "skills_used_json",
                    detail: e.to_string(),
                }
            })?,
            model: model_provider
                .zip(model_name)
                .map(|(provider, name)| athena_types::ModelRef { provider, name }),
            started_at,
            finished_at,
            execution_time_ms: u64::try_from(ms).unwrap_or(0),
            metadata: serde_json::from_str(&metadata_raw).map_err(|e| {
                PersistenceError::Decode {
                    entity: "result",
                    column: "metadata_json",
                    detail: e.to_string(),
                }
            })?,
        })
    })())
}

// --- Clarification repository (P4-18) ----------------------------------------------------

impl ClarificationRepository for SqliteStore {
    fn open(&mut self, clarification: &athena_types::Clarification) -> Result<()> {
        self.db.connection().execute(
            "INSERT INTO clarifications (id, task_id, state, question, answer,
                 expires_at, created_at, answered_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                clarification.id.as_str(),
                clarification.task_id.as_str(),
                clarification.state.as_str(),
                clarification.question,
                clarification.answer,
                clarification.expires_at,
                clarification.created_at,
                clarification.answered_at,
            ],
        )?;
        Ok(())
    }

    fn get(&self, id: &ClarificationId) -> Result<Option<athena_types::Clarification>> {
        self.db
            .connection()
            .query_row(
                "SELECT id, task_id, state, question, answer, expires_at, created_at, answered_at
                 FROM clarifications WHERE id = ?1",
                params![id.as_str()],
                clarification_from_row,
            )
            .optional()?
            .transpose()
    }

    fn pending_for_task(&self, task: &TaskId) -> Result<Vec<athena_types::Clarification>> {
        let conn = self.db.connection();
        let mut statement = conn.prepare(
            "SELECT id, task_id, state, question, answer, expires_at, created_at, answered_at
             FROM clarifications WHERE task_id = ?1 AND state = 'requested'
             ORDER BY created_at, id",
        )?;
        let rows = statement.query_map(params![task.as_str()], clarification_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()
    }

    fn pending_expired(&self, now_rfc3339: &str) -> Result<Vec<athena_types::Clarification>> {
        let conn = self.db.connection();
        let mut statement = conn.prepare(
            "SELECT id, task_id, state, question, answer, expires_at, created_at, answered_at
             FROM clarifications WHERE state = 'requested' AND expires_at <= ?1
             ORDER BY expires_at, id",
        )?;
        let rows = statement.query_map(params![now_rfc3339], clarification_from_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .collect::<Result<Vec<_>>>()
    }

    fn mark_expired(&mut self, id: &ClarificationId) -> Result<()> {
        let changed = self.db.connection().execute(
            "UPDATE clarifications SET state = 'expired' WHERE id = ?1 AND state = 'requested'",
            params![id.as_str()],
        )?;
        if changed == 0 {
            return Err(PersistenceError::NotFound {
                entity: "clarification",
                id: id.as_str().to_string(),
            });
        }
        Ok(())
    }

    fn answer(&mut self, id: &ClarificationId, answer: &str, answered_at: &str) -> Result<()> {
        // A non-`requested` row refuses: the answer that lands late (past the sweep)
        // cannot be read as answering anything.
        let changed = self.db.connection().execute(
            "UPDATE clarifications SET state = 'answered', answer = ?2, answered_at = ?3
             WHERE id = ?1 AND state = 'requested'",
            params![id.as_str(), answer, answered_at],
        )?;
        if changed == 0 {
            return Err(PersistenceError::NotFound {
                entity: "clarification",
                id: id.as_str().to_string(),
            });
        }
        Ok(())
    }
}

fn clarification_from_row(row: &Row<'_>) -> rusqlite::Result<Result<Clarification>> {
    let id = row.get::<_, String>("id")?;
    let task_id = row.get::<_, String>("task_id")?;
    let state_raw = row.get::<_, String>("state")?;
    Ok((|| {
        Ok(Clarification {
            id: ClarificationId::parse(id.clone()).ok_or_else(|| PersistenceError::Decode {
                entity: "clarification",
                column: "id",
                detail: format!("malformed identifier {id:?}"),
            })?,
            task_id: TaskId::parse(task_id.clone()).ok_or_else(|| PersistenceError::Decode {
                entity: "clarification",
                column: "task_id",
                detail: format!("malformed identifier {task_id:?}"),
            })?,
            state: decode_enum(
                "clarification",
                "state",
                &state_raw,
                ClarificationState::ALL,
                ClarificationState::as_str,
            )?,
            question: row.get("question")?,
            answer: row.get("answer")?,
            expires_at: row.get("expires_at")?,
            created_at: row.get("created_at")?,
            answered_at: row.get("answered_at")?,
        })
    })())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::now_rfc3339;

    fn store() -> SqliteStore {
        SqliteStore::in_memory().expect("in-memory store")
    }

    pub(super) fn sample_task(state: TaskState) -> Task {
        Task {
            id: TaskId::new(),
            parent_id: None,
            title: "FPGA timing fix".into(),
            instruction: "Analyse the timing violation and propose a fix.".into(),
            workspace: "/work/fpga-project".into(),
            goal: None,
            constraints: vec!["do not update the production bitstream".into()],
            completion_criteria: vec!["root cause identified".into()],
            state,
            priority: Priority::High,
            plan_id: None,
            risk_level: RiskLevel::Medium,
            attempt_count: 0,
            max_attempts: 3,
            iteration_count: 0,
            max_iterations: 50,
            deadline: None,
            created_by: "hidemi".into(),
            created_at: now_rfc3339(),
            updated_at: now_rfc3339(),
            finished_at: None,
        }
    }

    fn sample_event(task: &TaskId, event_type: &str) -> NewEvent {
        NewEvent {
            event_id: EventId::new(),
            schema_version: 1,
            event_type: event_type.into(),
            occurred_at: now_rfc3339(),
            task_id: Some(task.clone()),
            plan_step_id: None,
            agent_id: None,
            instance_id: None,
            skill_id: None,
            correlation_id: "cor-test".into(),
            causation_id: None,
            actor: "system".into(),
            payload_json: "{}".into(),
        }
    }

    #[test]
    fn a_task_round_trips_with_every_field_preserved() {
        let mut s = store();
        let task = sample_task(TaskState::Created);
        TaskRepository::insert(&mut s, &task).expect("insert");

        let back = TaskRepository::get(&s, &task.id)
            .expect("get")
            .expect("present");
        assert_eq!(back, task);
    }

    #[test]
    fn the_original_instruction_is_never_rewritten_by_an_update() {
        // ATH-DATA-004: the human's text is preserved verbatim.
        let mut s = store();
        let task = sample_task(TaskState::Created);
        TaskRepository::insert(&mut s, &task).expect("insert");

        let mut edited = task.clone();
        edited.instruction = "a summary that must not replace the original".into();
        edited.title = "new title".into();
        s.update(&edited).expect("update");

        let back = TaskRepository::get(&s, &task.id)
            .expect("get")
            .expect("present");
        assert_eq!(
            back.instruction, task.instruction,
            "instruction must be immutable"
        );
        assert_eq!(back.title, "new title", "other fields do update");
    }

    #[test]
    fn inserting_the_same_task_twice_is_a_conflict() {
        let mut s = store();
        let task = sample_task(TaskState::Created);
        TaskRepository::insert(&mut s, &task).expect("insert");
        let err = TaskRepository::insert(&mut s, &task).expect_err("must conflict");
        assert!(matches!(err, PersistenceError::Conflict { .. }), "{err}");
    }

    #[test]
    fn non_terminal_tasks_are_what_recovery_restores() {
        // ATH-NFR-003: a restart reloads exactly these.
        let mut s = store();
        for state in [
            TaskState::Created,
            TaskState::Queued,
            TaskState::Running,
            TaskState::Success,
            TaskState::Failed,
            TaskState::Cancelled,
        ] {
            TaskRepository::insert(&mut s, &sample_task(state)).expect("insert");
        }
        let restored = s.list_non_terminal().expect("list");
        assert_eq!(restored.len(), 3);
        for task in restored {
            assert!(!task.state.is_terminal(), "{} is terminal", task.state);
        }
    }

    #[test]
    fn a_transition_writes_the_state_and_its_event_together() {
        // ATH-PERS-003: both or neither.
        let mut s = store();
        let task = sample_task(TaskState::Created);
        TaskRepository::insert(&mut s, &task).expect("insert");

        let event = sample_event(&task.id, "TaskQueued");
        let stored = s
            .transition_task(&task.id, TaskState::Created, TaskState::Queued, &event)
            .expect("transition");

        assert_eq!(stored.sequence, 1);
        let back = TaskRepository::get(&s, &task.id)
            .expect("get")
            .expect("present");
        assert_eq!(back.state, TaskState::Queued);
        let events = EventRepository::list_for_task(&s, &task.id).expect("events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "TaskQueued");
    }

    #[test]
    fn a_stale_expected_state_refuses_the_transition_and_writes_nothing() {
        // ATH-STATE-014: optimistic concurrency. The Event must not be appended either,
        // otherwise an Event would describe a transition that never happened.
        let mut s = store();
        let task = sample_task(TaskState::Running);
        TaskRepository::insert(&mut s, &task).expect("insert");

        let event = sample_event(&task.id, "TaskQueued");
        let err = s
            .transition_task(&task.id, TaskState::Created, TaskState::Queued, &event)
            .expect_err("must refuse");
        assert!(matches!(err, PersistenceError::Conflict { .. }), "{err}");

        let back = TaskRepository::get(&s, &task.id)
            .expect("get")
            .expect("present");
        assert_eq!(back.state, TaskState::Running, "state must be untouched");
        assert_eq!(
            s.max_sequence().expect("max"),
            0,
            "no event may have been appended"
        );
    }

    #[test]
    fn transitioning_an_unknown_task_is_not_found() {
        let mut s = store();
        let missing = TaskId::new();
        let event = sample_event(&missing, "TaskQueued");
        let err = s
            .transition_task(&missing, TaskState::Created, TaskState::Queued, &event)
            .expect_err("must fail");
        assert!(matches!(err, PersistenceError::NotFound { .. }), "{err}");
    }

    #[test]
    fn event_sequence_increases_monotonically() {
        // ATH-EVENT-003.
        let mut s = store();
        let task = sample_task(TaskState::Created);
        TaskRepository::insert(&mut s, &task).expect("insert");

        let mut sequences = Vec::new();
        for n in 0..5 {
            let stored = s
                .append(&sample_event(&task.id, &format!("Event{n}")))
                .expect("append");
            sequences.push(stored.sequence);
        }
        assert_eq!(sequences, vec![1, 2, 3, 4, 5]);
        assert_eq!(s.max_sequence().expect("max"), 5);
    }

    #[test]
    fn events_for_a_task_come_back_in_sequence_order() {
        // ATH-EVENT-004 / ATH-EVENT-012: replay must reproduce the real order.
        let mut s = store();
        let task = sample_task(TaskState::Created);
        TaskRepository::insert(&mut s, &task).expect("insert");
        for name in ["TaskCreated", "TaskQueued", "TaskStarted"] {
            s.append(&sample_event(&task.id, name)).expect("append");
        }
        let events = EventRepository::list_for_task(&s, &task.id).expect("list");
        let names: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
        assert_eq!(names, vec!["TaskCreated", "TaskQueued", "TaskStarted"]);
        assert!(events.windows(2).all(|w| w[0].sequence < w[1].sequence));
    }

    #[test]
    fn a_subscriber_can_fetch_what_it_missed_by_sequence() {
        // ATH-EVENT-020: reconnection must not lose Events.
        let mut s = store();
        let task = sample_task(TaskState::Created);
        TaskRepository::insert(&mut s, &task).expect("insert");
        for n in 0..6 {
            s.append(&sample_event(&task.id, &format!("E{n}")))
                .expect("append");
        }
        let missed = s.list_since(3, 10).expect("since");
        assert_eq!(missed.len(), 3);
        assert_eq!(missed[0].sequence, 4);
    }

    #[test]
    fn pruning_events_leaves_results_untouched() {
        // ATH-EVENT-014: a retention sweep must preserve the Task's Results.
        let mut s = store();
        let task = sample_task(TaskState::Success);
        TaskRepository::insert(&mut s, &task).expect("insert");
        s.append(&NewEvent {
            occurred_at: "2020-01-01T00:00:00.000Z".into(),
            ..sample_event(&task.id, "TaskCreated")
        })
        .expect("append old");
        s.append(&sample_event(&task.id, "TaskCompleted"))
            .expect("append new");

        s.database()
            .connection()
            .execute(
                "INSERT INTO results (id, task_id, agent_id, instance_id, status, output,
                     started_at, finished_at, execution_time_ms)
                 VALUES ('res-keep', ?1, 'a', 'ai-1', 'success', 'out',
                     '2020-01-01T00:00:00.000Z', '2020-01-01T00:00:01.000Z', 1000)",
                params![task.id.as_str()],
            )
            .expect("seed result");

        let removed =
            EventRepository::prune_before(&mut s, "2021-01-01T00:00:00.000Z").expect("prune");
        assert_eq!(removed, 1);
        assert_eq!(
            EventRepository::list_for_task(&s, &task.id)
                .expect("list")
                .len(),
            1
        );

        let results: i64 = s
            .database()
            .connection()
            .query_row("SELECT COUNT(*) FROM results", [], |r| r.get(0))
            .expect("count");
        assert_eq!(results, 1, "Results must survive the sweep");
    }

    #[test]
    fn an_idempotency_key_can_be_claimed_only_once() {
        // ATH-CORE-035 / ATH-NFR-017: this is the double-delegation guard.
        let mut s = store();
        assert!(s.claim("step-1:0").expect("first claim"));
        assert!(
            !s.claim("step-1:0").expect("second claim"),
            "must not win twice"
        );
        assert!(s.is_claimed("step-1:0").expect("is_claimed"));
        // A different attempt of the same step is a different key.
        assert!(s.claim("step-1:1").expect("other attempt"));
    }

    #[test]
    fn an_idempotent_repeat_can_return_the_recorded_outcome() {
        // ATH-API-004: the same key returns the same result.
        let mut s = store();
        s.claim("req-1").expect("claim");
        s.record_outcome("req-1", "{\"id\":\"task-1\"}")
            .expect("record");
        assert_eq!(
            s.outcome("req-1").expect("outcome").as_deref(),
            Some("{\"id\":\"task-1\"}")
        );
        assert!(s.outcome("req-unknown").expect("none").is_none());
    }

    #[test]
    fn a_workspace_lease_is_exclusive_and_releasable() {
        // ATH-DATA-009: a second Task on the same workspace is refused, which the API turns
        // into 409 (ATH-API-023).
        let mut s = store();
        let first = sample_task(TaskState::Running);
        let second = sample_task(TaskState::Created);
        TaskRepository::insert(&mut s, &first).expect("insert");
        TaskRepository::insert(&mut s, &second).expect("insert");

        assert!(s.acquire("/work/p", &first.id).expect("acquire"));
        assert!(
            !s.acquire("/work/p", &second.id).expect("second"),
            "must be refused"
        );
        assert_eq!(s.holder("/work/p").expect("holder"), Some(first.id.clone()));

        s.release("/work/p").expect("release");
        assert!(s.acquire("/work/p", &second.id).expect("after release"));
        assert_eq!(s.holder("/work/p").expect("holder"), Some(second.id));
    }

    #[test]
    fn a_plan_round_trips_with_its_steps_and_dependencies() {
        let mut s = store();
        let task = sample_task(TaskState::Running);
        TaskRepository::insert(&mut s, &task).expect("insert task");

        let plan_id = PlanId::new();
        let step_a = PlanStepId::new();
        let step_b = PlanStepId::new();
        let plan = Plan {
            id: plan_id.clone(),
            task_id: task.id.clone(),
            revision: 1,
            goal: "fix timing".into(),
            steps: vec![
                PlanStep {
                    id: step_a.clone(),
                    plan_id: plan_id.clone(),
                    order: 1,
                    intent: "analyse".into(),
                    depends_on: vec![],
                    assigned_agent: Some(AgentId::new("timing-lead")),
                    skill_set: vec![SkillRef {
                        id: SkillId::new("fpga-timing-analysis"),
                        version: "1.2.0".into(),
                        checksum: Some("sha256:abc".into()),
                    }],
                    state: PlanStepState::Pending,
                    result_id: None,
                },
                PlanStep {
                    id: step_b.clone(),
                    plan_id: plan_id.clone(),
                    order: 2,
                    intent: "fix".into(),
                    depends_on: vec![step_a.clone()],
                    assigned_agent: None,
                    skill_set: vec![],
                    state: PlanStepState::Pending,
                    result_id: None,
                },
            ],
            created_at: now_rfc3339(),
            superseded_by: None,
        };
        PlanRepository::insert(&mut s, &plan).expect("insert plan");

        let back = PlanRepository::get(&s, &plan_id)
            .expect("get")
            .expect("present");
        assert_eq!(back.revision, 1);
        assert_eq!(back.steps.len(), 2);
        assert_eq!(back.steps[1].depends_on, vec![step_a.clone()]);
        assert_eq!(back.steps[0].skill_set[0].version, "1.2.0");
    }

    #[test]
    fn a_duplicate_plan_revision_is_refused() {
        // ATH-PLAN-004: a revision is never overwritten.
        let mut s = store();
        let task = sample_task(TaskState::Running);
        TaskRepository::insert(&mut s, &task).expect("insert task");

        let make = |revision| Plan {
            id: PlanId::new(),
            task_id: task.id.clone(),
            revision,
            goal: "g".into(),
            steps: vec![],
            created_at: now_rfc3339(),
            superseded_by: None,
        };
        PlanRepository::insert(&mut s, &make(1)).expect("first");
        let err = PlanRepository::insert(&mut s, &make(1)).expect_err("must conflict");
        assert!(matches!(err, PersistenceError::Conflict { .. }), "{err}");
        // A later revision is accepted, and both are retained.
        PlanRepository::insert(&mut s, &make(2)).expect("second revision");
        assert_eq!(
            s.revisions_for_task(&task.id).expect("revisions"),
            vec![1, 2]
        );
    }

    #[test]
    fn the_latest_revision_is_the_highest_not_the_newest_row() {
        let mut s = store();
        let task = sample_task(TaskState::Running);
        TaskRepository::insert(&mut s, &task).expect("insert task");
        for revision in [1_u64, 3, 2] {
            PlanRepository::insert(
                &mut s,
                &Plan {
                    id: PlanId::new(),
                    task_id: task.id.clone(),
                    revision,
                    goal: format!("r{revision}"),
                    steps: vec![],
                    created_at: now_rfc3339(),
                    superseded_by: None,
                },
            )
            .expect("insert");
        }
        let latest = s
            .latest_for_task(&task.id)
            .expect("latest")
            .expect("present");
        assert_eq!(latest.revision, 3);
        let specific = s
            .revision_for_task(&task.id, 2)
            .expect("revision")
            .expect("present");
        assert_eq!(specific.goal, "r2");
    }

    #[test]
    fn each_attempt_is_recorded_once_and_history_is_never_overwritten() {
        // ATH-FAIL-007.
        let mut s = store();
        let task = sample_task(TaskState::Running);
        TaskRepository::insert(&mut s, &task).expect("insert task");
        let plan_id = PlanId::new();
        let step_id = PlanStepId::new();
        PlanRepository::insert(
            &mut s,
            &Plan {
                id: plan_id.clone(),
                task_id: task.id.clone(),
                revision: 1,
                goal: "g".into(),
                steps: vec![PlanStep {
                    id: step_id.clone(),
                    plan_id: plan_id.clone(),
                    order: 1,
                    intent: "do".into(),
                    depends_on: vec![],
                    assigned_agent: None,
                    skill_set: vec![],
                    state: PlanStepState::Ready,
                    result_id: None,
                }],
                created_at: now_rfc3339(),
                superseded_by: None,
            },
        )
        .expect("insert plan");

        let instance = InstanceId::new();
        s.record_attempt(&step_id, 0, Some(&instance))
            .expect("attempt 0");
        s.record_attempt(&step_id, 1, None).expect("attempt 1");
        let err = s
            .record_attempt(&step_id, 0, None)
            .expect_err("must conflict");
        assert!(matches!(err, PersistenceError::Conflict { .. }), "{err}");
    }

    #[test]
    fn a_step_state_update_is_visible_and_an_unknown_step_is_not_found() {
        let mut s = store();
        let task = sample_task(TaskState::Running);
        TaskRepository::insert(&mut s, &task).expect("insert task");
        let plan_id = PlanId::new();
        let step_id = PlanStepId::new();
        PlanRepository::insert(
            &mut s,
            &Plan {
                id: plan_id.clone(),
                task_id: task.id.clone(),
                revision: 1,
                goal: "g".into(),
                steps: vec![PlanStep {
                    id: step_id.clone(),
                    plan_id: plan_id.clone(),
                    order: 1,
                    intent: "do".into(),
                    depends_on: vec![],
                    assigned_agent: None,
                    skill_set: vec![],
                    state: PlanStepState::Pending,
                    result_id: None,
                }],
                created_at: now_rfc3339(),
                superseded_by: None,
            },
        )
        .expect("insert plan");

        s.update_step_state(&step_id, PlanStepState::Ready)
            .expect("update");
        let step = s.get_step(&step_id).expect("get").expect("present");
        assert_eq!(step.state, PlanStepState::Ready);

        let err = s
            .update_step_state(&PlanStepId::new(), PlanStepState::Ready)
            .expect_err("must fail");
        assert!(matches!(err, PersistenceError::NotFound { .. }), "{err}");
    }

    #[test]
    fn a_stored_enum_token_that_no_longer_parses_is_a_decode_error() {
        // Not bad input but an internal inconsistency: writer and reader disagree.
        let s = store();
        s.database()
            .connection()
            .execute(
                "INSERT INTO tasks (id, title, instruction, workspace, state, priority,
                     risk_level, max_attempts, max_iterations, created_by, created_at, updated_at)
                 VALUES ('task-weird', 't', 'i', '/w', 'NOT_A_STATE', 'normal', 'low', 3, 50,
                     'u', '2026-10-14T00:00:00.000Z', '2026-10-14T00:00:00.000Z')",
                [],
            )
            .expect("seed");
        let err = TaskRepository::get(&s, &TaskId::parse("task-weird").expect("id"))
            .expect_err("must fail to decode");
        assert!(matches!(err, PersistenceError::Decode { .. }), "{err}");
    }

    // --- ToolExecutionRepository (P4-02) ----------------------------------------------------

    fn sample_execution(task: Option<&TaskId>) -> ToolExecution {
        ToolExecution {
            id: ToolExecutionId::new(),
            instance_id: InstanceId::new(),
            task_id: task.cloned(),
            tool: ToolName::FsWrite,
            operation_descriptor: "fs_write(target=/repo/src/lib.rs)".into(),
            risk_level: RiskLevel::Medium,
            approval_id: None,
            status: athena_types::ToolExecutionStatus::Started,
            started_at: now_rfc3339(),
            finished_at: None,
        }
    }

    #[test]
    fn tool_executions_roundtrip_and_finish_with_their_outcomes() {
        let mut s = store();
        // task_id is a FK; create the referenced task row first.
        let task = sample_task(TaskState::Running);
        TaskRepository::insert(&mut s, &task).expect("task row");
        let started = sample_execution(Some(&task.id));
        ToolExecutionRepository::insert(&mut s, &started).expect("inserted");

        let read = ToolExecutionRepository::get(&s, &started.id)
            .expect("read")
            .expect("present");
        assert_eq!(read, started, "the aggregate survives the store round trip");

        // The instance listing answers the audit question only this table owns.
        let listed =
            ToolExecutionRepository::list_for_instance(&s, &started.instance_id).expect("listed");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].status, athena_types::ToolExecutionStatus::Started);

        // Terminal outcomes stamp finished_at; `Denied` never does (it never ran).
        ToolExecutionRepository::update_status(
            &mut s,
            &started.id,
            athena_types::ToolExecutionStatus::Succeeded,
        )
        .expect("updated");
        let finished = ToolExecutionRepository::get(&s, &started.id)
            .expect("read")
            .expect("present");
        assert_eq!(
            finished.status,
            athena_types::ToolExecutionStatus::Succeeded
        );
        assert!(
            finished.finished_at.is_some(),
            "the outcome stamps finished_at"
        );

        let denied = sample_execution(Some(&task.id));
        ToolExecutionRepository::insert(&mut s, &denied).expect("inserted");
        ToolExecutionRepository::update_status(
            &mut s,
            &denied.id,
            athena_types::ToolExecutionStatus::Denied,
        )
        .expect("updated");
        let denied_row = ToolExecutionRepository::get(&s, &denied.id)
            .expect("read")
            .expect("present");
        assert_eq!(denied_row.status, athena_types::ToolExecutionStatus::Denied);
        assert!(denied_row.finished_at.is_none(), "a denial never ran");
    }

    #[test]
    fn tool_execution_updates_report_missing_rows_and_decode_failures() {
        let mut s = store();
        let missing = ToolExecutionRepository::update_status(
            &mut s,
            &ToolExecutionId::new(),
            athena_types::ToolExecutionStatus::Failed,
        )
        .expect_err("no such row");
        assert!(
            matches!(missing, PersistenceError::NotFound { .. }),
            "{missing}"
        );

        // A corrupted wire token is a Decode failure, surfacing the store's disagreement,
        // never a guessed enum.
        s.database()
            .connection()
            .execute(
                "INSERT INTO tool_executions (id, instance_id, tool, operation_descriptor,
                     risk_level, status, started_at)
                 VALUES ('tx-0001Y2A4WZCJ8MD7P9E3F0VQ5XA2BNH6SU', 'inst-x', 'NOT_A_TOOL', 'd', 'low', 'started',
                     '2026-10-14T00:00:00.000Z')",
                [],
            )
            .expect("seed");
        let err = ToolExecutionRepository::get(
            &s,
            &ToolExecutionId::parse("tx-0001Y2A4WZCJ8MD7P9E3F0VQ5XA2BNH6SU").expect("id"),
        )
        .expect_err("must fail to decode");
        assert!(matches!(err, PersistenceError::Decode { .. }), "{err}");
    }
}

// The ClarificationRepository tests (P4-18) live at the file end too.
#[cfg(test)]
mod clarification_tests {
    use super::*;
    use crate::db::now_rfc3339;

    fn seed_task(s: &mut SqliteStore) -> TaskId {
        let task = crate::sqlite::tests::sample_task(TaskState::Running);
        TaskRepository::insert(s, &task).expect("task seeds");
        task.id
    }

    fn sample(task: &TaskId) -> Clarification {
        Clarification {
            id: ClarificationId::new(),
            task_id: task.clone(),
            state: ClarificationState::Requested,
            question: "which shape does the report take?".into(),
            answer: None,
            expires_at: "2026-10-18T00:00:00.000Z".into(),
            created_at: now_rfc3339(),
            answered_at: None,
        }
    }

    #[test]
    fn the_open_row_roundtrips_and_the_answer_never_lands_late() {
        let mut s = SqliteStore::in_memory().expect("store");
        let task = seed_task(&mut s);
        let clarification = sample(&task);
        ClarificationRepository::open(&mut s, &clarification).expect("opened");

        let read = ClarificationRepository::get(&s, &clarification.id)
            .expect("read")
            .expect("present");
        assert_eq!(read.state, ClarificationState::Requested);
        assert_eq!(read.question, clarification.question);

        // The Task's pending view answers (`ATH-HITL-019`'s surface).
        let pending = ClarificationRepository::pending_for_task(&s, &task).expect("pending");
        assert_eq!(pending.len(), 1);

        // The answer lands; a second answer for the same id refuses (it cannot be read
        // as answering anything twice) (`ATH-HITL-018`'s single-answer rule).
        ClarificationRepository::answer(
            &mut s,
            &clarification.id,
            "the one-page form",
            &now_rfc3339(),
        )
        .expect("answered");
        let answered = ClarificationRepository::get(&s, &clarification.id)
            .expect("read")
            .expect("present");
        assert_eq!(answered.state, ClarificationState::Answered);
        assert_eq!(answered.answer.as_deref(), Some("the one-page form"));
        assert!(answered.answered_at.is_some());
        assert!(
            ClarificationRepository::answer(&mut s, &clarification.id, "again", &now_rfc3339())
                .is_err(),
            "a late answer never answers"
        );
        assert!(
            ClarificationRepository::pending_for_task(&s, &task)
                .expect("pending")
                .is_empty()
        );
    }

    #[test]
    fn the_expiry_sweep_names_silence_never_an_answer() {
        let mut s = SqliteStore::in_memory().expect("store");
        let task = seed_task(&mut s);

        // A row whose deadline already passed: the sweep sees it as expired silence
        // (`ATH-HITL-018`: silence is never read as an answer).
        let mut expired = sample(&task);
        expired.expires_at = "2026-10-17T00:00:00.000Z".into();
        ClarificationRepository::open(&mut s, &expired).expect("opened");

        let swept = ClarificationRepository::pending_expired(&s, "2026-10-17T03:00:00.000Z")
            .expect("swept");
        assert_eq!(swept.len(), 1);
        assert_eq!(swept[0].id, expired.id);
        assert_eq!(
            swept[0].state,
            ClarificationState::Requested,
            "the sweep never fabricates an answer"
        );
        // And the Task side transition (ClarificationExpired → FAILED) rides the
        // transition tables (athena-state); this store fact is the sweep's input only.
    }
}

// The long-term maturation store tests (P8-03): the provenance query over the
// `origin_task_id` / `origin_event_id` columns (`ATH-MEM-005`) and the scoping
// separation (`ProjectId` scope per project).
#[cfg(test)]
mod memory_maturation_tests {
    use super::*;

    fn record(tier: MemoryTier, scope: &str, origin: MemoryOrigin) -> MemoryRecord {
        MemoryRecord {
            id: MemoryId::new(),
            tier,
            scope_id: scope.into(),
            content: format!(
                "content-{}",
                origin.task_id.as_ref().map_or("none", TaskId::as_str)
            ),
            summary: None,
            origin,
            created_at: "2026-10-18T00:00:00Z".into(),
            expires_at: None,
        }
    }

    #[test]
    fn the_provenance_query_answers_by_task_and_event_anchor() {
        let mut s = SqliteStore::in_memory().expect("store");
        let task = TaskId::new();
        let other = TaskId::new();
        let event = EventId::new();
        let anchored = record(
            MemoryTier::Long,
            "proj-a",
            MemoryOrigin {
                task_id: Some(task.clone()),
                event_id: Some(event.clone()),
            },
        );
        let task_only = record(
            MemoryTier::Long,
            "proj-a",
            MemoryOrigin {
                task_id: Some(task.clone()),
                event_id: None,
            },
        );
        let elsewhere = record(
            MemoryTier::Long,
            "proj-b",
            MemoryOrigin {
                task_id: Some(other.clone()),
                event_id: None,
            },
        );
        for record in [&anchored, &task_only, &elsewhere] {
            MemoryRepository::insert(&mut s, record).expect("inserted");
        }

        // By Task: both anchored rows, the unanchored separate project's not.
        let by_task = MemoryRepository::list_by_provenance(&s, Some(&task), None).expect("by task");
        assert_eq!(by_task.len(), 2);
        assert!(by_task.iter().all(|row| row.tier == MemoryTier::Long));

        // By Event: exactly the row anchored to it.
        let by_event =
            MemoryRepository::list_by_provenance(&s, None, Some(&event)).expect("by event");
        assert_eq!(by_event.len(), 1);
        assert_eq!(by_event[0].id, anchored.id);

        // By both: an intersection, not a union.
        let by_both =
            MemoryRepository::list_by_provenance(&s, Some(&task), Some(&event)).expect("by both");
        assert_eq!(by_both.len(), 1);
        let wrong_pair = MemoryRepository::list_by_provenance(&s, Some(&other), Some(&event))
            .expect("by wrong pair");
        assert!(wrong_pair.is_empty());

        // Working-scope records answer too when their origin matches (the query is
        // anchor-based, not tier-gated — callers scope the tier per use).
        let working = record(
            MemoryTier::Working,
            "ai-instance",
            MemoryOrigin {
                task_id: Some(other.clone()),
                event_id: None,
            },
        );
        MemoryRepository::insert(&mut s, &working).expect("inserted");
        let by_other =
            MemoryRepository::list_by_provenance(&s, Some(&other), None).expect("by other");
        assert_eq!(by_other.len(), 2);
    }
}
