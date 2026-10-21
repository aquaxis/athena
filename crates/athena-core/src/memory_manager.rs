//! Memory Manager — `MemoryRecord` across the three tiers (spec §13).
//!
//! Owns limit / eviction policy application and the `SUSPENDED` snapshot, never the
//! summarization itself — that is delegated to the agent named by `memory.summarizer`
//! (ATH-MEM-008); Core Host does not infer.
//!
//! Owned entities (`OWNED`): MemoryRecord. Emitted Events: `MemoryRecord*`.

use crate::ownership::{Entity, ModuleRole};

/// The entities this module may mutate.
pub const OWNED: &[(Entity, ModuleRole)] = &[(Entity::MemoryRecord, ModuleRole::MemoryManager)];

/// P4-07: the memory policy built from the **resolved configuration** (`ATH-MEM-009`):
/// the tokenizer named in `memory.tokenizer` is the only counting rule the short-term
/// budget ever sees — an empty name is refused here as it is at startup
/// (`athena-config`'s fail-fast validation), so no provider default is ever implicit.
///
/// # Errors
/// [`athena_memory::PolicyFault`] with the offending value.
pub fn policy_from_config(
    memory: &athena_config::schema::MemorySection,
) -> Result<athena_memory::MemoryPolicy, athena_memory::PolicyFault> {
    athena_memory::MemoryPolicy::from_values(
        memory.short_term_max_tokens,
        &memory.tokenizer,
        memory.working_max_records,
        memory.long_term.enabled,
        memory.long_term.max_records,
        &memory.summarizer,
    )
}

#[cfg(test)]
mod policy_tests {
    use super::policy_from_config;

    #[test]
    fn the_policy_carries_the_resolved_tokenizer_and_summarizer() {
        let mut section = athena_config::schema::MemorySection {
            tokenizer: "provider-default-claude".into(),
            summarizer: "memory-summarizer-agent".into(),
            short_term_max_tokens: 48_000,
            ..athena_config::schema::MemorySection::default()
        };
        let policy = policy_from_config(&section).expect("the resolved section is a usable policy");
        assert_eq!(policy.tokenizer, "provider-default-claude");
        assert_eq!(policy.short_term_max_tokens, 48_000);
        assert_eq!(policy.summarizer, "memory-summarizer-agent");

        // An empty tokenizer cannot reach the policy (the config validation refuses it
        // at startup; this is the use-point mirror).
        section.tokenizer = "  ".into();
        assert!(policy_from_config(&section).is_err(), "ATH-MEM-009");
    }
}

// --- P4-06: eviction summarization, delegated ---------------------------------------------

pub mod eviction {
    //! The disposition of tier overflow (`ATH-MEM-002` × `ATH-MEM-008`): summarization is
    //! **inference**, so it is delegated to the agent named by `memory.summarizer`; Core
    //! Host implements no deterministic summarization. When the summarizer is
    //! unavailable, the specification's own fallback runs (R-MEM-07): discard the old
    //! records and keep only their references — the same tombstone shape P1-36's
    //! Working Memory writes.

    use crate::ownership::{Entity, ModuleRole};
    use athena_persistence::AthenaStore;
    use athena_persistence::repository::MemoryRepository;
    use athena_types::{MemoryId, MemoryTier};

