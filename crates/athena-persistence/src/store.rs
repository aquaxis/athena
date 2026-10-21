//! The ATHENA store — `AthenaStore`, every Repository trait over one engine.
//!
//! The engine is [`redb`]: a pure-Rust, embedded, file-backed (or in-memory) ACID store.
//! **There is no SQL and there is no SQLite** — its use is prohibited by the instruction
//! source (最優先事項, 2026-10-18). Everything else about the Persistence design stands
//! exactly as specified:
//!
//! - the store is authoritative; in-memory state is derived (`ATH-PERS-002`);
//! - a state transition and its Event commit inside **one** atomic unit
//!   (`ATH-PERS-003`) — a single engine write transaction;
//! - the §27.3 logical data model is realized table for table (`crate::db::tables`,
//!   R-PRS-04), with the keys the query shapes name (`ATH-DATA`),
//! - `events` and `results` are append-only (`ATH-PERS-006` — no path in this module
//!   rewrites or deletes them except the retention sweep);
//! - uniqueness lives on the row's own key and surfaces as `Conflict` (409)
//!   (`ATH-DATA-009`, `ATH-FAIL-007`, `ATH-CORE-035`).
//!
//! A row is the full entity as JSON; identifiers are the row key. Composite keys are
//! written `first|second` with fixed-width numeric parts, so ordered scans stay ordered.

