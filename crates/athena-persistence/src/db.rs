//! Database handle, migrations and the transactional primitive (spec section 27.2).
//!
//! The persistent store is authoritative and in-memory state is derived (ATH-PERS-002), which
//! is what makes crash recovery a reload rather than a reconstruction.

use crate::ddl::{CURRENT_SCHEMA_VERSION, MIGRATIONS};
use crate::error::{PersistenceError, Result};
use rusqlite::{Connection, OpenFlags, Transaction};
use std::path::Path;

/// A handle to the ATHENA database.
pub struct Database {
    conn: Connection,
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database").finish_non_exhaustive()
    }
}

impl Database {
    /// Open or create the database at `path`, applying pragmas and migrations.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|source| PersistenceError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_URI,
        )?;
        Self::from_connection(conn)
    }

    /// Open an in-memory database, for tests.
    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        let mut db = Self { conn };
        db.apply_pragmas()?;
        db.migrate()?;
        Ok(db)
    }

    /// Apply the required pragmas (ATH-PERS-005).
    fn apply_pragmas(&mut self) -> Result<()> {
        // WAL with synchronous=FULL is the specified pairing: WAL for concurrent readers,
        // FULL so a committed transition survives a power loss. Anything less would let a
        // state transition be acknowledged and then lost, breaking ATH-PERS-002.
        //
        // An in-memory database cannot use WAL, so the mode is applied best-effort and
        // verified only for file-backed databases.
        let _: String = self
            .conn
            .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
            .unwrap_or_else(|_| "memory".to_string());
        self.conn.pragma_update(None, "synchronous", "FULL")?;
        // Referential integrity is declared in the schema, so it must actually be enforced;
        // SQLite leaves it off by default.
        self.conn.pragma_update(None, "foreign_keys", true)?;
        Ok(())
    }

    /// The schema version recorded in the database, or `None` for a fresh database.
    pub fn recorded_version(&self) -> Result<Option<u32>> {
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_migrations')",
            [],
            |row| row.get(0),
        )?;
        if !exists {
            return Ok(None);
        }
        let version: Option<i64> =
            self.conn
                .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
                    row.get(0)
                })?;
        Ok(version.map(|v| u32::try_from(v).unwrap_or(u32::MAX)))
    }

    /// Verify the schema version and migrate forward, or abort (ATH-PERS-010).
    ///
    /// A database newer than this build is refused rather than used: proceeding would mean
    /// writing through a schema the build does not understand.
    fn migrate(&mut self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                 version     INTEGER PRIMARY KEY,
                 description TEXT NOT NULL,
                 applied_at  TEXT NOT NULL
             ) STRICT;",
        )?;

        let current = self.recorded_version()?.unwrap_or(0);
        if current > CURRENT_SCHEMA_VERSION {
            return Err(PersistenceError::SchemaTooNew {
                found: current,
                supported: CURRENT_SCHEMA_VERSION,
            });
        }

        for migration in MIGRATIONS.iter().filter(|m| m.version > current) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(migration.sql)?;
            tx.execute(
                "INSERT INTO schema_migrations (version, description, applied_at)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![migration.version, migration.description, now_rfc3339()],
            )?;
            tx.commit()?;
        }
        Ok(())
    }

    /// Run `work` inside a single transaction.
    ///
    /// This is the primitive behind ATH-PERS-003: a state transition and its Event are written
    /// atomically, so a transition that succeeds while its Event is lost cannot occur. Callers
    /// publish to subscribers only *after* this returns, which is also what ATH-EVENT-020
    /// requires.
    pub fn transaction<T, F>(&mut self, work: F) -> Result<T>
    where
        F: FnOnce(&Transaction<'_>) -> Result<T>,
    {
        let tx = self.conn.transaction()?;
        match work(&tx) {
            Ok(value) => {
                tx.commit()?;
                Ok(value)
            }
            Err(e) => {
                // Dropping the transaction rolls it back; being explicit documents that a
                // failed unit of work leaves no partial state behind.
                drop(tx);
                Err(e)
            }
        }
    }

    /// Borrow the connection for read-only queries.
    #[must_use]
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Borrow the connection mutably.
    pub fn connection_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }
}

/// The current time as an RFC 3339 UTC string (ATH-DATA-002).
///
/// Implemented without a date-time dependency: the specification fixes the representation, not
/// the library.
#[must_use]
pub fn now_rfc3339() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format_rfc3339(now.as_secs(), now.subsec_millis())
}

