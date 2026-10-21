//! Database handle, migrations and the transactional primitive (spec section 27.2).
//!
//! The persistent store is authoritative and in-memory state is derived (ATH-PERS-002), which
//! is what makes crash recovery a reload rather than a reconstruction.
//!
//! The engine is [`redb`] — a pure-Rust embedded ACID store, file-backed (or in-memory for
//! tests). There is no SQL and there is no SQLite anywhere: its use is prohibited by the
//! instruction source (最優先事項, 2026-10-18). Every durability clause of section 27
//! stands: each commit is a durable, atomic, multi-table write unit; the transition+Event
//! pair commits inside one such unit (ATH-PERS-003); `events`/`results` have no write or
//! delete path other than the retention sweep and the append.

use crate::error::{PersistenceError, Result};
use redb::{ReadableTable, TableDefinition};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Every logical table of the §27.3 data model, engine-natively (R-PRS-04).
///
/// A table is keyed by its row's identifier (or a composite key written
/// `first|second`); the value is the row's full entity as JSON text. Ordering comes
/// from the key: prefixed-ULID identifiers are lexicographically chronological, and
/// composite keys pad their numeric parts to a fixed width so ordered scans stay
/// ordered.
pub mod tables {
    use redb::TableDefinition;

    /// Tasks (spec section 26.2).
    pub const TASKS: TableDefinition<String, String> = TableDefinition::new("tasks");
    /// Plans (row: plan meta, no steps).
    pub const PLANS: TableDefinition<String, String> = TableDefinition::new("plans");
    /// Plan revisions by Task: `task_id|revision-padded` → `plan_id`.
    pub const PLANS_BY_REV: TableDefinition<String, String> = TableDefinition::new("plans_by_rev");
    /// Plan identifiers → their revision key (the `get(plan_id)` face).
    pub const PLANS_BY_ID: TableDefinition<String, String> = TableDefinition::new("plans_by_id");
    /// PlanSteps.
    pub const PLAN_STEPS: TableDefinition<String, String> = TableDefinition::new("plan_steps");
    /// Steps by Plan: `plan_id|step_order-padded` → `step_id`.
    pub const STEPS_BY_PLAN: TableDefinition<String, String> =
        TableDefinition::new("steps_by_plan");
    /// PlanStep attempts, keyed `step_id|attempt-padded`; the history is never rewritten.
    pub const PLAN_STEP_ATTEMPTS: TableDefinition<String, String> =
        TableDefinition::new("plan_step_attempts");
    /// Events, keyed by identifier (ULID-chronological). Append-only (R-PRS-05).
    pub const EVENTS: TableDefinition<String, String> = TableDefinition::new("events");
    /// Events by Task: `task_id|event_id` → `event_id`.
    pub const EVENTS_BY_TASK: TableDefinition<String, String> =
        TableDefinition::new("events_by_task");
    /// Events by sequence: `sequence-padded` → `event_id`.
    pub const EVENTS_BY_SEQ: TableDefinition<String, String> =
        TableDefinition::new("events_by_seq");
    /// Meta: the engine's own counters (`event_sequence`).
    pub const META: TableDefinition<String, String> = TableDefinition::new("meta");
    /// Idempotency keys (ATH-CORE-035); retained 7 days (R-PRS-06's table set).
    pub const IDEMPOTENCY_KEYS: TableDefinition<String, String> =
        TableDefinition::new("idempotency_keys");
    /// Workspace leases (ATH-DATA-009).
    pub const WORKSPACE_LEASES: TableDefinition<String, String> =
        TableDefinition::new("workspace_leases");
    /// Agent definitions.
    pub const AGENT_DEFINITIONS: TableDefinition<String, String> =
        TableDefinition::new("agent_definitions");
    /// Agent instances.
    pub const AGENT_INSTANCES: TableDefinition<String, String> =
        TableDefinition::new("agent_instances");
    /// Results. Append-only (R-PRS-05).
    pub const RESULTS: TableDefinition<String, String> = TableDefinition::new("results");
    /// Evaluations.
    pub const EVALUATIONS: TableDefinition<String, String> = TableDefinition::new("evaluations");
    /// Evaluations by Result: `result_id|evaluation_id` → `evaluation_id`.
    pub const EVALUATIONS_BY_RESULT: TableDefinition<String, String> =
        TableDefinition::new("evaluations_by_result");
    /// Skills, keyed `id|version`.
    pub const SKILLS: TableDefinition<String, String> = TableDefinition::new("skills");
    /// Approvals.
    pub const APPROVALS: TableDefinition<String, String> = TableDefinition::new("approvals");
    /// Memory records.
    pub const MEMORY_RECORDS: TableDefinition<String, String> =
        TableDefinition::new("memory_records");
    /// Clarifications.
    pub const CLARIFICATIONS: TableDefinition<String, String> =
        TableDefinition::new("clarifications");
    /// Messages.
    pub const MESSAGES: TableDefinition<String, String> = TableDefinition::new("messages");
    /// Message-delivery dedup rows, keyed `to_instance_id|message_id` (ATH-MSG-014).
    pub const MESSAGE_DELIVERIES: TableDefinition<String, String> =
        TableDefinition::new("message_deliveries");
    /// Hierarchies (one row carries the node set).
    pub const HIERARCHIES: TableDefinition<String, String> = TableDefinition::new("hierarchies");
    /// Runtime instances.
    pub const RUNTIME_INSTANCES: TableDefinition<String, String> =
        TableDefinition::new("runtime_instances");
    /// Tool executions.
    pub const TOOL_EXECUTIONS: TableDefinition<String, String> =
        TableDefinition::new("tool_executions");
    /// Conversations.
    pub const CONVERSATIONS: TableDefinition<String, String> =
        TableDefinition::new("conversations");
    /// Utterances, keyed `conversation_id|ordinal-padded`.
    pub const UTTERANCES: TableDefinition<String, String> = TableDefinition::new("utterances");
    /// The data's own schema-version record (R-PRS-06).
    pub const SCHEMA_VERSION: TableDefinition<String, String> =
        TableDefinition::new("schema_version");
}