use crate::db::{Database, ReadTx, WriteTx, tables};
use crate::error::{PersistenceError, Result};
use crate::repository::MessageRow;
use crate::repository::{
    AgentRepository, ApprovalRepository, ClarificationRepository, ConversationRepository,
    EventRepository, HierarchyRepository, IdempotencyRepository, MemoryRepository,
    MessageRepository, NewEvent, PlanRepository, ResultRepository, RuntimeInstanceRepository,
    SkillRepository, StoredEvent, TaskRepository, ToolExecutionRepository,
    WorkspaceLeaseRepository,
};
use athena_types::{
    AgentDefinition, AgentId, AgentInstance, AgentInstanceState, Approval, ApprovalId,
    ApprovalState, Artifact, Clarification, ClarificationId, ClarificationState, Conversation,
    ConversationId, Evaluation, EvaluationId, EventId, ExecutionResult, Hierarchy, HierarchyId,
    InstanceId, MemoryId, MemoryRecord, MemoryTier, MessageId, Plan, PlanId, PlanStep, PlanStepId,
    PlanStepState, ResultId, RuntimeInstance, RuntimeInstanceId, SideEffects, Skill, SkillId,
    SkillState, StopReason, Task, TaskId, TaskState, ToolExecution, ToolExecutionId,
    ToolExecutionStatus, Utterance,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::sync::Arc;

/// The ATHENA store: one engine handle, table-backed.
#[derive(Debug, Clone)]
pub struct AthenaStore {
    db: Arc<std::sync::Mutex<Database>>,
}

impl AthenaStore {
    /// Open a store at a path.
    ///
    /// # Errors
    /// Engine open or migration faults.
    pub fn open(path: &std::path::Path) -> Result<Self> {
        Ok(Self {
            db: Arc::new(std::sync::Mutex::new(Database::open(path)?)),
        })
    }

    /// Open an in-memory store, for tests.
    ///
    /// # Errors
    /// Engine open faults.
    pub fn in_memory() -> Result<Self> {
        Ok(Self {
            db: Arc::new(std::sync::Mutex::new(Database::open_in_memory()?)),
        })
    }

    /// Commit one atomic unit.
    fn commit<R>(&self, f: impl FnOnce(&mut WriteTx) -> Result<R>) -> Result<R> {
        self.db.lock().expect("store").commit(f)
    }

    /// Read once.
    fn read<R>(&self, f: impl FnOnce(&ReadTx<'_, '_>) -> Result<R>) -> Result<R> {
        self.db.lock().expect("store").read(f)
    }

    /// Seed one raw row (the test hatch): a malformed token is written as-is, unstructured,
    /// so the decode discipline becomes assertable against the engine (`PersistenceError`'s
    /// `Decode` face).
    ///
    /// # Errors
    /// Engine faults.
    pub fn raw_row(
        &self,
        definition: redb::TableDefinition<String, String>,
        key: &str,
        value: &str,
    ) -> Result<()> {
        self.commit(|tx| tx.put(definition, key, value))
    }
}

// --- encoding -----------------------------------------------------------------------------

/// Encode a row.
fn encode_row<T: Serialize>(entity: &'static str, column: &'static str, row: &T) -> Result<String> {
    serde_json::to_string(row).map_err(|e| PersistenceError::Decode {
        entity,
        column,
        detail: format!("encode: {e}"),
    })
}

/// Decode a row, naming the surface it failed on.
fn decode_row<T: DeserializeOwned>(
    entity: &'static str,
    column: &'static str,
    raw: &str,
) -> Result<T> {
    serde_json::from_str(raw).map_err(|e| PersistenceError::Decode {
        entity,
        column,
        detail: e.to_string(),
    })
}

/// Pad a numeric key part to a fixed width so ordered scans stay ordered.
fn pad(n: u64) -> String {
    format!("{n:020}")
}

/// The composite key separator.
const SEP: char = '|';

// --- TaskRepository ----------------------------------------------------------------------

impl TaskRepository for AthenaStore {
    fn insert(&mut self, task: &Task) -> Result<()> {
        self.commit(|tx| {
            if tx.get(tables::TASKS, task.id.as_str())?.is_some() {
                return Err(PersistenceError::Conflict {
                    detail: format!("task {} exists", task.id.as_str()),
                });
            }
            tx.put(
                tables::TASKS,
                task.id.as_str(),
                &encode_row("task", "row", task)?,
            )
        })
    }

    fn list_all(&self) -> Result<Vec<Task>> {
        self.read(|view| {
            let mut tasks = Vec::new();
            for (_, raw) in view.rows(tables::TASKS)? {
                tasks.push(decode_row::<Task>("task", "row", &raw)?);
            }
            // `ORDER BY created_at DESC`; a tie resolves by identifier (the engine's own
            // creation-order face).
            tasks.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
            Ok(tasks)
        })
    }

    fn list_page(&self, offset: u64, limit: u64) -> Result<Vec<Task>> {
        // The store-side deep-page face (NFR-02's p95 budget at 10,000 Tasks): the
        // skipped rows are keys only, never hydrated.
        self.read(|view| {
            let mut tasks = Vec::new();
            let mut skipped = 0_u64;
            for (_, raw) in view.rows_rev(tables::TASKS)? {
                if skipped < offset {
                    skipped += 1;
                    continue;
                }
                if u64::try_from(tasks.len()).unwrap_or(u64::MAX) >= limit {
                    break;
                }
                tasks.push(decode_row::<Task>("task", "row", &raw)?);
            }
            Ok(tasks)
        })
    }

    fn get(&self, id: &TaskId) -> Result<Option<Task>> {
        self.read(|view| {
            view.get(tables::TASKS, id.as_str())?
                .map(|raw| decode_row::<Task>("task", "row", &raw))
                .transpose()
        })
    }

    fn list_by_state(&self, state: TaskState, limit: u64) -> Result<Vec<Task>> {
        self.read(|view| {
            let mut tasks = Vec::new();
            for (_, raw) in view.rows_rev(tables::TASKS)? {
                let task = decode_row::<Task>("task", "row", &raw)?;
                if task.state == state {
                    tasks.push(task);
                    if u64::try_from(tasks.len()).unwrap_or(u64::MAX) >= limit {
                        break;
                    }
                }
            }
            Ok(tasks)
        })
    }

    fn list_non_terminal(&self) -> Result<Vec<Task>> {
        self.read(|view| {
            let mut tasks = Vec::new();
            for (_, raw) in view.rows_rev(tables::TASKS)? {
                let task = decode_row::<Task>("task", "row", &raw)?;
                if !task.state.is_terminal() {
                    tasks.push(task);
                }
            }
            Ok(tasks)
        })
    }

    fn list_children(&self, parent: &TaskId) -> Result<Vec<Task>> {
        self.read(|view| {
            let mut tasks = Vec::new();
            for (_, raw) in view.rows(tables::TASKS)? {
                let task = decode_row::<Task>("task", "row", &raw)?;
                if task.parent_id.as_ref().is_some_and(|id| id == parent) {
                    tasks.push(task);
                }
            }
            // `ORDER BY created_at ASC`.
            tasks.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
            Ok(tasks)
        })
    }

    fn update(&mut self, task: &Task) -> Result<()> {
        self.commit(|tx| {
            let stored_raw = tx.get(tables::TASKS, task.id.as_str())?.ok_or_else(|| {
                PersistenceError::NotFound {
                    entity: "task",
                    id: task.id.as_str().into(),
                }
            })?;
            // `ATH-DATA-004`: the original `instruction` text is preserved and never
            // replaced by a summary; the update carries the mutable fields only.
            let stored = decode_row::<Task>("task", "row", &stored_raw)?;
            let mut resolved = task.clone();
            resolved.instruction = stored.instruction;
            resolved.created_at = stored.created_at;
            tx.put(
                tables::TASKS,
                task.id.as_str(),
                &encode_row("task", "row", &resolved)?,
            )
        })
    }
}

// --- PlanRepository ----------------------------------------------------------------------

impl PlanRepository for AthenaStore {
    fn insert(&mut self, plan: &Plan) -> Result<()> {
        self.commit(|tx| {
            // `(task_id, revision)` is unique: a revision is never overwritten
            // (ATH-PLAN-004). A revision must win its never-overwritten slot before it
            // writes anything: claim first, then write both records inside this same unit.
            let revision_key = format!("{}|{}", plan.task_id.as_str(), pad(plan.revision));
            if !tx.insert_absent(tables::PLANS_BY_REV, &revision_key, plan.id.as_str())? {
                return Err(PersistenceError::Conflict {
                    detail: format!(
                        "plan revision {} for task {} already exists; a revision is never \
                         overwritten (ATH-PLAN-004)",
                        plan.revision, plan.task_id
                    ),
                });
            }
            // Plan row (meta only — the steps carry their own rows).
            let mut bare = plan.clone();
            bare.steps = Vec::new();
            tx.put(
                tables::PLANS,
                plan.id.as_str(),
                &encode_row("plan", "row", &bare)?,
            )?;
            tx.insert_absent(tables::PLANS_BY_ID, plan.id.as_str(), &revision_key)?;
            // Step rows (their dependency edges ride the step itself).
            for step in &plan.steps {
                tx.put(
                    tables::PLAN_STEPS,
                    step.id.as_str(),
                    &encode_row("plan_step", "row", step)?,
                )?;
                tx.insert_absent(
                    tables::STEPS_BY_PLAN,
                    &format!("{}|{}", step.plan_id.as_str(), pad(step.order)),
                    step.id.as_str(),
                )?;
            }
            Ok(())
        })
    }

    fn get(&self, id: &PlanId) -> Result<Option<Plan>> {
        self.read(|view| {
            // The plan's meta row, then its steps re-attached.
            let Some(revision_key) = view.get(tables::PLANS_BY_ID, id.as_str())? else {
                return Ok(None);
            };
            let Some(plan_id_raw) = view.get(tables::PLANS_BY_REV, &revision_key)? else {
                return Ok(None);
            };
            let plan_meta_id =
                PlanId::parse(&plan_id_raw).ok_or_else(|| PersistenceError::Decode {
                    entity: "plan",
                    column: "id",
                    detail: format!("malformed identifier {plan_id_raw:?}"),
                })?;
            let (task_id, revision_raw) =
                revision_key
                    .split_once(SEP)
                    .ok_or_else(|| PersistenceError::Decode {
                        entity: "plan",
                        column: "revision_key",
                        detail: format!("malformed composite key {revision_key:?}"),
                    })?;
            let revision = revision_raw.parse::<u64>().unwrap_or(0);
            let meta_raw = view
                .get(tables::PLANS, plan_meta_id.as_str())?
                .ok_or_else(|| PersistenceError::NotFound {
                    entity: "plan",
                    id: plan_meta_id.as_str().into(),
                })?;
            let mut plan = decode_row::<Plan>("plan", "row", &meta_raw)?;
            plan.task_id = TaskId::parse(task_id).ok_or_else(|| PersistenceError::Decode {
                entity: "plan",
                column: "task_id",
                detail: format!("malformed identifier {task_id:?}"),
            })?;
            plan.revision = revision;
            plan.id = plan_meta_id;
            // Steps, ordered by their own step order.
            let mut steps = Vec::new();
            let plan_prefix = format!("{}|", id.as_str());
            for (key, step_id_raw) in view.rows(tables::STEPS_BY_PLAN)? {
                if !key.starts_with(&plan_prefix) {
                    continue;
                }
                if let Some(step_raw) = view.get(tables::PLAN_STEPS, &step_id_raw)? {
                    steps.push(decode_row::<PlanStep>("plan_step", "row", &step_raw)?);
                }
            }
            steps.sort_by_key(|step| step.order);
            plan.steps = steps;
            Ok(Some(plan))
        })
    }

    fn latest_for_task(&self, task: &TaskId) -> Result<Option<Plan>> {
        self.read(|view| {
            let prefix = format!("{}|", task.as_str());
            let mut best: Option<(u64, PlanId)> = None;
            for (key, plan_id_raw) in view.rows(tables::PLANS_BY_REV)? {
                if !key.starts_with(&prefix) {
                    continue;
                }
                let revision = key
                    .rsplit(SEP)
                    .next()
                    .and_then(|padded| padded.parse::<u64>().ok())
                    .unwrap_or(0);
                let plan_id =
                    PlanId::parse(&plan_id_raw).ok_or_else(|| PersistenceError::Decode {
                        entity: "plan",
                        column: "id",
                        detail: format!("malformed identifier {plan_id_raw:?}"),
                    })?;
                let should_replace = best.as_ref().is_none_or(|(current, _)| revision > *current);
                if should_replace {
                    best = Some((revision, plan_id));
                }
            }
            // The plan's own re-assembly happens through its own read: keep the unit
            // closures non-reentrant (a store face must never call a store face inside
            // one of its own units).
            Ok(best.map(|(_, plan_id)| plan_id))
        })
        .and_then(|best| match best {
            Some(plan_id) => PlanRepository::get(self, &plan_id),
            None => Ok(None),
        })
    }

    fn revision_for_task(&self, task: &TaskId, revision: u64) -> Result<Option<Plan>> {
        self.read(|view| {
            let key = format!("{}|{}", task.as_str(), pad(revision));
            view.get(tables::PLANS_BY_REV, &key)
        })
        .and_then(|found| match found {
            Some(plan_id_raw) => {
                let plan_id =
                    PlanId::parse(&plan_id_raw).ok_or_else(|| PersistenceError::Decode {
                        entity: "plan",
                        column: "id",
                        detail: format!("malformed identifier {plan_id_raw:?}"),
                    })?;
                PlanRepository::get(self, &plan_id)
            }
            None => Ok(None),
        })
    }

    fn revisions_for_task(&self, task: &TaskId) -> Result<Vec<u64>> {
        self.read(|view| {
            let prefix = format!("{}|", task.as_str());
            let mut revisions = Vec::new();
            for (key, _) in view.rows(tables::PLANS_BY_REV)? {
                if !key.starts_with(&prefix) {
                    continue;
                }
                if let Some(padded) = key.rsplit(SEP).next() {
                    revisions.push(padded.parse::<u64>().unwrap_or(0));
                }
            }
            revisions.sort_unstable();
            Ok(revisions)
        })
    }

    fn update_step_state(&mut self, step: &PlanStepId, state: PlanStepState) -> Result<()> {
        self.commit(|tx| {
            let raw = tx.get(tables::PLAN_STEPS, step.as_str())?.ok_or_else(|| {
                PersistenceError::NotFound {
                    entity: "plan_step",
                    id: step.to_string(),
                }
            })?;
            let mut resolved = decode_row::<PlanStep>("plan_step", "row", &raw)?;
            resolved.state = state;
            tx.put(
                tables::PLAN_STEPS,
                step.as_str(),
                &encode_row("plan_step", "row", &resolved)?,
            )
        })
    }

    fn get_step(&self, step: &PlanStepId) -> Result<Option<PlanStep>> {
        self.read(|view| {
            view.get(tables::PLAN_STEPS, step.as_str())?
                .map(|raw| decode_row::<PlanStep>("plan_step", "row", &raw))
                .transpose()
        })
    }

    fn record_attempt(
        &mut self,
        step: &PlanStepId,
        attempt: u64,
        instance: Option<&InstanceId>,
    ) -> Result<()> {
        // The attempt table is the history: a duplicate `(step, attempt)` is a conflict,
        // never an overwrite (ATH-FAIL-007).
        self.commit(|tx| {
            let claimed = tx.insert_absent(
                tables::PLAN_STEP_ATTEMPTS,
                &format!("{}|{}", step.as_str(), pad(attempt)),
                &serde_json::json!({
                    "instance_id": instance.map(|id| id.as_str().to_string()),
                    "started_at": crate::db::now_rfc3339(),
                })
                .to_string(),
            )?;
            if !claimed {
                return Err(PersistenceError::Conflict {
                    detail: format!(
                        "attempt {attempt} of step {step} is already recorded; attempt \
                         history is never overwritten (ATH-FAIL-007)"
                    ),
                });
            }
            Ok(())
        })
    }
}

// --- EventRepository (append-only) ------------------------------------------------------

/// The event-append half used by the atomic primitives: the next `sequence` is
/// allocated, the rows (event, by-task, by-sequence) are written inside the caller's
/// unit.
fn append_event_tx(tx: &mut WriteTx, event: &NewEvent) -> Result<StoredEvent> {
    let sequence_raw = tx
        .get(tables::META, "event_sequence")?
        .unwrap_or_else(|| "0".to_string());
    let next = sequence_raw.parse::<u64>().unwrap_or(0) + 1;
    let stored = StoredEvent {
        event_id: event.event_id.clone(),
        schema_version: event.schema_version,
        event_type: event.event_type.clone(),
        occurred_at: event.occurred_at.clone(),
        sequence: next,
        task_id: event.task_id.clone(),
        plan_step_id: event.plan_step_id.clone(),
        agent_id: event.agent_id.clone(),
        instance_id: event.instance_id.clone(),
        skill_id: event.skill_id.clone(),
        correlation_id: event.correlation_id.clone(),
        causation_id: event.causation_id.clone(),
        actor: event.actor.clone(),
        payload_json: event.payload_json.clone(),
    };
    tx.put(
        tables::EVENTS,
        stored.event_id.as_str(),
        &encode_row("event", "row", &stored)?,
    )?;
    tx.insert_absent(tables::EVENTS_BY_SEQ, &pad(next), stored.event_id.as_str())?;
    if let Some(task) = &stored.task_id {
        tx.insert_absent(
            tables::EVENTS_BY_TASK,
            &format!("{}|{}", task.as_str(), stored.event_id.as_str()),
            stored.event_id.as_str(),
        )?;
    }
    tx.put(tables::META, "event_sequence", &next.to_string())?;
    Ok(stored)
}

impl EventRepository for AthenaStore {
    fn append(&mut self, event: &NewEvent) -> Result<StoredEvent> {
        self.commit(|tx| append_event_tx(tx, event))
    }

    fn get(&self, id: &EventId) -> Result<Option<StoredEvent>> {
        self.read(|view| {
            view.get(tables::EVENTS, id.as_str())?
                .map(|raw| decode_row::<StoredEvent>("event", "row", &raw))
                .transpose()
        })
    }

    fn list_for_task(&self, task: &TaskId) -> Result<Vec<StoredEvent>> {
        self.read(|view| {
            // The replay read (ATH-EVENT-012): the task's own rows, ordered by sequence.
            let prefix = format!("{}|", task.as_str());
            let mut events = Vec::new();
            for (key, _) in view.rows(tables::EVENTS_BY_TASK)? {
                if !key.starts_with(&prefix) {
                    continue;
                }
                if let Some((_, event_id)) = key.split_once(SEP)
                    && let Some(raw) = view.get(tables::EVENTS, event_id)?
                {
                    events.push(decode_row::<StoredEvent>("event", "row", &raw)?);
                }
            }
            events.sort_by_key(|event| event.sequence);
            Ok(events)
        })
    }

    fn list_since(&self, sequence: u64, limit: u64) -> Result<Vec<StoredEvent>> {
        self.read(|view| {
            let mut events = Vec::new();
            for (_, event_id) in view.rows(tables::EVENTS_BY_SEQ)? {
                if u64::try_from(events.len()).unwrap_or(u64::MAX) >= limit {
                    break;
                }
                let Some(raw) = view.get(tables::EVENTS, &event_id)? else {
                    continue;
                };
                let stored = decode_row::<StoredEvent>("event", "row", &raw)?;
                if stored.sequence > sequence {
                    events.push(stored);
                }
            }
            events.sort_by_key(|event| event.sequence);
            Ok(events)
        })
    }

    fn max_sequence(&self) -> Result<u64> {
        self.read(|view| {
            Ok(view
                .get(tables::META, "event_sequence")?
                .and_then(|raw| raw.parse::<u64>().ok())
                .unwrap_or(0))
        })
    }

    fn prune_before(&mut self, cutoff_rfc3339: &str) -> Result<u64> {
        // The only permitted deletion for `events` (ATH-PERS-006/008); it preserves each
        // Task's final result and Results, which live in their own tables.
        self.commit(|tx| {
            let mut removed = 0_u64;
            for (event_key, raw) in tx.rows(tables::EVENTS)? {
                let stored = decode_row::<StoredEvent>("event", "row", &raw)?;
                if stored.occurred_at.as_str() < cutoff_rfc3339 {
                    let _ = tx.remove(tables::EVENTS, &event_key)?;
                    let _ = tx.remove(tables::EVENTS_BY_SEQ, &pad(stored.sequence))?;
                    if let Some(task) = &stored.task_id {
                        let _ = tx.remove(
                            tables::EVENTS_BY_TASK,
                            &format!("{}|{}", task.as_str(), stored.event_id.as_str()),
                        )?;
                    }
                    removed += 1;
                }
            }
            Ok(removed)
        })
    }
}

// --- IdempotencyRepository ---------------------------------------------------------------

impl IdempotencyRepository for AthenaStore {
    fn claim(&mut self, key: &str) -> Result<bool> {
        // One check-then-put row in one unit: the engine's write face is single-writer,
        // so two claims serialize — the second unit sees the committed first row and
        // refuses (ATH-CORE-035, ATH-NFR-017).
        self.commit(|tx| {
            if tx.get(tables::IDEMPOTENCY_KEYS, key)?.is_some() {
                return Ok(false);
            }
            tx.put(
                tables::IDEMPOTENCY_KEYS,
                key,
                &serde_json::json!({
                    "created_at": crate::db::now_rfc3339(),
                    "outcome": serde_json::Value::Null,
                })
                .to_string(),
            )?;
            Ok(true)
        })
    }

    fn is_claimed(&self, key: &str) -> Result<bool> {
        self.read(|view| Ok(view.get(tables::IDEMPOTENCY_KEYS, key)?.is_some()))
    }

    fn record_outcome(&mut self, key: &str, outcome: &str) -> Result<()> {
        self.commit(|tx| {
            let raw = tx.get(tables::IDEMPOTENCY_KEYS, key)?.ok_or_else(|| {
                PersistenceError::NotFound {
                    entity: "idempotency_key",
                    id: key.to_string(),
                }
            })?;
            let mut row = decode_row::<serde_json::Value>("idempotency_key", "row", &raw)?;
            row["outcome"] = serde_json::Value::String(outcome.to_string());
            tx.put(tables::IDEMPOTENCY_KEYS, key, &row.to_string())
        })
    }

    fn outcome(&self, key: &str) -> Result<Option<String>> {
        self.read(|view| {
            Ok(view.get(tables::IDEMPOTENCY_KEYS, key)?.and_then(|raw| {
                decode_row::<serde_json::Value>("idempotency_key", "row", &raw)
                    .ok()
                    .and_then(|row| match row["outcome"].clone() {
                        serde_json::Value::Null => None,
                        serde_json::Value::String(text) => Some(text),
                        other => Some(other.to_string()),
                    })
            }))
        })
    }

    fn prune_before(&mut self, cutoff_rfc3339: &str) -> Result<u64> {
        self.commit(|tx| {
            let mut removed = 0_u64;
            for (key, raw) in tx.rows(tables::IDEMPOTENCY_KEYS)? {
                let row = decode_row::<serde_json::Value>("idempotency_key", "row", &raw)?;
                if row["created_at"]
                    .as_str()
                    .is_some_and(|at| at < cutoff_rfc3339)
                {
                    let _ = tx.remove(tables::IDEMPOTENCY_KEYS, &key)?;
                    removed += 1;
                }
            }
            Ok(removed)
        })
    }
}

// --- WorkspaceLeaseRepository ------------------------------------------------------------

impl WorkspaceLeaseRepository for AthenaStore {
    fn acquire(&mut self, workspace: &str, task: &TaskId) -> Result<bool> {
        self.commit(|tx| {
            if tx.get(tables::WORKSPACE_LEASES, workspace)?.is_some() {
                return Ok(false);
            }
            tx.put(
                tables::WORKSPACE_LEASES,
                workspace,
                &serde_json::json!({
                    "task_id": task.as_str(),
                    "acquired_at": crate::db::now_rfc3339(),
                })
                .to_string(),
            )?;
            Ok(true)
        })
    }

    fn release(&mut self, workspace: &str) -> Result<()> {
        self.commit(|tx| {
            let _ = tx.remove(tables::WORKSPACE_LEASES, workspace)?;
            Ok(())
        })
    }

    fn holder(&self, workspace: &str) -> Result<Option<TaskId>> {
        self.read(|view| {
            let Some(raw) = view.get(tables::WORKSPACE_LEASES, workspace)? else {
                return Ok(None);
            };
            let row = decode_row::<serde_json::Value>("workspace_lease", "row", &raw)?;
            Ok(row["task_id"].as_str().and_then(TaskId::parse))
        })
    }
}

// --- AgentRepository ---------------------------------------------------------------------

impl AgentRepository for AthenaStore {
    fn upsert_definition(&mut self, def: &AgentDefinition) -> Result<()> {
        self.commit(|tx| {
            tx.put(
                tables::AGENT_DEFINITIONS,
                def.id.as_str(),
                &encode_row("agent_definition", "row", def)?,
            )
        })
    }

    fn get_definition(&self, id: &AgentId) -> Result<Option<AgentDefinition>> {
        self.read(|view| {
            view.get(tables::AGENT_DEFINITIONS, id.as_str())?
                .map(|raw| decode_row::<AgentDefinition>("agent_definition", "row", &raw))
                .transpose()
        })
    }

    fn list_definitions(&self) -> Result<Vec<AgentDefinition>> {
        self.read(|view| {
            let mut definitions = Vec::new();
            for (_, raw) in view.rows(tables::AGENT_DEFINITIONS)? {
                definitions.push(decode_row::<AgentDefinition>(
                    "agent_definition",
                    "row",
                    &raw,
                )?);
            }
            // `ORDER BY id`.
            definitions.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
            Ok(definitions)
        })
    }

    fn insert_instance(&mut self, instance: &AgentInstance) -> Result<()> {
        self.commit(|tx| {
            // A definition row is the validated parent record (ATH-AGENT-002): an
            // instance must never outlive a definition absent from the store.
            if tx
                .get(tables::AGENT_DEFINITIONS, instance.agent_id.as_str())?
                .is_none()
            {
                return Err(PersistenceError::NotFound {
                    entity: "agent_definition",
                    id: instance.agent_id.as_str().into(),
                });
            }
            // 1 : 1 (ATH-RT-012): one AgentInstance row is one Runtime Instance row — an
            // instance whose `runtime_instance_id` is already owned is refused. The scan
            // reads the same unit's own rows, so the ownership answer is atomic with the
            // write.
            if let Some(runtime_id) = &instance.runtime_instance_id
                && let Some(owner) = full_instance_by_runtime(tx, runtime_id)?
            {
                return Err(PersistenceError::Conflict {
                    detail: format!(
                        "runtime instance {} is already owned by {} (ATH-RT-012)",
                        runtime_id.as_str(),
                        owner.as_str()
                    ),
                });
            }
            if tx
                .get(tables::AGENT_INSTANCES, instance.id.as_str())?
                .is_some()
            {
                return Err(PersistenceError::Conflict {
                    detail: format!("agent instance {} exists", instance.id.as_str()),
                });
            }
            tx.put(
                tables::AGENT_INSTANCES,
                instance.id.as_str(),
                &encode_row("agent_instance", "row", instance)?,
            )
        })
    }

    fn get_instance(&self, id: &InstanceId) -> Result<Option<AgentInstance>> {
        self.read(|view| {
            view.get(tables::AGENT_INSTANCES, id.as_str())?
                .map(|raw| decode_row::<AgentInstance>("agent_instance", "row", &raw))
                .transpose()
        })
    }

    fn update_instance_state(&mut self, id: &InstanceId, state: AgentInstanceState) -> Result<()> {
        self.commit(|tx| {
            let raw = tx
                .get(tables::AGENT_INSTANCES, id.as_str())?
                .ok_or_else(|| PersistenceError::NotFound {
                    entity: "agent_instance",
                    id: id.as_str().into(),
                })?;
            let mut resolved = decode_row::<AgentInstance>("agent_instance", "row", &raw)?;
            resolved.state = state;
            tx.put(
                tables::AGENT_INSTANCES,
                id.as_str(),
                &encode_row("agent_instance", "row", &resolved)?,
            )
        })
    }

    fn list_instances_for_task(&self, task: &TaskId) -> Result<Vec<AgentInstance>> {
        self.read(|view| {
            let mut instances = Vec::new();
            for (_, raw) in view.rows(tables::AGENT_INSTANCES)? {
                let resolved = decode_row::<AgentInstance>("agent_instance", "row", &raw)?;
                if resolved.task_id.as_ref().is_some_and(|id| id == task) {
                    instances.push(resolved);
                }
            }
            // `ORDER BY attempt, id`.
            instances.sort_by(|a, b| a.attempt.cmp(&b.attempt).then(a.id.cmp(&b.id)));
            Ok(instances)
        })
    }

    fn list_non_terminal_instances(&self) -> Result<Vec<AgentInstance>> {
        self.read(|view| {
            let mut instances = Vec::new();
            for (_, raw) in view.rows(tables::AGENT_INSTANCES)? {
                let resolved = decode_row::<AgentInstance>("agent_instance", "row", &raw)?;
                if resolved.state.as_str() != "STOPPED" {
                    instances.push(resolved);
                }
            }
            // `ORDER BY started_at, id`.
            instances.sort_by(|a, b| a.started_at.cmp(&b.started_at).then(a.id.cmp(&b.id)));
            Ok(instances)
        })
    }

    fn count_non_terminal_for_step(&self, step: &PlanStepId) -> Result<u64> {
        self.read(|view| {
            let mut counted = 0_u64;
            for (_, raw) in view.rows(tables::AGENT_INSTANCES)? {
                let resolved = decode_row::<AgentInstance>("agent_instance", "row", &raw)?;
                if resolved.plan_step_id.as_ref().is_some_and(|id| id == step)
                    && !matches!(resolved.state.as_str(), "STOPPED" | "FAILED")
                {
                    counted += 1;
                }
            }
            Ok(counted)
        })
    }

    fn list_all_instances(&self) -> Result<Vec<AgentInstance>> {
        self.read(|view| {
            let mut instances = Vec::new();
            for (_, raw) in view.rows(tables::AGENT_INSTANCES)? {
                instances.push(decode_row::<AgentInstance>("agent_instance", "row", &raw)?);
            }
            // `ORDER BY started_at, id`.
            instances.sort_by(|a, b| a.started_at.cmp(&b.started_at).then(a.id.cmp(&b.id)));
            Ok(instances)
        })
    }
}

/// `AgentInstance : RuntimeInstance = 1 : 1` — the owner of a runtime id, for the
/// refusal that keeps the 1 : 1 rule mechanical (`ATH-RT-012`). Reads the unit's own
/// rows, so the answer is atomic with the caller's write.
fn full_instance_by_runtime(
    tx: &mut WriteTx,
    runtime_id: &RuntimeInstanceId,
) -> Result<Option<InstanceId>> {
    for (_, raw) in tx.rows(tables::AGENT_INSTANCES)? {
        let resolved = decode_row::<AgentInstance>("agent_instance", "row", &raw)?;
        if resolved
            .runtime_instance_id
            .as_ref()
            .is_some_and(|id| id == runtime_id)
        {
            return Ok(Some(resolved.id));
        }
    }
    Ok(None)
}

// --- ResultRepository --------------------------------------------------------------------

impl ResultRepository for AthenaStore {
    fn insert(&mut self, result: &ExecutionResult) -> Result<()> {
        // Results are immutable once written (`ATH-RESULT-003`): the row's own key is
        // the immutability, and a second insert is a conflict.
        self.commit(|tx| {
            if tx.get(tables::RESULTS, result.id.as_str())?.is_some() {
                return Err(PersistenceError::Conflict {
                    detail: format!("result {} exists", result.id.as_str()),
                });
            }
            tx.put(
                tables::RESULTS,
                result.id.as_str(),
                &encode_row("result", "row", result)?,
            )
        })
    }

    fn get(&self, id: &ResultId) -> Result<Option<ExecutionResult>> {
        self.read(|view| {
            view.get(tables::RESULTS, id.as_str())?
                .map(|raw| decode_row::<ExecutionResult>("result", "row", &raw))
                .transpose()
        })
    }

    fn list_for_task(&self, task: &TaskId) -> Result<Vec<ExecutionResult>> {
        self.read(|view| {
            let mut results = Vec::new();
            for (_, raw) in view.rows(tables::RESULTS)? {
                let resolved = decode_row::<ExecutionResult>("result", "row", &raw)?;
                if resolved.task_id == *task {
                    results.push(resolved);
                }
            }
            // `ORDER BY started_at ASC` (the task's own reception trail).
            results.sort_by(|a, b| a.started_at.cmp(&b.started_at).then(a.id.cmp(&b.id)));
            Ok(results)
        })
    }

    fn list_artifacts(&self, result: &ResultId) -> Result<Vec<Artifact>> {
        self.read(|view| {
            let Some(raw) = view.get(tables::RESULTS, result.as_str())? else {
                return Ok(Vec::new());
            };
            let resolved = decode_row::<ExecutionResult>("result", "row", &raw)?;
            Ok(resolved.artifacts)
        })
    }

    fn insert_evaluation(&mut self, evaluation: &Evaluation) -> Result<()> {
        self.commit(|tx| {
            // An evaluation row answers a Result that exists.
            if tx
                .get(tables::RESULTS, evaluation.result_id.as_str())?
                .is_none()
            {
                return Err(PersistenceError::NotFound {
                    entity: "result",
                    id: evaluation.result_id.as_str().into(),
                });
            }
            if tx
                .get(tables::EVALUATIONS, evaluation.id.as_str())?
                .is_some()
            {
                return Err(PersistenceError::Conflict {
                    detail: format!("evaluation {} exists", evaluation.id.as_str()),
                });
            }
            tx.put(
                tables::EVALUATIONS,
                evaluation.id.as_str(),
                &encode_row("evaluation", "row", evaluation)?,
            )?;
            tx.insert_absent(
                tables::EVALUATIONS_BY_RESULT,
                &format!(
                    "{}|{}",
                    evaluation.result_id.as_str(),
                    evaluation.id.as_str()
                ),
                evaluation.id.as_str(),
            )?;
            Ok(())
        })
    }

    fn get_evaluation(&self, id: &EvaluationId) -> Result<Option<Evaluation>> {
        self.read(|view| {
            view.get(tables::EVALUATIONS, id.as_str())?
                .map(|raw| decode_row::<Evaluation>("evaluation", "row", &raw))
                .transpose()
        })
    }

    fn list_evaluations_for_result(&self, result: &ResultId) -> Result<Vec<Evaluation>> {
        self.read(|view| {
            let prefix = format!("{}|", result.as_str());
            let mut evaluations = Vec::new();
            for (key, _) in view.rows(tables::EVALUATIONS_BY_RESULT)? {
                if !key.starts_with(&prefix) {
                    continue;
                }
                if let Some((_, eval_id)) = key.split_once(SEP)
                    && let Some(raw) = view.get(tables::EVALUATIONS, eval_id)?
                {
                    evaluations.push(decode_row::<Evaluation>("evaluation", "row", &raw)?);
                }
            }
            evaluations.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(evaluations)
        })
    }
}

// --- SkillRepository ---------------------------------------------------------------------

impl SkillRepository for AthenaStore {
    fn upsert(&mut self, skill: &Skill) -> Result<()> {
        self.commit(|tx| {
            tx.put(
                tables::SKILLS,
                &skill_key(&skill.id, &skill.version),
                &encode_row("skill", "row", skill)?,
            )
        })
    }

    fn get(&self, id: &SkillId, version: &str) -> Result<Option<Skill>> {
        self.read(|view| {
            view.get(tables::SKILLS, &skill_key(id, version))?
                .map(|raw| decode_row::<Skill>("skill", "row", &raw))
                .transpose()
        })
    }

    fn list_versions(&self, id: &SkillId) -> Result<Vec<Skill>> {
        self.read(|view| {
            let prefix = format!("{}|", id.as_str());
            let mut versions = Vec::new();
            for (key, raw) in view.rows(tables::SKILLS)? {
                if key.starts_with(&prefix) {
                    versions.push(decode_row::<Skill>("skill", "row", &raw)?);
                }
            }
            versions.sort_by(|a, b| semver_key(&b.version).cmp(&semver_key(&a.version)));
            Ok(versions)
        })
    }

    fn list_by_state(&self, state: SkillState) -> Result<Vec<Skill>> {
        self.read(|view| {
            let mut skills = Vec::new();
            for (_, raw) in view.rows(tables::SKILLS)? {
                let resolved = decode_row::<Skill>("skill", "row", &raw)?;
                if resolved.state == state {
                    skills.push(resolved);
                }
            }
            skills.sort_by(|a, b| {
                a.id.as_str()
                    .cmp(b.id.as_str())
                    .then(b.version.cmp(&a.version))
            });
            Ok(skills)
        })
    }

    fn search_by_capability(&self, capability: &str) -> Result<Vec<Skill>> {
        self.read(|view| {
            let mut skills = Vec::new();
            for (_, raw) in view.rows(tables::SKILLS)? {
                let resolved = decode_row::<Skill>("skill", "row", &raw)?;
                // `ATH-STATE-008`: selection candidates are `ENABLED` only; the search is
                // exact-text on capability.
                if resolved.state == SkillState::Enabled
                    && resolved
                        .capabilities
                        .iter()
                        .any(|offered| offered == capability)
                {
                    skills.push(resolved);
                }
            }
            skills.sort_by(|a, b| {
                a.id.as_str()
                    .cmp(b.id.as_str())
                    .then(b.version.cmp(&a.version))
            });
            Ok(skills)
        })
    }

    fn delete(&mut self, id: &SkillId, version: &str) -> Result<()> {
        self.commit(|tx| {
            let _ = tx.remove(tables::SKILLS, &skill_key(id, version))?;
            Ok(())
        })
    }
}

/// A skill's composite key (`id|version`).
fn skill_key(id: &SkillId, version: &str) -> String {
    format!("{}|{}", id.as_str(), version)
}

/// A version's ordering key: SemVer `MAJOR.MINOR.PATCH` sorts numerically.
fn semver_key(version: &str) -> Vec<u64> {
    version
        .split('.')
        .map(|part| {
            part.trim_end_matches(|c: char| !c.is_ascii_digit())
                .parse::<u64>()
                .unwrap_or(0)
        })
        .collect()
}

// --- ApprovalRepository ------------------------------------------------------------------

impl ApprovalRepository for AthenaStore {
    fn insert(&mut self, approval: &Approval) -> Result<()> {
        self.commit(|tx| {
            if tx.get(tables::APPROVALS, approval.id.as_str())?.is_some() {
                return Err(PersistenceError::Conflict {
                    detail: format!("approval {} exists", approval.id.as_str()),
                });
            }
            tx.put(
                tables::APPROVALS,
                approval.id.as_str(),
                &encode_row("approval", "row", approval)?,
            )
        })
    }

    fn get(&self, id: &ApprovalId) -> Result<Option<Approval>> {
        self.read(|view| {
            view.get(tables::APPROVALS, id.as_str())?
                .map(|raw| decode_row::<Approval>("approval", "row", &raw))
                .transpose()
        })
    }

    fn list_by_state(&self, state: ApprovalState) -> Result<Vec<Approval>> {
        self.read(|view| {
            let mut approvals = Vec::new();
            for (_, raw) in view.rows(tables::APPROVALS)? {
                let resolved = decode_row::<Approval>("approval", "row", &raw)?;
                if resolved.state == state {
                    approvals.push(resolved);
                }
            }
            // `ORDER BY expires_at, id`.
            approvals.sort_by(|a, b| {
                a.expires_at
                    .cmp(&b.expires_at)
                    .then(a.id.as_str().cmp(b.id.as_str()))
            });
            Ok(approvals)
        })
    }

    fn update_state(
        &mut self,
        id: &ApprovalId,
        state: ApprovalState,
        responded_by: Option<&str>,
        uses_remaining: u64,
    ) -> Result<()> {
        self.commit(|tx| {
            let raw = tx.get(tables::APPROVALS, id.as_str())?.ok_or_else(|| {
                PersistenceError::NotFound {
                    entity: "approval",
                    id: id.as_str().into(),
                }
            })?;
            let mut resolved = decode_row::<Approval>("approval", "row", &raw)?;
            resolved.state = state;
            if let Some(responder) = responded_by {
                resolved.responded_by = Some(responder.to_string());
                resolved.responded_at = Some(crate::db::now_rfc3339());
            }
            resolved.uses_remaining = uses_remaining;
            tx.put(
                tables::APPROVALS,
                id.as_str(),
                &encode_row("approval", "row", &resolved)?,
            )
        })
    }

    fn list_expired(&self, now_rfc3339: &str) -> Result<Vec<Approval>> {
        self.read(|view| {
            let mut approvals = Vec::new();
            for (_, raw) in view.rows(tables::APPROVALS)? {
                let resolved = decode_row::<Approval>("approval", "row", &raw)?;
                // `ATH-HITL-005`: a `REQUESTED` approval past its deadline is a rejection.
                if resolved.state == ApprovalState::Requested
                    && resolved.expires_at.as_str() < now_rfc3339
                {
                    approvals.push(resolved);
                }
            }
            approvals.sort_by(|a, b| {
                a.expires_at
                    .cmp(&b.expires_at)
                    .then(a.id.as_str().cmp(b.id.as_str()))
            });
            Ok(approvals)
        })
    }

    fn list_granted_unconsumed(&self) -> Result<Vec<Approval>> {
        self.read(|view| {
            let mut approvals = Vec::new();
            for (_, raw) in view.rows(tables::APPROVALS)? {
                let resolved = decode_row::<Approval>("approval", "row", &raw)?;
                // `ATH-CORE-046`: granted and unconsumed appear in restart context.
                if resolved.state == ApprovalState::Granted && resolved.uses_remaining > 0 {
                    approvals.push(resolved);
                }
            }
            approvals.sort_by(|a, b| {
                a.expires_at
                    .cmp(&b.expires_at)
                    .then(a.id.as_str().cmp(b.id.as_str()))
            });
            Ok(approvals)
        })
    }
}

// --- MemoryRepository --------------------------------------------------------------------

impl MemoryRepository for AthenaStore {
    fn insert(&mut self, record: &MemoryRecord) -> Result<()> {
        self.commit(|tx| {
            if tx
                .get(tables::MEMORY_RECORDS, record.id.as_str())?
                .is_some()
            {
                return Err(PersistenceError::Conflict {
                    detail: format!("memory record {} exists", record.id.as_str()),
                });
            }
            tx.put(
                tables::MEMORY_RECORDS,
                record.id.as_str(),
                &encode_row("memory_record", "row", record)?,
            )
        })
    }

    fn get(&self, id: &MemoryId) -> Result<Option<MemoryRecord>> {
        self.read(|view| {
            view.get(tables::MEMORY_RECORDS, id.as_str())?
                .map(|raw| decode_row::<MemoryRecord>("memory_record", "row", &raw))
                .transpose()
        })
    }

    fn list(&self, tier: MemoryTier, scope_id: &str) -> Result<Vec<MemoryRecord>> {
        self.read(|view| {
            let mut records = Vec::new();
            for (_, raw) in view.rows(tables::MEMORY_RECORDS)? {
                let resolved = decode_row::<MemoryRecord>("memory_record", "row", &raw)?;
                if resolved.tier == tier && resolved.scope_id == scope_id {
                    records.push(resolved);
                }
            }
            records.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
            Ok(records)
        })
    }

    fn list_by_tier(&self, tier: MemoryTier) -> Result<Vec<MemoryRecord>> {
        self.read(|view| {
            let mut records = Vec::new();
            for (_, raw) in view.rows(tables::MEMORY_RECORDS)? {
                let resolved = decode_row::<MemoryRecord>("memory_record", "row", &raw)?;
                if resolved.tier == tier {
                    records.push(resolved);
                }
            }
            records.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
            Ok(records)
        })
    }

    fn count(&self, tier: MemoryTier, scope_id: &str) -> Result<u64> {
        self.read(|view| {
            let mut counted = 0_u64;
            for (_, raw) in view.rows(tables::MEMORY_RECORDS)? {
                let resolved = decode_row::<MemoryRecord>("memory_record", "row", &raw)?;
                if resolved.tier == tier && resolved.scope_id == scope_id {
                    counted += 1;
                }
            }
            Ok(counted)
        })
    }

    fn delete(&mut self, id: &MemoryId) -> Result<()> {
        // The caller records the deletion as an Event (ATH-MEM-006).
        self.commit(|tx| {
            let _ = tx.remove(tables::MEMORY_RECORDS, id.as_str())?;
            Ok(())
        })
    }

    fn summarize(&mut self, id: &MemoryId, summary: &str) -> Result<()> {
        self.commit(|tx| {
            let raw = tx
                .get(tables::MEMORY_RECORDS, id.as_str())?
                .ok_or_else(|| PersistenceError::NotFound {
                    entity: "memory_record",
                    id: id.as_str().into(),
                })?;
            let mut resolved = decode_row::<MemoryRecord>("memory_record", "row", &raw)?;
            resolved.summary = Some(summary.to_string());
            tx.put(
                tables::MEMORY_RECORDS,
                id.as_str(),
                &encode_row("memory_record", "row", &resolved)?,
            )
        })
    }

    fn list_by_provenance(
        &self,
        task: Option<&TaskId>,
        event: Option<&EventId>,
    ) -> Result<Vec<MemoryRecord>> {
        self.read(|view| {
            let mut records = Vec::new();
            for (_, raw) in view.rows(tables::MEMORY_RECORDS)? {
                let resolved = decode_row::<MemoryRecord>("memory_record", "row", &raw)?;
                let task_matches =
                    task.is_none_or(|anchor| resolved.origin.task_id.as_ref() == Some(anchor));
                let event_matches =
                    event.is_none_or(|anchor| resolved.origin.event_id.as_ref() == Some(anchor));
                if task_matches && event_matches {
                    records.push(resolved);
                }
            }
            records.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
            Ok(records)
        })
    }
}

// --- ClarificationRepository -------------------------------------------------------------

impl ClarificationRepository for AthenaStore {
    fn open(&mut self, clarification: &Clarification) -> Result<()> {
        self.commit(|tx| {
            if tx
                .get(tables::CLARIFICATIONS, clarification.id.as_str())?
                .is_some()
            {
                return Err(PersistenceError::Conflict {
                    detail: format!("clarification {} exists", clarification.id.as_str()),
                });
            }
            tx.put(
                tables::CLARIFICATIONS,
                clarification.id.as_str(),
                &encode_row("clarification", "row", clarification)?,
            )
        })
    }

    fn get(&self, id: &ClarificationId) -> Result<Option<Clarification>> {
        self.read(|view| {
            view.get(tables::CLARIFICATIONS, id.as_str())?
                .map(|raw| decode_row::<Clarification>("clarification", "row", &raw))
                .transpose()
        })
    }

    fn pending_for_task(&self, task: &TaskId) -> Result<Vec<Clarification>> {
        self.read(|view| {
            let mut clarifications = Vec::new();
            for (_, raw) in view.rows(tables::CLARIFICATIONS)? {
                let resolved = decode_row::<Clarification>("clarification", "row", &raw)?;
                if resolved.task_id == *task && resolved.state == ClarificationState::Requested {
                    clarifications.push(resolved);
                }
            }
            clarifications.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(clarifications)
        })
    }

    fn pending_expired(&self, now_rfc3339: &str) -> Result<Vec<Clarification>> {
        self.read(|view| {
            let mut clarifications = Vec::new();
            for (_, raw) in view.rows(tables::CLARIFICATIONS)? {
                let resolved = decode_row::<Clarification>("clarification", "row", &raw)?;
                if resolved.state == ClarificationState::Requested
                    && resolved.expires_at.as_str() < now_rfc3339
                {
                    clarifications.push(resolved);
                }
            }
            clarifications.sort_by(|a, b| a.id.cmp(&b.id));
            Ok(clarifications)
        })
    }

    fn answer(&mut self, id: &ClarificationId, answer: &str, answered_at: &str) -> Result<()> {
        self.commit(|tx| {
            let raw = tx
                .get(tables::CLARIFICATIONS, id.as_str())?
                .ok_or_else(|| PersistenceError::NotFound {
                    entity: "clarification",
                    id: id.as_str().into(),
                })?;
            let resolved = decode_row::<Clarification>("clarification", "row", &raw)?;
            // Silence is never an answer; a row that is not `requested` is not
            // answerable either (ATH-HITL-019).
            if resolved.state != ClarificationState::Requested {
                return Err(PersistenceError::Conflict {
                    detail: format!(
                        "clarification {} is {}, not answerable",
                        id.as_str(),
                        resolved.state.as_str()
                    ),
                });
            }
            let mut answered = resolved;
            answered.answer = Some(answer.to_string());
            answered.answered_at = Some(answered_at.to_string());
            answered.state = ClarificationState::Answered;
            tx.put(
                tables::CLARIFICATIONS,
                id.as_str(),
                &encode_row("clarification", "row", &answered)?,
            )
        })
    }

    fn mark_expired(&mut self, id: &ClarificationId) -> Result<()> {
        self.commit(|tx| {
            let raw = tx
                .get(tables::CLARIFICATIONS, id.as_str())?
                .ok_or_else(|| PersistenceError::NotFound {
                    entity: "clarification",
                    id: id.as_str().into(),
                })?;
            let resolved = decode_row::<Clarification>("clarification", "row", &raw)?;
            let mut expired = resolved;
            expired.state = ClarificationState::Expired;
            tx.put(
                tables::CLARIFICATIONS,
                id.as_str(),
                &encode_row("clarification", "row", &expired)?,
            )
        })
    }
}

// --- MessageRepository --------------------------------------------------------------------

impl MessageRepository for AthenaStore {
    fn insert(&mut self, message: &MessageRow) -> Result<()> {
        self.commit(|tx| {
            if tx
                .get(tables::MESSAGES, message.message_id.as_str())?
                .is_some()
            {
                return Err(PersistenceError::Conflict {
                    detail: format!("message {} exists", message.message_id.as_str()),
                });
            }
            tx.put(
                tables::MESSAGES,
                message.message_id.as_str(),
                &encode_row("message", "row", message)?,
            )
        })
    }

    fn get(&self, id: &MessageId) -> Result<Option<MessageRow>> {
        self.read(|view| {
            view.get(tables::MESSAGES, id.as_str())?
                .map(|raw| decode_row::<MessageRow>("message", "row", &raw))
                .transpose()
        })
    }

    fn list_for_thread(&self, thread_id: &str) -> Result<Vec<MessageRow>> {
        self.read(|view| {
            let mut messages = Vec::new();
            for (_, raw) in view.rows(tables::MESSAGES)? {
                let resolved = decode_row::<MessageRow>("message", "row", &raw)?;
                if resolved.thread_id == thread_id {
                    messages.push(resolved);
                }
            }
            // `ORDER BY created_at, message_id`.
            messages.sort_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then(a.message_id.as_str().cmp(b.message_id.as_str()))
            });
            Ok(messages)
        })
    }

    fn mark_delivered(&mut self, to: &InstanceId, id: &MessageId) -> Result<bool> {
        self.commit(|tx| {
            let key = format!("{}|{}", to.as_str(), id.as_str());
            if tx.get(tables::MESSAGE_DELIVERIES, &key)?.is_some() {
                return Ok(false);
            }
            tx.put(
                tables::MESSAGE_DELIVERIES,
                &key,
                &serde_json::json!({ "delivered_at": crate::db::now_rfc3339() }).to_string(),
            )?;
            Ok(true)
        })
    }

    fn mark_accepted(&mut self, to: &InstanceId, id: &MessageId) -> Result<()> {
        self.commit(|tx| {
            let key = format!("{}|{}", to.as_str(), id.as_str());
            if let Some(raw) = tx.get(tables::MESSAGE_DELIVERIES, &key)? {
                let mut row = decode_row::<serde_json::Value>("message_delivery", "row", &raw)?;
                row["accepted_at"] = serde_json::json!(crate::db::now_rfc3339());
                tx.put(tables::MESSAGE_DELIVERIES, &key, &row.to_string())?;
            }
            Ok(())
        })
    }

    fn was_delivered(&self, to: &InstanceId, id: &MessageId) -> Result<bool> {
        self.read(|view| {
            Ok(view
                .get(
                    tables::MESSAGE_DELIVERIES,
                    &format!("{}|{}", to.as_str(), id.as_str()),
                )?
                .is_some())
        })
    }

    fn prune_deliveries_before(&mut self, cutoff_rfc3339: &str) -> Result<u64> {
        self.commit(|tx| {
            let mut removed = 0_u64;
            for (key, raw) in tx.rows(tables::MESSAGE_DELIVERIES)? {
                let row = decode_row::<serde_json::Value>("message_delivery", "row", &raw)?;
                if row["delivered_at"]
                    .as_str()
                    .is_some_and(|at| at < cutoff_rfc3339)
                {
                    let _ = tx.remove(tables::MESSAGE_DELIVERIES, &key)?;
                    removed += 1;
                }
            }
            Ok(removed)
        })
    }
}

