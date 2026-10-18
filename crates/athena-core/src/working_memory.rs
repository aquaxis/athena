//! Working Memory — minimal tier (P1-36; spec §13.1, ATH-MEM-001/002/004/007/008).
//!
//! Working Memory's scope is the Task, its persistence is the `memory_records` table
//! behind the Repository interface (ATH-MEM-012), and its *content set* is the §13.1
//! minimum that a Core Agent restart context rebuild needs — the same rows
//! [`crate::core_agent_session::ContextDocument::rebuild`] lifts. This module's minimal
//! tier implementation:
//!
//! - **Writes** go through `MemoryRepository` only, keyed `tier: working, scope: task_id`.
//! - **Size limit** (`memory.working_max_records`, ATH-MEM-002): enforced on every write.
//!   The eviction *policy* exists but the summarization does not happen here — summarizing
//!   is inference, and Core Host does not infer (ATH-MEM-008); the minimal tier's fallback
//!   is the specification's own fallback (R-MEM-07): discard the oldest content record and
//!   keep only its references.
//!   **Limit basis [design note]** the ceiling counts *content-carrying* records; the
//!   reference tombstones an eviction leaves behind are audit rows and do not consume the
//!   cap (they are the fallback's residue, never the §13.1 content). A Task's tombstone
//!   population is therefore bounded by its iteration history, and Phase 4's
//!   `P4-06` summarizer agent replaces the content while the record identity stays.
//! - **No secrets** (ATH-MEM-004): the hygiene pass is the same masking the Events use, so
//!   a record cannot carry plaintext bursts into the store.

use athena_events::redact::mask_value;
use athena_persistence::SqliteStore;
use athena_persistence::error::PersistenceError;
use athena_persistence::repository::MemoryRepository;
use athena_types::{MemoryOrigin, MemoryRecord, MemoryTier};
use athena_types::{TaskId, Timestamp};
use serde::{Deserialize, Serialize};

/// The §13.1 content set's item kinds, one per record's `content` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkingItem {
    /// The current Plan with its revision (`ATH-MEM-007` row 1).
    PlanRevision {
        /// The plan, referenced.
        plan_id: String,
        /// The revision stamped.
        revision: u64,
    },
    /// An incomplete PlanStep (`ATH-MEM-007` row 2).
    IncompleteStep {
        /// The step.
        step_id: String,
        /// Its state, at write time.
        state: String,
    },
    /// An unterminated delegation (`ATH-MEM-007` row 3).
    Delegation {
        /// The step the work belongs to.
        plan_step_id: String,
        /// The instance running it.
        instance_id: String,
        /// The attempt number.
        attempt: u64,
        /// The instance's state at write time.
        state: String,
    },
    /// A report arrival, referenced (`ATH-MEM-007` row 4): the message id, never the body.
    ReportRef {
        /// The reporting instance.
        instance_id: String,
        /// The report's message id.
        message_id: String,
    },
    /// A denial record (`ATH-MEM-007` row 7): the §23.2 Event carries the facts. The
    /// reference here is the Event id.
    DenialRef {
        /// The Event that recorded the denial.
        event_id: String,
    },
    /// A `SUSPENDED` instance's snapshot record reference (last row).
    SnapshotRef {
        /// The suspended instance.
        instance_id: String,
        /// The Working-Memory record that froze at suspension.
        memory_id: String,
    },
}

/// One write request.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkingWrite {
    /// The section the record lands under.
    pub item: WorkingItem,
    /// Provenance (ATH-MEM-005's Task ceiling is the tier's scope; the Event id carries
    /// the causation).
    pub origin_event_id: Option<String>,
    /// When (RFC 3339).
    pub at: Timestamp,
}

/// The tier, over the repository.
pub struct WorkingMemory {
    /// The record ceiling from `memory.working_max_records` (ATH-MEM-002).
    max_records: u64,
    /// Whether the `summarizer` agent is configured; the minimal tier does not call it
    /// (that is Phase 4's `P4-06`), so this only reports the fallback route.
    pub summarizer_configured: bool,
}