/// A handle to the ATHENA database.
pub struct Database {
    engine: redb::Database,
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database").finish_non_exhaustive()
    }
}

impl Database {
    /// Open or create the database at `path` and bring it to the current schema.
    ///
    /// # Errors
    /// Engine open, or schema bring-up.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|source| PersistenceError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let engine = redb::Database::create(path)?;
        Self::from_engine(engine)
    }

    /// Open an in-memory database, for tests.
    ///
    /// # Errors
    /// Engine open, or schema bring-up.
    pub fn open_in_memory() -> Result<Self> {
        let engine = redb::backends::InMemoryBackend::new();
        let engine = redb::Database::builder().create_with_backend(engine)?;
        Self::from_engine(engine)
    }

    fn from_engine(engine: redb::Database) -> Result<Self> {
        let db = Self { engine };
        db.migrate()?;
        Ok(db)
    }

    /// Bring the database to the current schema version (`ATH-PERS-009`/`010`): a fresh
    /// or older stamp moves forward; a newer stamp aborts startup — silent operation on
    /// a mismatched schema is forbidden. The check runs inside one write unit (a fresh
    /// database has no version table; the write face auto-creates).
    fn migrate(&self) -> Result<()> {
        let current = crate::CURRENT_SCHEMA_VERSION;
        let txn = self.engine.begin_write()?;
        let mut table = txn.open_table(tables::SCHEMA_VERSION)?;
        let moved = table
            .get("version".to_string())?
            .and_then(|guard| guard.value().parse::<u32>().ok())
            .unwrap_or(0);
        if moved > current {
            drop(table);
            drop(txn); // the refuser's unit commits nothing
            return Err(PersistenceError::SchemaTooNew {
                found: moved,
                supported: current,
            });
        }
        if moved != current {
            table.insert("version".to_string(), current.to_string())?;
        }
        drop(table);
        txn.commit()?;
        Ok(())
    }

    /// Commit one atomic unit: everything the closure writes commits, or nothing does
    /// (`ATH-PERS-003`). Write transactions see their own writes, so
    /// check-then-write transitions stay conflict-checkable inside the unit.
    ///
    /// # Errors
    /// The closure's own faults, or the unit's commit fault.
    pub fn commit<R>(&mut self, f: impl FnOnce(&mut WriteTx) -> Result<R>) -> Result<R> {
        let txn = self.engine.begin_write()?;
        let outcome = {
            let mut tx = WriteTx { txn: &txn };
            f(&mut tx)
        };
        match outcome {
            Ok(value) => {
                txn.commit()?; // consumes txn — the unit is atomic everything-or-nothing
                Ok(value)
            }
            Err(fault) => {
                drop(txn); // an uncommitted write transaction aborts on drop
                Err(fault)
            }
        }
    }

    /// Read without committing anything.
    ///
    /// # Errors
    /// The closure's own faults.
    pub fn read<R>(&self, f: impl FnOnce(&ReadTx<'_, '_>) -> Result<R>) -> Result<R> {
        let txn = self.engine.begin_read()?;
        let view = ReadTx {
            txn: &txn,
            _marker: std::marker::PhantomData,
        };
        let outcome = f(&view);
        drop(txn);
        outcome
    }
}