// --- HierarchyRepository -----------------------------------------------------------------

impl HierarchyRepository for AthenaStore {
    fn insert(&mut self, hierarchy: &Hierarchy) -> Result<()> {
        // The settled tree is one whole row (its nodes ride the aggregate's own
        // serialization) — one unit, no cross-table ordering question at all.
        self.commit(|tx| {
            if tx
                .get(tables::HIERARCHIES, hierarchy.id.as_str())?
                .is_some()
            {
                return Err(PersistenceError::Conflict {
                    detail: format!("hierarchy {} exists", hierarchy.id.as_str()),
                });
            }
            tx.put(
                tables::HIERARCHIES,
                hierarchy.id.as_str(),
                &encode_row("hierarchy", "row", hierarchy)?,
            )
        })
    }

    fn get(&self, id: &HierarchyId) -> Result<Option<Hierarchy>> {
        self.read(|view| {
            view.get(tables::HIERARCHIES, id.as_str())?
                .map(|raw| decode_row::<Hierarchy>("hierarchy", "row", &raw))
                .transpose()
        })
    }

    fn latest_for_task(&self, task: &TaskId) -> Result<Option<Hierarchy>> {
        self.read(|view| {
            let mut best: Option<(u64, Hierarchy)> = None;
            for (_, raw) in view.rows(tables::HIERARCHIES)? {
                let resolved = decode_row::<Hierarchy>("hierarchy", "row", &raw)?;
                if resolved.task_id != *task {
                    continue;
                }
                let should_replace = best
                    .as_ref()
                    .is_none_or(|(current, _)| resolved.revision > *current);
                if should_replace {
                    best = Some((resolved.revision, resolved));
                }
            }
            Ok(best.map(|(_, hierarchy)| hierarchy))
        })
    }
}