    /// Silent ownership re-statement (drift-checked like every Core Host module).
    #[must_use]
    pub const fn owned() -> &'static [(Entity, ModuleRole)] {
        &[(Entity::MemoryRecord, ModuleRole::MemoryManager)]
    }

    /// One record standing for eviction, oldest-first by the caller's ordering.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Target {
        /// The record evicted.
        pub id: MemoryId,
        /// Its creation stamp (the audit order is the caller's sort).
        pub created_at: String,
        /// The tier the record lives in.
        pub tier: MemoryTier,
        /// The scope the record is bound to (task for Working, project slug for Long).
        pub scope: String,
    }

    /// What the overflow disposition resolves to.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Resolution {
        /// Delegate to the `memory.summarizer` agent (`ATH-MEM-008`): the draft carries
        /// the targets and the direction that reaches the agent — the *summary itself*
        /// is the agent's output, never Core Host's prose.
        Delegate(Draft),
        /// The summarizer is unavailable: the R-MEM-07 fallback — discard the old
        /// records and keep only their references. The reason is preserved for the
        /// escalation record, never silently swallowed.
        KeepReferencesOnly {
            /// Why the summarizer could not be reached.
            reason: String,
        },
    }

    /// The delegation draft the Core Host supervision wiring submits as its own
    /// `CoreRequest::Delegate` for the summarizer agent. Deterministic orchestration
    /// (ids, targets, direction) — the *content* it will produce is inference and
    /// belongs to the agent.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Draft {
        /// The summarizer definition's id (`memory.summarizer`).
        pub summarizer: String,
        /// The task the work is attributed to (`None` for a project-scoped sweep).
        pub task_scope: String,
        /// The target record ids, oldest first.
        pub targets: Vec<String>,
        /// What Core Host asks for — deterministic orchestration only: read the place,
        /// produce a summary of the records, report `complete` with the summary text.
        pub instruction: String,
    }

    /// Kind of Tier for `tier`/`scope` record: the disposition for one tier's overflow.
    ///
    /// `summarizer_available(None)` resolves to the fallback; the *caller* decides
    /// availability (a registry lookup the wiring does), keeping this pure.
    #[must_use]
    pub fn resolve(summarizer: Option<&str>, task_scope: &str, targets: &[Target]) -> Resolution {
        match summarizer {
            Some(definition) if !targets.is_empty() => Resolution::Delegate(Draft {
                summarizer: definition.to_string(),
                task_scope: task_scope.to_string(),
                targets: targets.iter().map(|t| t.id.as_str().to_string()).collect(),
                instruction: format!(
                    "Summarize the memory records named by ids below into a concise \
                     narrative for tier {} scope {}: report the summary text, then the \
                     references to save (HALT if the ids do not resolve).",
                    targets[0].tier, targets[0].scope
                ),
            }),
            _ if targets.is_empty() => {
                // Nothing to evict; no delegation, no fallback.
                Resolution::Delegate(Draft {
                    summarizer: summarizer.unwrap_or("").to_string(),
                    task_scope: task_scope.to_string(),
                    targets: vec![],
                    instruction: String::new(),
                })
            }
            _ => Resolution::KeepReferencesOnly {
                reason: "the memory.summarizer agent is unavailable (R-MEM-07 fallback)".into(),
            },
        }
    }

    /// The tombstone the fallback writes (a *reference*, not a narrative):
    /// the same shape the Working tier's eviction already produces (P1-36). The text is
    /// a constant; writing it is record-keeping, never inference.
    const FALLBACK_TOMBSTONE: &str = "content evicted under ATH-MEM-002's fallback; \
         only references kept (R-MEM-07; the summarizer was unavailable)";

    /// Apply the reference-only fallback: each target's content is replaced by the
    /// tombstone reference through `MemoryRepository::summarize` (`ATH-MEM-002`).
    ///
    /// # Errors
    /// Store failures, surfaced whole.
    pub fn apply_fallback(
        store: &mut AthenaStore,
        targets: &[Target],
    ) -> Result<usize, athena_persistence::error::PersistenceError> {
        let mut applied = 0usize;
        for target in targets {
            MemoryRepository::summarize(store, &target.id, FALLBACK_TOMBSTONE)?;
            applied += 1;
        }
        Ok(applied)
    }

    /// The fallback text (exposed so tests can assert the *absence* of narrative).
    #[must_use]
    pub fn fallback_text() -> &'static str {
        FALLBACK_TOMBSTONE
    }
}

#[cfg(test)]
mod eviction_tests {
    use super::eviction::{self, Resolution, Target};
    use athena_types::{MemoryId, MemoryTier};

    fn targets() -> Vec<Target> {
        vec![
            Target {
                id: MemoryId::new(),
                created_at: "2026-10-17T00:00:00Z".into(),
                tier: MemoryTier::Working,
                scope: "task-x".into(),
            },
            Target {
                id: MemoryId::new(),
                created_at: "2026-10-17T01:00:00Z".into(),
                tier: MemoryTier::Working,
                scope: "task-x".into(),
            },
        ]
    }