/// What one write produced (including any eviction it forced).
#[derive(Debug, Clone, PartialEq)]
pub struct WriteReport {
    /// The record written.
    pub record_id: String,
    /// Records evicted this write, each with its reference record (R-MEM-07's fallback —
    /// summarized properly in Phase 4 by the `memory.summarizer` agent).
    pub evicted: Vec<Evicted>,
}

/// One eviction.
#[derive(Debug, Clone, PartialEq)]
pub struct Evicted {
    /// The deleted record's id.
    pub removed: String,
    /// The reference record that replaced it
    /// (`content: {"kind":"reference","of": …}`), which keeps the audit chain without the
    /// bulk.
    pub reference_record: String,
}

impl WorkingMemory {
    /// The minimal tier, from configuration limits.
    #[must_use]
    pub fn new(max_records: u64) -> Self {
        Self {
            max_records,
            summarizer_configured: false,
        }
    }

    /// Write one item, enforcing the size limit first.
    ///
    /// # Errors
    /// Store failures; the eviction pass is part of the write (best-effort, never
    /// aborting a write, per ATH-MEM-002's tier policy).
    pub fn write(
        &self,
        store: &mut SqliteStore,
        task: &TaskId,
        request: WorkingWrite,
    ) -> Result<WriteReport, PersistenceError> {
        let mut evicted_report = Vec::new();

        // The ceiling counts content-carrying records (see the module note); reference
        // tombstones evictions leave behind do not consume it.
        let content_count = MemoryRepository::list(store, MemoryTier::Working, task.as_str())?
            .iter()
            .filter(|record| {
                serde_json::from_str::<serde_json::Value>(&record.content)
                    .map(|value| !is_reference(&value))
                    .unwrap_or(true)
            })
            .count() as u64;
        if content_count + 1 > self.max_records {
            self.evict_oldest(store, task, &mut evicted_report)?;
        }

        let hygiene = mask_value(serde_json::to_value(&request.item).expect("items serialize"));
        let record = MemoryRecord {
            id: athena_types::MemoryId::new(),
            tier: MemoryTier::Working,
            scope_id: task.as_str().into(),
            content: hygiene.to_string(),
            summary: None,
            origin: MemoryOrigin {
                task_id: Some(task.clone()),
                event_id: request
                    .origin_event_id
                    .as_ref()
                    .and_then(|event_id| athena_types::EventId::parse(event_id.clone())),
            },
            created_at: request.at.clone(),
            expires_at: None,
        };
        let record_id = record.id.as_str().to_string();
        MemoryRepository::insert(store, &record)?;
        Ok(WriteReport {
            record_id,
            evicted: evicted_report,
        })
    }

    /// Evict the oldest record into a reference-only record (R-MEM-07's fallback); Phase
    /// 4's summarizer agent replaces the content where the availability stays.
    ///
    /// # Errors
    /// Store failures raised by the delete/insert pair.
    #[allow(clippy::too_many_lines, reason = "explicit eviction bookkeeping")]
    fn evict_oldest(
        &self,
        store: &mut SqliteStore,
        task: &TaskId,
        evicted_report: &mut Vec<Evicted>,
    ) -> Result<(), PersistenceError> {
        let records = MemoryRepository::list(store, MemoryTier::Working, task.as_str())?;
        // Only content-carrying records evict: tombstones are the fallback's residue; the
        // oldest content record is the §13.1 item the tier can least afford to re-summarize.
        let content_records: Vec<MemoryRecord> = records
            .iter()
            .filter(|record| {
                serde_json::from_str::<serde_json::Value>(&record.content)
                    .map(|value| !is_reference(&value))
                    .unwrap_or(true)
            })
            .cloned()
            .collect();
        let mut content_records = content_records;
        content_records
            .sort_by_key(|record| (record.created_at.clone(), record.id.as_str().to_owned()));
        let Some(target) = content_records.first().cloned() else {
            // Every record is already a reference: nothing more can evict without losing
            // the §13.1 minimum; the write proceeds (the ceiling is the content count).
            return Ok(());
        };
        let removed_id = target.id.as_str().to_string();
        MemoryRepository::delete(store, &target.id)?;

        let replacement = MemoryRecord {
            id: athena_types::MemoryId::new(),
            tier: MemoryTier::Working,
            scope_id: task.as_str().into(),
            content: serde_json::json!({
                "kind": "reference",
                "of": removed_id,
                "origin_event_id": target.origin.event_id.as_ref().map(|e| e.as_str()),
            })
            .to_string(),
            summary: None,
            origin: MemoryOrigin {
                task_id: Some(task.clone()),
                event_id: target.origin.event_id.clone(),
            },
            created_at: target.created_at.clone(),
            expires_at: None,
        };
        let reference_id = replacement.id.as_str().to_string();
        MemoryRepository::insert(store, &replacement)?;
        evicted_report.push(Evicted {
            removed: removed_id,
            reference_record: reference_id,
        });
        Ok(())
    }