// --- RuntimeInstanceRepository ------------------------------------------------------------

impl RuntimeInstanceRepository for AthenaStore {
    fn insert(&mut self, instance: &RuntimeInstance) -> Result<()> {
        self.commit(|tx| {
            if tx
                .get(tables::RUNTIME_INSTANCES, instance.id.as_str())?
                .is_some()
            {
                return Err(PersistenceError::Conflict {
                    detail: format!("runtime instance {} exists", instance.id.as_str()),
                });
            }
            tx.put(
                tables::RUNTIME_INSTANCES,
                instance.id.as_str(),
                &encode_row("runtime_instance", "row", instance)?,
            )
        })
    }

    fn get(&self, id: &RuntimeInstanceId) -> Result<Option<RuntimeInstance>> {
        self.read(|view| {
            view.get(tables::RUNTIME_INSTANCES, id.as_str())?
                .map(|raw| decode_row::<RuntimeInstance>("runtime_instance", "row", &raw))
                .transpose()
        })
    }

    fn record_heartbeat(&mut self, id: &RuntimeInstanceId, at: &str) -> Result<()> {
        self.commit(|tx| {
            let raw = tx
                .get(tables::RUNTIME_INSTANCES, id.as_str())?
                .ok_or_else(|| PersistenceError::NotFound {
                    entity: "runtime_instance",
                    id: id.as_str().into(),
                })?;
            let mut resolved = decode_row::<RuntimeInstance>("runtime_instance", "row", &raw)?;
            resolved.last_heartbeat_at = Some(at.to_string());
            tx.put(
                tables::RUNTIME_INSTANCES,
                id.as_str(),
                &encode_row("runtime_instance", "row", &resolved)?,
            )
        })
    }