/// Format a Unix timestamp as RFC 3339 UTC with millisecond precision.
#[must_use]
pub fn format_rfc3339(unix_secs: u64, millis: u32) -> String {
    // Days from 1970-01-01, converted with the civil-from-days algorithm.
    let days = i64::try_from(unix_secs / 86_400).unwrap_or(0);
    let secs_of_day = unix_secs % 86_400;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y,
        m,
        d,
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
        millis
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().expect("open in-memory database")
    }

    #[test]
    fn a_fresh_database_is_migrated_to_the_current_version() {
        let db = db();
        assert_eq!(
            db.recorded_version().expect("version"),
            Some(CURRENT_SCHEMA_VERSION)
        );
    }

    #[test]
    fn migration_is_idempotent_across_reopen() {
        // Reopening must not re-apply a migration; a second CREATE TABLE would fail.
        let dir = std::env::temp_dir().join(format!("athena-db-{}", std::process::id()));
        let path = dir.join("athena.db");
        let _ = std::fs::remove_dir_all(&dir);

        {
            let db = Database::open(&path).expect("open");
            assert_eq!(db.recorded_version().expect("version"), Some(1));
        }
        {
            let db = Database::open(&path).expect("reopen");
            assert_eq!(db.recorded_version().expect("version"), Some(1));
            let applied: i64 = db
                .connection()
                .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
                .expect("count");
            assert_eq!(applied, 1, "a migration must be applied exactly once");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_database_newer_than_the_build_is_refused() {
        // ATH-PERS-010: never operate silently against an unknown schema.
        let dir = std::env::temp_dir().join(format!("athena-db-new-{}", std::process::id()));
        let path = dir.join("athena.db");
        let _ = std::fs::remove_dir_all(&dir);

        {
            let db = Database::open(&path).expect("open");
            db.connection()
                .execute(
                    "INSERT INTO schema_migrations (version, description, applied_at)
                     VALUES (999, 'from the future', '2030-01-01T00:00:00.000Z')",
                    [],
                )
                .expect("insert future version");
        }
        let err = Database::open(&path).expect_err("must refuse");
        assert!(
            matches!(err, PersistenceError::SchemaTooNew { found: 999, .. }),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_backed_databases_run_in_wal_mode_with_full_synchronous() {
        // ATH-PERS-005.
        let dir = std::env::temp_dir().join(format!("athena-db-wal-{}", std::process::id()));
        let path = dir.join("athena.db");
        let _ = std::fs::remove_dir_all(&dir);

        let db = Database::open(&path).expect("open");
        let mode: String = db
            .connection()
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .expect("journal_mode");
        assert_eq!(mode.to_lowercase(), "wal");
        let sync: i64 = db
            .connection()
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .expect("synchronous");
        assert_eq!(sync, 2, "synchronous=FULL is 2");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn foreign_keys_are_enforced() {
        // The schema declares references, so they must actually hold.
        let db = db();
        let on: i64 = db
            .connection()
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .expect("foreign_keys");
        assert_eq!(on, 1);

        let err = db.connection().execute(
            "INSERT INTO plans (id, task_id, revision, goal, created_at)
             VALUES ('plan-1', 'task-missing', 1, 'g', '2026-10-14T00:00:00.000Z')",
            [],
        );
        assert!(err.is_err(), "a plan referencing no task must be refused");
    }

    #[test]
    fn a_committed_transaction_persists_and_a_failed_one_leaves_nothing() {
        // ATH-PERS-003's primitive: all or nothing.
        let mut db = db();
        db.transaction(|tx| {
            tx.execute(
                "INSERT INTO tasks (id, title, instruction, workspace, state, priority,
                     risk_level, max_attempts, max_iterations, created_by, created_at, updated_at)
                 VALUES ('task-1', 't', 'i', '/w', 'CREATED', 'normal', 'low', 3, 50, 'u',
                     '2026-10-14T00:00:00.000Z', '2026-10-14T00:00:00.000Z')",
                [],
            )?;
            Ok(())
        })
        .expect("commit");

        let count: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
            .expect("count");
        assert_eq!(count, 1);

        let failed: Result<()> = db.transaction(|tx| {
            tx.execute(
                "INSERT INTO tasks (id, title, instruction, workspace, state, priority,
                     risk_level, max_attempts, max_iterations, created_by, created_at, updated_at)
                 VALUES ('task-2', 't', 'i', '/w', 'CREATED', 'normal', 'low', 3, 50, 'u',
                     '2026-10-14T00:00:00.000Z', '2026-10-14T00:00:00.000Z')",
                [],
            )?;
            Err(PersistenceError::Conflict {
                detail: "deliberate".into(),
            })
        });
        assert!(failed.is_err());

        let count: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
            .expect("count");
        assert_eq!(count, 1, "the failed transaction must leave no row behind");
    }

    #[test]
    fn events_cannot_be_updated() {
        // ATH-PERS-008: history is append-only, enforced in the database so no code path can
        // rewrite it.
        let db = db();
        db.connection()
            .execute(
                "INSERT INTO events (event_id, schema_version, event_type, occurred_at, sequence,
                     correlation_id, actor)
                 VALUES ('evt-1', 1, 'TaskCreated', '2026-10-14T00:00:00.000Z', 1, 'cor-1',
                     'human')",
                [],
            )
            .expect("append");

        let err = db
            .connection()
            .execute(
                "UPDATE events SET event_type = 'TaskFailed' WHERE event_id = 'evt-1'",
                [],
            )
            .expect_err("update must be refused");
        assert!(err.to_string().contains("append-only"), "{err}");
    }

    #[test]
    fn results_and_artifacts_cannot_be_updated() {
        // ATH-RESULT-003: a correction is a new Result.
        let db = db();
        db.connection()
            .execute_batch(
                "INSERT INTO tasks (id, title, instruction, workspace, state, priority,
                     risk_level, max_attempts, max_iterations, created_by, created_at, updated_at)
                 VALUES ('task-1', 't', 'i', '/w', 'RUNNING', 'normal', 'low', 3, 50, 'u',
                     '2026-10-14T00:00:00.000Z', '2026-10-14T00:00:00.000Z');
                 INSERT INTO results (id, task_id, agent_id, instance_id, status, output,
                     started_at, finished_at, execution_time_ms)
                 VALUES ('res-1', 'task-1', 'a', 'ai-1', 'success', 'out',
                     '2026-10-14T00:00:00.000Z', '2026-10-14T00:00:01.000Z', 1000);
                 INSERT INTO artifacts (result_id, base, path, kind, created_at)
                 VALUES ('res-1', 'store', 'report.md', 'report', '2026-10-14T00:00:01.000Z');",
            )
            .expect("seed");

        let err = db
            .connection()
            .execute(
                "UPDATE results SET output = 'tampered' WHERE id = 'res-1'",
                [],
            )
            .expect_err("must be refused");
        assert!(err.to_string().contains("immutable"), "{err}");

        let err = db
            .connection()
            .execute(
                "UPDATE artifacts SET path = 'other.md' WHERE result_id = 'res-1'",
                [],
            )
            .expect_err("must be refused");
        assert!(err.to_string().contains("immutable"), "{err}");
    }

    #[test]
    fn an_artifact_base_outside_the_two_permitted_values_is_refused() {
        // ATH-RESULT-002: a Workspace-relative base must be unrepresentable, because Instance
        // areas are deleted at retention expiry and the reference would dangle.
        let db = db();
        db.connection()
            .execute_batch(
                "INSERT INTO tasks (id, title, instruction, workspace, state, priority,
                     risk_level, max_attempts, max_iterations, created_by, created_at, updated_at)
                 VALUES ('task-1', 't', 'i', '/w', 'RUNNING', 'normal', 'low', 3, 50, 'u',
                     '2026-10-14T00:00:00.000Z', '2026-10-14T00:00:00.000Z');
                 INSERT INTO results (id, task_id, agent_id, instance_id, status, output,
                     started_at, finished_at, execution_time_ms)
                 VALUES ('res-1', 'task-1', 'a', 'ai-1', 'success', 'out',
                     '2026-10-14T00:00:00.000Z', '2026-10-14T00:00:01.000Z', 1000);",
            )
            .expect("seed");

        let err = db
            .connection()
            .execute(
                "INSERT INTO artifacts (result_id, base, path, kind, created_at)
                 VALUES ('res-1', 'workspace', 'x.md', 'file', '2026-10-14T00:00:01.000Z')",
                [],
            )
            .expect_err("must be refused");
        assert!(err.to_string().contains("CHECK"), "{err}");
    }

    #[test]
    fn plan_revisions_are_unique_per_task() {
        // ATH-PLAN-004: a revision is never overwritten, so a duplicate must be impossible.
        let db = db();
        db.connection()
            .execute_batch(
                "INSERT INTO tasks (id, title, instruction, workspace, state, priority,
                     risk_level, max_attempts, max_iterations, created_by, created_at, updated_at)
                 VALUES ('task-1', 't', 'i', '/w', 'RUNNING', 'normal', 'low', 3, 50, 'u',
                     '2026-10-14T00:00:00.000Z', '2026-10-14T00:00:00.000Z');
                 INSERT INTO plans (id, task_id, revision, goal, created_at)
                 VALUES ('plan-1', 'task-1', 1, 'g', '2026-10-14T00:00:00.000Z');",
            )
            .expect("seed");

        let err = db.connection().execute(
            "INSERT INTO plans (id, task_id, revision, goal, created_at)
             VALUES ('plan-2', 'task-1', 1, 'g', '2026-10-14T00:00:00.000Z')",
            [],
        );
        assert!(err.is_err(), "a duplicate revision must be refused");
    }

    #[test]
    fn event_sequence_is_unique() {
        // ATH-EVENT-003: monotonic per Core, so two events cannot share a position.
        let db = db();
        db.connection()
            .execute(
                "INSERT INTO events (event_id, schema_version, event_type, occurred_at, sequence,
                     correlation_id, actor)
                 VALUES ('evt-1', 1, 'TaskCreated', '2026-10-14T00:00:00.000Z', 1, 'cor-1', 'human')",
                [],
            )
            .expect("append");
        let err = db.connection().execute(
            "INSERT INTO events (event_id, schema_version, event_type, occurred_at, sequence,
                 correlation_id, actor)
             VALUES ('evt-2', 1, 'TaskQueued', '2026-10-14T00:00:01.000Z', 1, 'cor-1', 'system')",
            [],
        );
        assert!(err.is_err(), "a duplicate sequence must be refused");
    }

    #[test]
    fn a_workspace_can_be_leased_by_only_one_task() {
        // ATH-DATA-009: the primary key is the exclusivity that the API's 409 rests on.
        let db = db();
        db.connection()
            .execute_batch(
                "INSERT INTO tasks (id, title, instruction, workspace, state, priority,
                     risk_level, max_attempts, max_iterations, created_by, created_at, updated_at)
                 VALUES ('task-1', 't', 'i', '/w', 'RUNNING', 'normal', 'low', 3, 50, 'u',
                     '2026-10-14T00:00:00.000Z', '2026-10-14T00:00:00.000Z');
                 INSERT INTO tasks (id, title, instruction, workspace, state, priority,
                     risk_level, max_attempts, max_iterations, created_by, created_at, updated_at)
                 VALUES ('task-2', 't', 'i', '/w', 'CREATED', 'normal', 'low', 3, 50, 'u',
                     '2026-10-14T00:00:00.000Z', '2026-10-14T00:00:00.000Z');
                 INSERT INTO workspace_leases (workspace, task_id, acquired_at)
                 VALUES ('/w', 'task-1', '2026-10-14T00:00:00.000Z');",
            )
            .expect("seed");

        let err = db.connection().execute(
            "INSERT INTO workspace_leases (workspace, task_id, acquired_at)
             VALUES ('/w', 'task-2', '2026-10-14T00:00:00.000Z')",
            [],
        );
        assert!(
            err.is_err(),
            "a second lease on one workspace must be refused"
        );
    }

    #[test]
    fn an_idempotency_key_cannot_be_inserted_twice() {
        // ATH-CORE-035: this is what prevents a double delegation after a restart.
        let db = db();
        db.connection()
            .execute(
                "INSERT INTO idempotency_keys (key, created_at)
                 VALUES ('step-1:0', '2026-10-14T00:00:00.000Z')",
                [],
            )
            .expect("first");
        let err = db.connection().execute(
            "INSERT INTO idempotency_keys (key, created_at)
             VALUES ('step-1:0', '2026-10-14T00:00:01.000Z')",
            [],
        );
        assert!(err.is_err(), "a repeated key must be refused");
    }

    #[test]
    fn skill_rows_are_keyed_by_id_and_version() {
        // ATH-STATE-026: state is tracked per (id, version).
        let db = db();
        let insert = |id: &str, version: &str| {
            db.connection().execute(
                "INSERT INTO skills (id, version, name, description, runtime_provider, source,
                     checksum, publisher, risk_level, state, registered_at, updated_at)
                 VALUES (?1, ?2, 'n', 'd', 'agent-cli', 's', 'c', 'p', 'medium', 'AVAILABLE',
                     '2026-10-14T00:00:00.000Z', '2026-10-14T00:00:00.000Z')",
                rusqlite::params![id, version],
            )
        };
        insert("fpga-timing-analysis", "1.2.0").expect("first version");
        insert("fpga-timing-analysis", "1.3.0").expect("second version coexists");
        assert!(
            insert("fpga-timing-analysis", "1.2.0").is_err(),
            "the same (id, version) must not be inserted twice"
        );
    }

    #[test]
    fn timestamps_are_rfc3339_utc() {
        // ATH-DATA-002.
        let now = now_rfc3339();
        assert!(now.ends_with('Z'), "{now}");
        assert_eq!(now.len(), 24, "{now}");
        assert_eq!(&now[4..5], "-");
        assert_eq!(&now[10..11], "T");

        // Known epochs, to show the civil-from-days conversion is right rather than merely
        // well-formed.
        assert_eq!(format_rfc3339(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(format_rfc3339(1_000_000_000, 0), "2001-09-09T01:46:40.000Z");
        assert_eq!(
            format_rfc3339(1_760_000_000, 123),
            "2025-10-09T08:53:20.123Z"
        );
        // A leap day, where an off-by-one in the algorithm would show.
        assert_eq!(format_rfc3339(1_709_164_800, 0), "2024-02-29T00:00:00.000Z");
    }
}