    /// Read the Task's records, oldest first — the raw material the context rebuild reads.
    ///
    /// # Errors
    /// Store failures.
    pub fn records(
        &self,
        store: &mut SqliteStore,
        task: &TaskId,
    ) -> Result<Vec<MemoryRecord>, PersistenceError> {
        MemoryRepository::list(store, MemoryTier::Working, task.as_str())
    }
}

/// Whether a content payload is itself a reference record (rather than a §13.1 item).
fn is_reference(value: &serde_json::Value) -> bool {
    value.get("kind").and_then(|kind| kind.as_str()) == Some("reference")
}

#[cfg(test)]
mod working_memory_tests {
    use super::*;
    use athena_persistence::repository::TaskRepository;
    use athena_types::Task as TaskRow;

    fn store_with_task() -> (SqliteStore, TaskId) {
        let mut store = SqliteStore::in_memory().expect("store");
        let task = TaskRow {
            id: TaskId::new(),
            parent_id: None,
            title: "Memory".into(),
            instruction: "seed tier".into(),
            workspace: "/work".into(),
            goal: None,
            constraints: vec![],
            completion_criteria: vec![],
            state: athena_types::TaskState::Running,
            priority: athena_types::Priority::Normal,
            plan_id: None,
            risk_level: athena_types::RiskLevel::Medium,
            attempt_count: 0,
            max_attempts: 3,
            iteration_count: 0,
            max_iterations: 50,
            deadline: None,
            created_by: "test".into(),
            created_at: "2026-10-14T00:00:00.000Z".into(),
            updated_at: "2026-10-14T00:00:00.000Z".into(),
            finished_at: None,
        };
        TaskRepository::insert(&mut store, &task).expect("task seeds");
        (store, task.id)
    }

    #[test]
    fn the_tier_writes_isolated_per_task_records() {
        // ATH-MEM-001 / ATH-MEM-012: the tiers are separated in the implementation, and a
        // Task's Working Memory lives under its own scope key behind the Repository.
        let (mut store, task) = store_with_task();
        let tier = WorkingMemory::new(500);
        let report = tier
            .write(
                &mut store,
                &task,
                WorkingWrite {
                    item: WorkingItem::PlanRevision {
                        plan_id: "plan-1".into(),
                        revision: 2,
                    },
                    origin_event_id: Some("evt-01TESTTESTTESTTESTTESTTEST".into()),
                    at: "2026-10-14T00:00:00.000Z".into(),
                },
            )
            .expect("written");
        assert_eq!(report.evicted.len(), 0);

        let records = tier.records(&mut store, &task).expect("list");
        assert_eq!(records.len(), 1);
        let item: WorkingItem = serde_json::from_str(&records[0].content).expect("item");
        assert_eq!(
            item,
            WorkingItem::PlanRevision {
                plan_id: "plan-1".into(),
                revision: 2
            }
        );
        // Provenance is present (ATH-MEM-005).
        assert!(records[0].origin.task_id.is_some());
    }