    fn list_stale(&self, cutoff_rfc3339: &str) -> Result<Vec<RuntimeInstance>> {
        self.read(|view| {
            let stale = view
                .rows(tables::RUNTIME_INSTANCES)?
                .into_iter()
                .filter_map(|(_, raw)| {
                    decode_row::<RuntimeInstance>("runtime_instance", "row", &raw).ok()
                })
                .filter(|record| {
                    record
                        .last_heartbeat_at
                        .as_ref()
                        .is_some_and(|heartbeat| heartbeat.as_str() < cutoff_rfc3339)
                })
                .collect();
            Ok(stale)
        })
    }

    fn mark_stopped(
        &mut self,
        id: &RuntimeInstanceId,
        reason: StopReason,
        exit_code: Option<i32>,
        side_effects: SideEffects,
    ) -> Result<()> {
        self.commit(|tx| {
            let raw = tx
                .get(tables::RUNTIME_INSTANCES, id.as_str())?
                .ok_or_else(|| PersistenceError::NotFound {
                    entity: "runtime_instance",
                    id: id.as_str().into(),
                })?;
            let mut resolved = decode_row::<RuntimeInstance>("runtime_instance", "row", &raw)?;
            resolved.state = athena_types::AgentInstanceState::Stopped;
            resolved.stopped_at = Some(crate::db::now_rfc3339());
            resolved.stop_reason = Some(reason);
            resolved.exit_code = exit_code;
            resolved.side_effects = side_effects;
            tx.put(
                tables::RUNTIME_INSTANCES,
                id.as_str(),
                &encode_row("runtime_instance", "row", &resolved)?,
            )
        })
    }
}