/// The written half of one atomic unit.
pub struct WriteTx<'e> {
    txn: &'e redb::WriteTransaction,
}

impl<'e> WriteTx<'e> {
    fn open<'t>(
        &'t self,
        definition: TableDefinition<String, String>,
    ) -> Result<redb::Table<'t, String, String>> {
        self.txn
            .open_table(definition)
            .map_err(PersistenceError::from)
    }

    /// Insert (or replace) a row.
    ///
    /// # Errors
    /// Engine faults.
    pub fn put(
        &mut self,
        definition: TableDefinition<String, String>,
        key: &str,
        value: &str,
    ) -> Result<()> {
        let mut table = self.open(definition)?;
        table
            .insert(key.to_string(), value.to_string())
            .map_err(PersistenceError::from)?;
        Ok(())
    }

    /// Read one row inside the unit.
    ///
    /// # Errors
    /// Engine faults.
    pub fn get(
        &self,
        definition: TableDefinition<String, String>,
        key: &str,
    ) -> Result<Option<String>> {
        let table = self.open(definition)?;
        let guard = table.get(key.to_string()).map_err(PersistenceError::from)?;
        Ok(guard.map(|row| row.value().to_string()))
    }

    /// Remove one row; returns whether a row was removed.
    ///
    /// # Errors
    /// Engine faults.
    pub fn remove(
        &mut self,
        definition: TableDefinition<String, String>,
        key: &str,
    ) -> Result<bool> {
        let mut table = self.open(definition)?;
        let prior = table
            .remove(key.to_string())
            .map_err(PersistenceError::from)?;
        Ok(prior.is_some())
    }

    /// Insert only when the key is absent; `false` means the key already existed.
    ///
    /// This is the atomic claim/lease primitive (`ATH-CORE-035`, `ATH-NFR-017`,
    /// `ATH-DATA-009`).
    ///
    /// # Errors
    /// Engine faults.
    pub fn insert_absent(
        &mut self,
        definition: TableDefinition<String, String>,
        key: &str,
        value: &str,
    ) -> Result<bool> {
        let mut table = self.open(definition)?;
        let prior = table
            .insert(key.to_string(), value.to_string())
            .map_err(PersistenceError::from)?;
        Ok(prior.is_none())
    }

    /// Iterate one table's rows in key order.
    ///
    /// # Errors
    /// Engine faults.
    pub fn rows(
        &self,
        definition: TableDefinition<String, String>,
    ) -> Result<Vec<(String, String)>> {
        let table = self.open(definition)?;
        let mut rows = Vec::new();
        for entry in table.iter()? {
            let (key, value) = entry?;
            rows.push((key.value().to_string(), value.value().to_string()));
        }
        Ok(rows)
    }

    /// Iterate one table's rows reversed (newest first).
    ///
    /// # Errors
    /// Engine faults.
    pub fn rows_rev(
        &self,
        definition: TableDefinition<String, String>,
    ) -> Result<Vec<(String, String)>> {
        let table = self.open(definition)?;
        let mut rows = Vec::new();
        for entry in table.iter()?.rev() {
            let (key, value) = entry?;
            rows.push((key.value().to_string(), value.value().to_string()));
        }
        Ok(rows)
    }
}