    #[test]
    fn an_available_summarizer_delegates_the_inference_never_bakes_it() {
        let resolution = eviction::resolve(Some("memory-summarizer-agent"), "task-x", &targets());
        match resolution {
            Resolution::Delegate(draft) => {
                assert_eq!(draft.summarizer, "memory-summarizer-agent");
                assert_eq!(draft.targets.len(), 2, "oldest-first targets");
                // The instruction is orchestration: names the ids and the asked output,
                // and *writes no summary itself* (no prose content Core bakes in).
                assert!(
                    !draft.targets.is_empty()
                        && draft.instruction.contains("Summarize the memory records"),
                    "{:?}",
                    draft.instruction
                );
                assert!(
                    !draft.instruction.contains("was a successful build"),
                    "no deterministic summary content exists in Core Host"
                );
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn an_unavailable_summarizer_falls_back_to_references_only() {
        let resolution = eviction::resolve(None, "task-x", &targets());
        match resolution {
            Resolution::KeepReferencesOnly { reason } => {
                assert!(reason.contains("unavailable"), "{reason}");
                // And the fallback applies reference tombstones, not prose.
                let text = eviction::fallback_text();
                assert!(text.contains("references kept"), "{text}");
                assert!(text.contains("the summarizer was unavailable"));
            }
            other => panic!("unexpected {other:?}"),
        }
        // The fallback applies cleanly through the store (its unit cover lives in the
        // store's summarize() tests; this asserts the disposition choice only).
    }

    #[test]
    fn empty_targets_deliberate_nothing() {
        let resolution = eviction::resolve(None, "task-x", &[]);
        assert!(
            matches!(resolution, Resolution::Delegate(_)),
            "{resolution:?}"
        );
    }
}

// --- P4-08: the long-term write gate ------------------------------------------------------

pub mod long_term {
    //! Long Term (`ATH-MEM-003`/`005`/`006`/`012`): explicit writes only, provenance
    //! carried, deletion recorded. The storage is the **same persistent store** behind
    //! the `MemoryRepository` (`ATH-MEM-012`) — enforced by construction here: every
    //! staged write goes through the repository's `insert` with
    //! `tier: Long`, `scope_id: ProjectId`, never a separate file or vector store.

    use crate::ownership::{Entity, ModuleRole};
    use athena_persistence::AthenaStore;
    use athena_persistence::repository::MemoryRepository;
    use athena_types::{
        EventId, MemoryId, MemoryOrigin, MemoryRecord, MemoryTier, ProjectId, TaskId,
    };

    /// Silent ownership re-statement (drift-checked like every Core Host module).
    #[must_use]
    pub const fn owned() -> &'static [(Entity, ModuleRole)] {
        &[(Entity::MemoryRecord, ModuleRole::MemoryManager)]
    }

    /// Who may request a Long write (`ATH-MEM-003`): **only** (a) the Core Agent's
    /// explicit judgement, expressed through the Core Control Protocol's
    /// `WriteMemory { tier: Long }` (the only wire kind that can even express it —
    /// see P4-04's audit), or (b) a human instruction through the API (`admin`). An
    /// Agent Process's own runtime behaviour is never a requester: no tool, message,
    /// or skill output reaches this gate except through those two shapes.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Requester {
        /// The Core Agent judged the write explicitly (`ATH-CORE-022`'s decision).
        CoreAgentJudgement,
        /// A human asked for it (the API's `admin` rote path carries its approver).
        HumanInstruction {
            /// The documented requester (the API token's role + identity trail).
            identity: String,
        },
    }

    /// Why a Long write is refused.
    #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
    pub enum WriteFault {
        /// An implicit write: no explicit judgement or human instruction backs it.
        #[error(
            "a long-term write requires an explicit Core Agent judgement or a human instruction (ATH-MEM-003); got {0}"
        )]
        Implicit(&'static str),
        /// Provenance missing — every Long record carries its Task and Event anchors.
        #[error(
            "a long-term record carries provenance (task and event anchors, ATH-MEM-005); got neither"
        )]
        ProvenanceMissing,
        /// The long-term tier is disabled by configuration.
        #[error("memory.long_term.enabled is false")]
        TierDisabled,
    }

    /// Review one Long write (`ATH-MEM-003` ⇒ `ATH-MEM-005`). Returns the staged
    /// record — not yet persisted; the caller (`insert_long`) writes it and the caller's
    /// `MemoryRecordCreated` Event materialization follows (D-1).
    ///
    /// # Errors
    /// [`WriteFault`] with the violated rule.
    pub fn review(
        requester: &Requester,
        content: &str,
        scope: &athena_types::ProjectId,
        long_term_enabled: bool,
        provenance: MemoryOrigin,
    ) -> Result<MemoryRecord, WriteFault> {
        // The sting: an *absent* requester is an implicit write — the caller may not
        // smuggle a tool call, a message, or a skill output in as memory.
        if content.trim().is_empty() {
            return Err(WriteFault::Implicit("empty content is not a judgement"));
        }
        if !long_term_enabled {
            return Err(WriteFault::TierDisabled);
        }
        if provenance.task_id.is_none() && provenance.event_id.is_none() {
            return Err(WriteFault::ProvenanceMissing);
        }
        match requester {
            Requester::CoreAgentJudgement | Requester::HumanInstruction { .. } => {}
        }
        Ok(MemoryRecord {
            id: MemoryId::new(),
            tier: MemoryTier::Long,
            scope_id: scope.as_str().to_string(),
            content: content.to_string(),
            summary: None,
            origin: provenance,
            created_at: athena_persistence::db::now_rfc3339(),
            expires_at: None,
        })
    }

    /// The staged write: the repository's `insert` — the same connection the store owns
    /// (`ATH-MEM-012`: one database; no separate file, no external vector store).
    ///
    /// # Errors
    /// Store failures, surfaced whole.
    pub fn insert_long(
        store: &mut AthenaStore,
        record: &MemoryRecord,
    ) -> Result<(), athena_persistence::error::PersistenceError> {
        MemoryRepository::insert(store, record)
    }

    /// The deletion request (`ATH-MEM-006`): the record must exist; the *Event* is the
    /// audit record — the builder lives with the Event Manager's emission — so this
    /// returns the deleted fact (id + the tier/scope it had) for the caller's Event.
    ///
    /// # Errors
    /// Store failures, or `NotFound` when no record is there to delete.
    pub fn delete(
        store: &mut AthenaStore,
        id: &MemoryId,
    ) -> Result<MemoryRecord, athena_persistence::error::PersistenceError> {
        let record = MemoryRepository::get(store, id)?.ok_or_else(|| {
            athena_persistence::error::PersistenceError::NotFound {
                entity: "memory_record",
                id: id.as_str().to_string(),
            }
        })?;
        MemoryRepository::delete(store, id)?;
        Ok(record)
    }

    // --- P8-03: maturation — retention pressure, provenance queries, scoping ------

    /// Why a Long write under retention pressure refuses.
    #[derive(Debug, thiserror::Error)]
    pub enum RetentionFault {
        /// A store failure, surfaced whole (never renamed into a policy fault).
        #[error("{0}")]
        Store(#[from] athena_persistence::error::PersistenceError),
        /// The tier's ceiling is met: further writes are refused so that the caller
        /// (Core Agent / human) is the one who chooses what to remove — long-term
        /// history is never silently digested (`ATH-MEM-002`'s long-tier policy,
        /// the explicit-deliberation design decision).
        #[error(
            "the long-term tier is at its ceiling ({used}/{ceiling} for {scope}): removal is an explicit judgement, never a silent digest (ATH-MEM-002)"
        )]
        CeilingReached {
            /// Records already held in the scope.
            used: u64,
            /// The configured ceiling.
            ceiling: u64,
            /// The project scope at pressure.
            scope: String,
        },
    }

    /// The retention-policy guard around a staged Long write (P8-03): the per-project
    /// ceiling (`memory.long_term.max_records`) is counted by scope (`ProjectId`), and
    /// pressure at the ceiling refuses the write with a cycle-ready escalation fact
    /// rather than evicting anything itself.
    ///
    /// # Errors
    /// [`RetentionFault::CeilingReached`] at pressure; store failures surfacing whole.
    pub fn write_with_retention(
        store: &mut AthenaStore,
        record: &MemoryRecord,
        max_records: u64,
    ) -> Result<(), RetentionFault> {
        let used = MemoryRepository::count(store, MemoryTier::Long, &record.scope_id)
            .map_err(RetentionFault::Store)?;
        if used >= max_records {
            return Err(RetentionFault::CeilingReached {
                used,
                ceiling: max_records,
                scope: record.scope_id.clone(),
            });
        }
        super::long_term::insert_long(store, record).map_err(RetentionFault::Store)
    }

    /// The provenance query face (P8-03, `ATH-MEM-005`): find Long records anchored to
    /// a Task and/or Event — "how did we know this?" is the mature tier's purpose.
    ///
    /// # Errors
    /// Store failures, surfaced whole.
    pub fn provenance_query(
        store: &AthenaStore,
        task: Option<&TaskId>,
        event: Option<&EventId>,
    ) -> Result<Vec<MemoryRecord>, athena_persistence::error::PersistenceError> {
        MemoryRepository::list_by_provenance(store, task, event)
    }

    /// The `ProjectId` derivation from a normalized absolute workspace path
    /// (`R-DAT-02`, deterministic SHA-256) — the scoping gate a Long write must name.
    #[must_use]
    pub fn project_scope_of(normalized_absolute_workspace: &str) -> ProjectId {
        ProjectId::derive(normalized_absolute_workspace)
    }

    /// The `MemoryRecordCreated` payload facts (the Event keeps references only,
    /// `ATH-EVENT-007` — the content never rides an Event).
    #[must_use]
    pub fn created_event_facts(record: &MemoryRecord) -> (String, &'static str, String, String) {
        (
            record.id.as_str().to_string(),
            "memory_record_created",
            record.scope_id.clone(),
            record.created_at.clone(),
        )
    }
}