// --- ToolExecutionRepository ---------------------------------------------------------------

impl ToolExecutionRepository for AthenaStore {
    fn insert(&mut self, execution: &ToolExecution) -> Result<()> {
        self.commit(|tx| {
            if tx
                .get(tables::TOOL_EXECUTIONS, execution.id.as_str())?
                .is_some()
            {
                return Err(PersistenceError::Conflict {
                    detail: format!("tool execution {} exists", execution.id.as_str()),
                });
            }
            tx.put(
                tables::TOOL_EXECUTIONS,
                execution.id.as_str(),
                &encode_row("tool_execution", "row", execution)?,
            )
        })
    }

    fn get(&self, id: &ToolExecutionId) -> Result<Option<ToolExecution>> {
        self.read(|view| {
            view.get(tables::TOOL_EXECUTIONS, id.as_str())?
                .map(|raw| decode_row::<ToolExecution>("tool_execution", "row", &raw))
                .transpose()
        })
    }

    fn update_status(&mut self, id: &ToolExecutionId, status: ToolExecutionStatus) -> Result<()> {
        self.commit(|tx| {
            let raw = tx
                .get(tables::TOOL_EXECUTIONS, id.as_str())?
                .ok_or_else(|| PersistenceError::NotFound {
                    entity: "tool_execution",
                    id: id.as_str().to_string(),
                })?;
            let mut resolved = decode_row::<ToolExecution>("tool_execution", "row", &raw)?;
            resolved.status = status;
            // `Denied` never stamps a finish (P4-02's note).
            if !matches!(
                status,
                ToolExecutionStatus::Started | ToolExecutionStatus::Denied
            ) {
                resolved.finished_at = Some(crate::db::now_rfc3339());
            }
            tx.put(
                tables::TOOL_EXECUTIONS,
                id.as_str(),
                &encode_row("tool_execution", "row", &resolved)?,
            )
        })
    }