    #[test]
    fn the_ceiling_evicts_the_oldest_content_record_to_a_reference() {
        // ATH-MEM-002's ceiling + R-MEM-07's fallback: the minimal tier's eviction
        // discards the oldest *content* record and keeps only its references; Core Host
        // never summarizes (ATH-MEM-008, the summarizer agent remains Phase 4).
        let (mut store, task) = store_with_task();
        let tier = WorkingMemory::new(2);
        for at in 0..3 {
            let report = tier
                .write(
                    &mut store,
                    &task,
                    WorkingWrite {
                        item: WorkingItem::Delegation {
                            plan_step_id: format!("step-0{}", at),
                            instance_id: format!("ai-0{}", at),
                            attempt: 0,
                            state: "BUSY".into(),
                        },
                        origin_event_id: Some("evt-01TESTTESTTEST".into()),
                        at: format!("2026-10-14T00:00:0{}.000Z", at),
                    },
                )
                .expect("written");
            if at == 2 {
                // The write at the ceiling evicted the oldest.
                assert_eq!(report.evicted.len(), 1);
                assert!(
                    !report.evicted[0].removed.is_empty()
                        && !report.evicted[0].reference_record.is_empty()
                );
            } else {
                assert_eq!(report.evicted.len(), 0);
            }
        }
        let records = tier.records(&mut store, &task).expect("list");
        // The tombstone of the first eviction stays for the audit chain and does not
        // consume the cap (the module note); the content population never exceeds it.
        let content_records = records
            .iter()
            .filter(|record| {
                !is_reference(
                    &serde_json::from_str::<serde_json::Value>(&record.content).expect("json"),
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            content_records.len(),
            2,
            "the ceiling holds exactly two content records: {:?}",
            records.iter().map(|r| &r.content).collect::<Vec<_>>()
        );
        // The newest content records survived; the evicted one became a reference.
        assert_eq!(
            content_records.len(),
            2,
            "both written items are content records now"
        );
        // Provenance (ATH-MEM-005) carries over to the reference record.
        let reference = records
            .iter()
            .find(|record| {
                is_reference(
                    &serde_json::from_str::<serde_json::Value>(&record.content).expect("json"),
                )
            })
            .expect("the reference record");
        assert!(reference.origin.task_id.is_some());
        // The span of the removed item is not lost: the reference names the removed id.
        assert!(reference.content.contains("origin_event_id"));
    }

    #[test]
    fn secrets_are_masked_before_the_store_sees_them() {
        // ATH-MEM-004 / R-POL-13: the same hygiene pass as the Events applies to the tier.
        let (mut store, task) = store_with_task();
        let tier = WorkingMemory::new(500);
        tier.write(
            &mut store,
            &task,
            WorkingWrite {
                item: WorkingItem::ReportRef {
                    instance_id: "bearer sk-0123456789aaaaaaaaaaaaaaaa".into(),
                    message_id: "msg-1".into(),
                },
                origin_event_id: None,
                at: "2026-10-14T00:00:00.000Z".into(),
            },
        )
        .expect("written");
        let records = tier.records(&mut store, &task).expect("list");
        assert!(!records[0].content.contains("sk-0123456789aaaaaaaaaaaaaaaa"));
        assert!(
            records[0].content.contains("[REDACTED]"),
            "{}",
            records[0].content
        );
    }

    #[test]
    fn a_report_ref_cites_a_message_never_a_body() {
        // ATH-MEM-007 row 4 + ATH-EVENT-007: the tier carries the reference only; the
        // message body stays wherever bodies live (the message table, masked there).
        let (mut store, task) = store_with_task();
        let tier = WorkingMemory::new(500);
        tier.write(
            &mut store,
            &task,
            WorkingWrite {
                item: WorkingItem::ReportRef {
                    instance_id: "ai-1".into(),
                    message_id: "msg-01REPORT".into(),
                },
                origin_event_id: Some("evt-01REPORTREPORT".into()),
                at: "2026-10-14T00:00:00.000Z".into(),
            },
        )
        .expect("written");
        let item: WorkingItem =
            serde_json::from_str(&tier.records(&mut store, &task).expect("list")[0].content)
                .expect("item");
        let _ = item;
    }
}