#[cfg(test)]
mod long_term_tests {
    use super::long_term::{self, Requester, WriteFault};
    use athena_persistence::repository::TaskRepository;
    use athena_types::{EventId, MemoryOrigin, MemoryTier, ProjectId, Task, TaskId, TaskState};

    fn task_row() -> Task {
        Task {
            id: TaskId::new(),
            parent_id: None,
            title: "Long-term gate".into(),
            instruction: "seed".into(),
            workspace: "/work/lt".into(),
            goal: None,
            constraints: vec![],
            completion_criteria: vec![],
            state: TaskState::Running,
            priority: athena_types::Priority::Normal,
            plan_id: None,
            risk_level: athena_types::RiskLevel::Low,
            attempt_count: 0,
            max_attempts: 3,
            iteration_count: 0,
            max_iterations: 50,
            deadline: None,
            created_by: "test".into(),
            created_at: "2026-10-17T00:00:00.000Z".into(),
            updated_at: "2026-10-17T00:00:00.000Z".into(),
            finished_at: None,
        }
    }

    #[test]
    fn only_explicit_requesters_stage_a_long_write() {
        let scope = ProjectId::derive("/work/lt-project");
        let provenance = MemoryOrigin {
            task_id: Some(TaskId::new()),
            event_id: Some(
                EventId::parse("evt-0001Y2A4WZCJ8MD7P9E3F0VQ5XA2BNH6SU").expect("evt id"),
            ),
        };

        // The two legal requesters stage a provenance-carrying record.
        for requester in [
            Requester::CoreAgentJudgement,
            Requester::HumanInstruction {
                identity: "admin token rt-1".into(),
            },
        ] {
            let staged = long_term::review(
                &requester,
                "the project uses fnv hashing for ids",
                &scope,
                true,
                provenance.clone(),
            )
            .expect("staged");
            assert_eq!(staged.tier, MemoryTier::Long);
            assert_eq!(staged.scope_id, scope.as_str());
            assert!(staged.origin.task_id.is_some() && staged.origin.event_id.is_some());
            assert!(
                long_term::created_event_facts(&staged).1 == "memory_record_created",
                "the Event facts stay reference-shaped"
            );
        }

        // An empty content is not a judgement; a disabled tier refuses.
        assert!(matches!(
            long_term::review(
                &Requester::CoreAgentJudgement,
                "   ",
                &scope,
                true,
                provenance.clone()
            ),
            Err(WriteFault::Implicit(_))
        ));
        assert!(matches!(
            long_term::review(
                &Requester::HumanInstruction {
                    identity: "x".into()
                },
                "content",
                &scope,
                false,
                provenance.clone()
            ),
            Err(WriteFault::TierDisabled)
        ));

        // Provenance is mandatory (`ATH-MEM-005`).
        assert!(matches!(
            long_term::review(
                &Requester::CoreAgentJudgement,
                "content",
                &scope,
                true,
                MemoryOrigin {
                    task_id: None,
                    event_id: None
                },
            ),
            Err(WriteFault::ProvenanceMissing)
        ));
    }