    fn list_for_instance(&self, instance: &InstanceId) -> Result<Vec<ToolExecution>> {
        self.read(|view| {
            let mut executions = Vec::new();
            for (_, raw) in view.rows(tables::TOOL_EXECUTIONS)? {
                let resolved = decode_row::<ToolExecution>("tool_execution", "row", &raw)?;
                if resolved.instance_id == *instance {
                    executions.push(resolved);
                }
            }
            // `ORDER BY started_at, id`.
            executions.sort_by(|a, b| {
                a.started_at
                    .cmp(&b.started_at)
                    .then(a.id.as_str().cmp(b.id.as_str()))
            });
            Ok(executions)
        })
    }
}

// --- ConversationRepository ---------------------------------------------------------------

impl ConversationRepository for AthenaStore {
    fn insert(&mut self, conversation: &Conversation) -> Result<()> {
        self.commit(|tx| {
            if tx
                .get(tables::CONVERSATIONS, conversation.id.as_str())?
                .is_some()
            {
                return Err(PersistenceError::Conflict {
                    detail: format!("conversation {} already exists", conversation.id.as_str()),
                });
            }
            tx.put(
                tables::CONVERSATIONS,
                conversation.id.as_str(),
                &encode_row("conversation", "row", conversation)?,
            )
        })
    }

