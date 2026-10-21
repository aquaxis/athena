#[cfg(test)]
mod tests {
    use crate::AthenaStore;
    use crate::db::now_rfc3339;
    use crate::error::PersistenceError;
    use crate::repository::*;
    use athena_types::*;

    fn store() -> AthenaStore {
        AthenaStore::in_memory().expect("in-memory store")
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

        ResultRepository::insert(
            &mut s,
            &ExecutionResult {
                id: ResultId::parse("res-keep").expect("shape"),
                task_id: task.id.clone(),
                agent_id: AgentId::new("a"),
                instance_id: InstanceId::parse("ai-1").expect("shape"),
                status: athena_types::ResultStatus::Success,
                output: "out".into(),
                artifacts: vec![],
                logs_ref: None,
                skills_used: vec![],
                plan_step_id: None,
                model: None,
                started_at: "2020-01-01T00:00:00.000Z".into(),
                finished_at: "2020-01-01T00:00:01.000Z".into(),
                execution_time_ms: 1000,
                metadata: serde_json::json!({}),
            },
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

        let results = ResultRepository::list_for_task(&s, &task.id)
            .expect("count")
            .len();
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
        eprintln!("DIAG first={:?} second={:?}", first.id, second.id);
        eprintln!("DIAG holder-after-release = {:?}", s.holder("/work/p"));
        assert!(s.acquire("/work/p", &second.id).expect("after release"));
        eprintln!("DIAG holder-final = {:?}", s.holder("/work/p"));
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
        let latest = PlanRepository::latest_for_task(&s, &task.id)
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
        s.raw_row(
            crate::db::tables::TASKS,
            "task-weird",
            r#"{"id":"task-weird","title":"t","instruction":"i","workspace":"/w",
                 "state":"NOT_A_STATE","priority":"normal","risk_level":"low",
                 "max_attempts":3,"max_iterations":50,"created_by":"u",
                 "created_at":"2026-10-14T00:00:00.000Z","updated_at":"2026-10-14T00:00:00.000Z"}"#,
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
        s.raw_row(
            crate::db::tables::TOOL_EXECUTIONS,
            "tx-0001Y2A4WZCJ8MD7P9E3F0VQ5XA2BNH6SU",
            r#"{"id":"tx-0001Y2A4WZCJ8MD7P9E3F0VQ5XA2BNH6SU","instance_id":"inst-x",
                 "task_id":null,"tool":"NOT_A_TOOL","operation_descriptor":"d",
                 "risk_level":"low","approval_id":null,"status":"started",
                 "started_at":"2026-10-14T00:00:00.000Z","finished_at":null}"#,
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
    use crate::AthenaStore;
    use crate::db::now_rfc3339;

    use crate::repository::*;
    use athena_types::*;

    fn seed_task(s: &mut AthenaStore) -> TaskId {
        let task = super::tests::sample_task(TaskState::Running);
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
        let mut s = AthenaStore::in_memory().expect("store");
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
        let mut s = AthenaStore::in_memory().expect("store");
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

    use crate::AthenaStore;

    use crate::repository::*;
    use athena_types::*;

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
        let mut s = AthenaStore::in_memory().expect("store");
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