    #[test]
    fn the_staged_record_lives_in_the_same_store_and_deletion_is_the_audit() {
        let mut store = athena_persistence::AthenaStore::in_memory().expect("store");
        let task = task_row();
        TaskRepository::insert(&mut store, &task).expect("task seeds");
        let scope = ProjectId::derive("/work/lt-project");
        let provenance = MemoryOrigin {
            task_id: Some(task.id.clone()),
            event_id: Some(EventId::parse("evt-0001Y2A4WZCJ8MD7P9E3F0VQ5XA2BNH6SU").expect("evt")),
        };
        let staged = long_term::review(
            &Requester::CoreAgentJudgement,
            "the project decided to keep fnv hashing",
            &scope,
            true,
            provenance,
        )
        .expect("staged");

        // Same-database persistence through the repository (`ATH-MEM-012`).
        long_term::insert_long(&mut store, &staged).expect("persisted");
        let listed = athena_persistence::repository::MemoryRepository::list(
            &store,
            MemoryTier::Long,
            scope.as_str(),
        )
        .expect("listed");
        assert_eq!(listed.len(), 1, "one Long row behind the repository");
        assert_eq!(listed[0].id, staged.id);

        // Deletion removes the row; the caller's `MemoryRecordDeleted` Event is the audit.
        let deleted = long_term::delete(&mut store, &staged.id).expect("deleted");
        assert_eq!(deleted.id, staged.id);
        assert_eq!(
            deleted.scope_id,
            scope.as_str(),
            "the audit fact carries where the record lived"
        );
        let after = athena_persistence::repository::MemoryRepository::count(
            &store,
            MemoryTier::Long,
            scope.as_str(),
        )
        .expect("counted");
        assert_eq!(after, 0);
    }