/// The read-only view.
pub struct ReadTx<'e, 'a> {
    _marker: std::marker::PhantomData<&'a ()>,
    txn: &'e redb::ReadTransaction,
}

impl<'e, 'a> ReadTx<'e, 'a> {
    /// Read one row.
    ///
    /// # Errors
    /// Engine faults.
    pub fn get(
        &self,
        definition: TableDefinition<String, String>,
        key: &str,
    ) -> Result<Option<String>> {
        // A table that was never committed to is an absent table, not an internal
        // fault: an absent row answers None the same way an absent row does.
        let Ok(table) = self.txn.open_table(definition) else {
            return Ok(None);
        };
        let guard = table.get(key.to_string()).map_err(PersistenceError::from)?;
        Ok(guard.map(|row| row.value().to_string()))
    }

    /// Iterate one table's rows in key order.
    ///
    /// # Errors
    /// Engine faults.
    pub fn rows(
        &self,
        definition: TableDefinition<String, String>,
    ) -> Result<Vec<(String, String)>> {
        let Ok(table) = self.txn.open_table(definition) else {
            return Ok(Vec::new());
        };
        let mut rows = Vec::new();
        for entry in table.iter()? {
            let (key, value) = entry?;
            rows.push((key.value().to_string(), value.value().to_string()));
        }
        Ok(rows)
    }

    /// Iterate one table's rows reversed (newest first).
    ///
    /// # Errors
    /// Engine faults.
    pub fn rows_rev(
        &self,
        definition: TableDefinition<String, String>,
    ) -> Result<Vec<(String, String)>> {
        let Ok(table) = self.txn.open_table(definition) else {
            return Ok(Vec::new());
        };
        let mut rows = Vec::new();
        for entry in table.iter()?.rev() {
            let (key, value) = entry?;
            rows.push((key.value().to_string(), value.value().to_string()));
        }
        Ok(rows)
    }
}

/// Now, RFC 3339 UTC.
#[must_use]
pub fn now_rfc3339() -> String {
    format_rfc3339(SystemTime::now())
}

/// Format a system time as RFC 3339 UTC (millisecond precision, `Z` suffix).
#[must_use]
pub fn format_rfc3339(when: SystemTime) -> String {
    let since_epoch = when
        .duration_since(UNIX_EPOCH)
        .unwrap_or(std::time::Duration::ZERO);
    time_string(since_epoch.as_secs() as i64, since_epoch.subsec_millis())
}

/// Seconds+millis → RFC 3339.
fn time_string(seconds: i64, millis: u32) -> String {
    let days = seconds.div_euclid(86_400);
    let second_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = second_of_day / 3_600;
    let minute = (second_of_day % 3_600) / 60;
    let second = second_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Days since the epoch → `(year, month, day)` (civil calendar; the Hinnant algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (y + i64::from(m <= 2), m as u32, d as u32)
}

impl From<redb::DatabaseError> for PersistenceError {
    fn from(error: redb::DatabaseError) -> Self {
        Self::Backend {
            detail: format!("DatabaseError: {error}"),
        }
    }
}

impl From<redb::TransactionError> for PersistenceError {
    fn from(error: redb::TransactionError) -> Self {
        Self::Backend {
            detail: format!("TransactionError: {error}"),
        }
    }
}

impl From<redb::StorageError> for PersistenceError {
    fn from(error: redb::StorageError) -> Self {
        Self::Backend {
            detail: format!("StorageError: {error}"),
        }
    }
}

impl From<redb::CommitError> for PersistenceError {
    fn from(error: redb::CommitError) -> Self {
        Self::Backend {
            detail: format!("CommitError: {error}"),
        }
    }
}

impl From<redb::TableError> for PersistenceError {
    fn from(error: redb::TableError) -> Self {
        Self::Backend {
            detail: format!("TableError: {error}"),
        }
    }
}