    fn get(&self, id: &ConversationId) -> Result<Option<Conversation>> {
        self.read(|view| {
            view.get(tables::CONVERSATIONS, id.as_str())?
                .map(|raw| decode_row::<Conversation>("conversation", "row", &raw))
                .transpose()
        })
    }

    fn link_task(&mut self, id: &ConversationId, task: &TaskId) -> Result<()> {
        self.commit(|tx| {
            let raw = tx.get(tables::CONVERSATIONS, id.as_str())?.ok_or_else(|| {
                PersistenceError::NotFound {
                    entity: "conversation",
                    id: id.as_str().into(),
                }
            })?;
            let mut resolved = decode_row::<Conversation>("conversation", "row", &raw)?;
            resolved.task_id = Some(task.clone());
            tx.put(
                tables::CONVERSATIONS,
                id.as_str(),
                &encode_row("conversation", "row", &resolved)?,
            )
        })
    }

    fn append_utterance(&mut self, utterance: &Utterance) -> Result<()> {
        self.commit(|tx| {
            // The conversation row must exist; the ordinal is the next slot in the
            // conversation's own trail, `pad`-keyed so ordered scans stay ordered.
            if tx
                .get(tables::CONVERSATIONS, utterance.conversation_id.as_str())?
                .is_none()
            {
                return Err(PersistenceError::NotFound {
                    entity: "conversation",
                    id: utterance.conversation_id.as_str().into(),
                });
            }
            if tx.get(tables::UTTERANCES, utterance.id.as_str())?.is_some() {
                return Err(PersistenceError::Conflict {
                    detail: format!("utterance {} exists", utterance.id.as_str()),
                });
            }
            tx.put(
                tables::UTTERANCES,
                &format!(
                    "{}|{}",
                    utterance.conversation_id.as_str(),
                    utterance.id.as_str()
                ),
                &encode_row("utterance", "row", utterance)?,
            )
        })
    }

    fn list_utterances(&self, conversation: &ConversationId) -> Result<Vec<Utterance>> {
        self.read(|view| {
            let prefix = format!("{}|", conversation.as_str());
            let mut utterances = Vec::new();
            for (key, raw) in view.rows(tables::UTTERANCES)? {
                if key.starts_with(&prefix) {
                    utterances.push(decode_row::<Utterance>("utterance", "row", &raw)?);
                }
            }
            utterances.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
            Ok(utterances)
        })
    }
}

// --- the atomic primitives of the store itself (the State Manager's face) -------------------

impl AthenaStore {
    /// Apply a state transition and its Event atomically (ATH-PERS-003), with the
    /// optimistic concurrency check (`ATH-STATE-014`): a mismatch refuses the
    /// transition, writes nothing, appends nothing.
    pub fn transition_task(
        &mut self,
        task: &TaskId,
        expected: TaskState,
        to: TaskState,
        event: &NewEvent,
    ) -> Result<StoredEvent> {
        self.commit(|tx| {
            let raw = tx.get(tables::TASKS, task.as_str())?.ok_or_else(|| {
                PersistenceError::NotFound {
                    entity: "task",
                    id: task.to_string(),
                }
            })?;
            let mut resolved = decode_row::<Task>("task", "row", &raw)?;
            if resolved.state != expected {
                return Err(PersistenceError::Conflict {
                    detail: format!(
                        "task {task} is in {}, not the expected {}; \
                         transition refused (ATH-STATE-014)",
                        resolved.state.as_str(),
                        expected.as_str()
                    ),
                });
            }
            resolved.state = to;
            resolved.updated_at = event.occurred_at.clone();
            tx.put(
                tables::TASKS,
                task.as_str(),
                &encode_row("task", "row", &resolved)?,
            )?;
            append_event_tx(tx, event)
        })
    }

    /// Create a Task together with its `TaskCreated` Event in one unit (ATH-PERS-003).
    pub fn create_task_with_event(
        &mut self,
        task: &Task,
        event_id: &EventId,
        occurred_at: &str,
        correlation_id: &str,
    ) -> Result<StoredEvent> {
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
                "instruction_digest": digest_of(task.instruction.as_str()),
                "workspace": task.workspace,
                "priority": task.priority.as_str(),
                "created_by": task.created_by,
            })
            .to_string(),
        };
        self.commit(|tx| {
            if tx.get(tables::TASKS, task.id.as_str())?.is_some() {
                return Err(PersistenceError::Conflict {
                    detail: format!("task {} exists", task.id.as_str()),
                });
            }
            tx.put(
                tables::TASKS,
                task.id.as_str(),
                &encode_row("task", "row", task)?,
            )?;
            append_event_tx(tx, &event)
        })
    }

    /// Finalize one reconciliation row atomically (recovery's write), the §17.2/§17.5
    /// triggers' resolved shape (ATH-CORE-048).
    pub fn reconcile_instance(
        &mut self,
        instance: &AgentInstance,
        final_state: AgentInstanceState,
        event_type: &str,
        orphan_step: Option<&PlanStepId>,
        occurred_at: &str,
        correlation_id: &str,
    ) -> Result<StoredEvent> {
        self.commit(|tx| {
            let raw = tx
                .get(tables::AGENT_INSTANCES, instance.id.as_str())?
                .ok_or_else(|| PersistenceError::NotFound {
                    entity: "agent_instance",
                    id: instance.id.as_str().into(),
                })?;
            let mut resolved = decode_row::<AgentInstance>("agent_instance", "row", &raw)?;
            resolved.state = final_state;
            tx.put(
                tables::AGENT_INSTANCES,
                instance.id.as_str(),
                &encode_row("agent_instance", "row", &resolved)?,
            )?;

            let mut step_event: Option<StoredEvent> = None;
            if let Some(step) = orphan_step {
                let step_raw = tx.get(tables::PLAN_STEPS, step.as_str())?.ok_or_else(|| {
                    PersistenceError::NotFound {
                        entity: "plan_step",
                        id: step.to_string(),
                    }
                })?;
                let mut orphaned = decode_row::<PlanStep>("plan_step", "row", &step_raw)?;
                orphaned.state = PlanStepState::Orphaned;
                tx.put(
                    tables::PLAN_STEPS,
                    step.as_str(),
                    &encode_row("plan_step", "row", &orphaned)?,
                )?;
                let orphan_event = NewEvent {
                    event_id: EventId::new(),
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
                        "side_effects": "unknown",
                    })
                    .to_string(),
                };
                step_event = Some(append_event_tx(tx, &orphan_event)?);
            }

            let failure = NewEvent {
                event_id: EventId::new(),
                schema_version: 1,
                event_type: event_type.to_string(),
                occurred_at: occurred_at.to_string(),
                task_id: instance.task_id.clone(),
                plan_step_id: instance.plan_step_id.clone(),
                agent_id: Some(instance.agent_id.clone()),
                instance_id: Some(instance.id.clone()),
                skill_id: None,
                correlation_id: correlation_id.to_string(),
                causation_id: step_event.map(|event| event.event_id.clone()),
                actor: "system".into(),
                payload_json: serde_json::json!({
                    "error_class": "internal_error",
                    "attempt": instance.attempt,
                    "side_effects": "unknown",
                })
                .to_string(),
            };
            append_event_tx(tx, &failure)
        })
    }

    /// The insert path the API reads after `create_task_with_event` — the same
    /// transition offering (no separate surface).
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

/// The §23.2 `TaskCreated` payload digest (SHA-256 first 16 bytes hex).
#[must_use]
pub fn digest_of(text: &str) -> String {
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