    // --- P8-03: maturation ------------------------------------------------------------

    use super::super::memory_manager::long_term::{RetentionFault, write_with_retention};

    #[test]
    fn retention_pressure_refuses_at_the_ceiling_and_never_digests() {
        let mut store = athena_persistence::AthenaStore::in_memory().expect("store");
        let task = TaskId::new();
        TaskRepository::insert(&mut store, &task_row_method(task.clone())).ok();
        // A prepared scope + provenance-carrying staged record from `review`.
        let scope = ProjectId::derive("/work/maturation");

        let staged = (0..2)
            .map(|index| {
                super::long_term::review(
                    &Requester::CoreAgentJudgement,
                    &format!("judged fact {index}"),
                    &scope,
                    true,
                    MemoryOrigin {
                        task_id: Some(task.clone()),
                        event_id: None,
                    },
                )
                .expect("staged")
            })
            .collect::<Vec<_>>();

        // One of two slots filled; the second write meets the ceiling.
        write_with_retention(&mut store, &staged[0], 1).expect("first write accepted");
        let refusal = write_with_retention(&mut store, &staged[1], 1)
            .expect_err("the ceiling refuses further writes");
        assert!(
            matches!(
                &refusal,
                RetentionFault::CeilingReached {
                    used: 1,
                    ceiling: 1,
                    ..
                }
            ),
            "{refusal}"
        );
        // The refusal is an escalation the caller surfaces — removal stays an explicit
        // judgement (the deterministic tier never digests history silently).
        let kept = athena_persistence::repository::MemoryRepository::count(
            &store,
            MemoryTier::Long,
            scope.as_str(),
        )
        .expect("counted");
        assert_eq!(kept, 1);
    }

    fn task_row_method(id: TaskId) -> Task {
        let mut row = task_row();
        row.id = id;
        row
    }

    #[test]
    fn the_provenance_query_answers_anchored_long_records() {
        let mut store = athena_persistence::AthenaStore::in_memory().expect("store");
        let task = TaskId::new();
        let event = athena_types::EventId::new();
        let scope = ProjectId::derive("/work/provenance");
        let staged = super::long_term::review(
            &Requester::CoreAgentJudgement,
            "the resolver refuses conflicting pins",
            &scope,
            true,
            MemoryOrigin {
                task_id: Some(task.clone()),
                event_id: Some(event.clone()),
            },
        )
        .expect("staged");
        long_term::insert_long(&mut store, &staged).expect("inserted");

        // By task, by event, and the negative pair.
        let by_task = long_term::provenance_query(&store, Some(&task), None).expect("by task");
        assert_eq!(by_task.len(), 1);
        let by_event = long_term::provenance_query(&store, None, Some(&event)).expect("by event");
        assert_eq!(by_event[0].id, staged.id);
        let unrelated =
            long_term::provenance_query(&store, Some(&TaskId::new()), None).expect("annotated");
        assert!(unrelated.is_empty());
    }

    #[test]
    fn the_project_scope_is_deterministic_from_the_workspace_path() {
        use super::super::memory_manager::long_term::project_scope_of;
        let first = project_scope_of("/work/athena");
        let again = project_scope_of("/work/athena");
        let different = project_scope_of("/work/other");
        assert_eq!(
            first, again,
            "ATH-DATA-011: the derivation is deterministic"
        );
        assert_ne!(first, different, "different projects scope apart");
    }
}
