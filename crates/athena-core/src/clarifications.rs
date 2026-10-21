//! The clarification path (P4-18, `ATH-HITL-017..020`): a Core Agent question a human
//! must answer, with a deadline whose silence is **never** read as an answer.
//!
//! The deterministic orchestration only: open ⇒ `WAITING_INPUT`; answer ⇒ back to
//! `RUNNING` (or `REPLANNING` when the answer forces a plan change, the caller's
//! statement); deadline expiry ⇒ the Task `FAILED` — the route each way rides the
//! §17.1 transition tables through the store's atomic transition+Event primitive
//! (`ATH-PERS-003`). The Events (`ClarificationRequested` / `ClarificationAnswered`)
//! carry digests only (`ATH-EVENT-007`).

use crate::digest;
use athena_persistence::AthenaStore;
use athena_persistence::error::PersistenceError;
use athena_persistence::repository::{ClarificationRepository, TaskRepository};
use athena_types::{Clarification, ClarificationId, ClarificationState, Task, TaskId, TaskState};

/// Why the clarification path refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClarifyFault {
    /// The deadline must be in the future — an already-past ask would immediately
    /// fail its own Task.
    #[error("the clarification deadline is already past (ATH-HITL-018)")]
    DeadlinePast,
    /// A question needs text.
    #[error("the clarification question must not be empty (ATH-HITL-018)")]
    EmptyQuestion,
    /// The Task is not in a state that may ask (`RUNNING` per the §17.1 table).
    #[error("the Task is {0:?}; only a RUNNING task may ask a human (ATH-HITL-017)")]
    NotAsking(String),
}

/// Ask, with the owning Task named (the API-level entry).
///
/// # Errors
/// Store or table faults surface whole through [`ClarifyFailure`].
pub fn ask_task(
    store: &mut AthenaStore,
    task: &TaskId,
    question: &str,
    expires_at: &str,
    now: &str,
    correlation_id: &str,
) -> Result<Clarification, ClarifyFailure> {
    use athena_persistence::repository::EventRepository;
    if question.trim().is_empty() {
        return Err(ClarifyFailure::Fault(ClarifyFault::EmptyQuestion));
    }
    let task_row: Task = TaskRepository::get(store, task)
        .map_err(ClarifyFailure::Store)?
        .ok_or(ClarifyFailure::UnknownTask(task.as_str().to_string()))?;
    match task_row.state {
        TaskState::Running | TaskState::Replanning => {}
        other => {
            return Err(ClarifyFailure::Fault(ClarifyFault::NotAsking(format!(
                "{other:?}"
            ))));
        }
    }
    if deadline_passed(expires_at, &now_least(expires_at, now)) {
        return Err(ClarifyFailure::Fault(ClarifyFault::DeadlinePast));
    }
    let clarification = Clarification {
        id: ClarificationId::new(),
        task_id: task.clone(),
        state: ClarificationState::Requested,
        question: question.to_string(),
        answer: None,
        expires_at: expires_at.to_string(),
        created_at: now.to_string(),
        answered_at: None,
    };
    ClarificationRepository::open(store, &clarification).map_err(ClarifyFailure::Store)?;
    // The Task transition rides the atomic pair; the Event carries the digest only.
    let event = athena_persistence::NewEvent {
        event_id: athena_types::EventId::new(),
        schema_version: 1,
        event_type: "ClarificationRequested".into(),
        occurred_at: now.to_string(),
        task_id: Some(task.clone()),
        plan_step_id: None,
        agent_id: None,
        instance_id: None,
        skill_id: None,
        correlation_id: correlation_id.to_string(),
        causation_id: None,
        actor: "core_agent".into(),
        payload_json: serde_json::json!({
            "clarification_id": clarification.id.as_str(),
            "question_digest": digest(question),
        })
        .to_string(),
    };
    store
        .transition_task(task, task_row.state, TaskState::WaitingInput, &event)
        .map_err(ClarifyFailure::Store)?;
    let _ = EventRepository::max_sequence(store); // (a read; the append rode the transition)
    Ok(clarification)
}

/// Answer the human's question: the row goes `answered` and the Task resumes
/// (`WAITING_INPUT → RUNNING`, or `REPLANNING` when `requires_plan_change` is set —
/// the answer's meaning is the caller's statement, the table's destinations apply).
///
/// # Errors
/// Unknown/malformed id ⇒ [`ClarifyFailure::UnknownClarification`]; a row already
/// answered or expired refuses (`ClarifyFailure::Store` `NotFound`); store faults whole.
pub fn answer(
    store: &mut AthenaStore,
    clarification: &ClarificationId,
    text: &str,
    now: &str,
    correlation_id: &str,
    requires_plan_change: bool,
) -> Result<(), ClarifyFailure> {
    let row = ClarificationRepository::get(store, clarification)
        .map_err(ClarifyFailure::Store)?
        .ok_or_else(|| ClarifyFailure::UnknownClarification(clarification.as_str().to_string()))?;
    match row.state {
        ClarificationState::Requested => {}
        other => {
            return Err(ClarifyFailure::NotAnswerable(format!("{other:?}")));
        }
    }
    // Silence never answers; an answer past the deadline was already swept out — but a
    // just-in-time answer is legal only before the deadline.
    if deadline_passed(&row.expires_at, now) {
        return Err(ClarifyFailure::NotAnswerable(
            "the deadline already passed".into(),
        ));
    }
    ClarificationRepository::answer(store, clarification, text, now)
        .map_err(ClarifyFailure::Store)?;
    // The Task transitions through the asked state — the pool row is the expected state.
    let task_row: Task = TaskRepository::get(store, &row.task_id)
        .map_err(ClarifyFailure::Store)?
        .ok_or_else(|| ClarifyFailure::UnknownTask(row.task_id.as_str().to_string()))?;
    let to = if requires_plan_change {
        TaskState::Replanning
    } else {
        TaskState::Running
    };
    let event = athena_persistence::NewEvent {
        event_id: athena_types::EventId::new(),
        schema_version: 1,
        event_type: "ClarificationAnswered".into(),
        occurred_at: now.to_string(),
        task_id: Some(row.task_id.clone()),
        plan_step_id: None,
        agent_id: None,
        instance_id: None,
        skill_id: None,
        correlation_id: correlation_id.to_string(),
        causation_id: None,
        actor: "human".into(),
        payload_json: serde_json::json!({
            "clarification_id": clarification.as_str(),
            "question_digest": digest(&row.question),
        })
        .to_string(),
    };
    store
        .transition_task(&row.task_id, task_row.state, to, &event)
        .map_err(ClarifyFailure::Store)?;
    Ok(())
}

/// The expiry sweep (`ATH-HITL-018`): every requested row whose deadline passed marks
/// `expired` and its Task `FAILED` (`ClarificationExpired`) — silence is never an
/// answer.
///
/// # Errors
/// Store faults surface whole.
pub fn expiry_sweep(
    store: &mut AthenaStore,
    now: &str,
    correlation_id: &str,
) -> Result<Vec<TaskId>, ClarifyFailure> {
    let expired =
        ClarificationRepository::pending_expired(store, now).map_err(ClarifyFailure::Store)?;
    let mut tasks = Vec::new();
    for row in expired {
        ClarificationRepository::mark_expired(store, &row.id).map_err(ClarifyFailure::Store)?;
        let task_row: Option<Task> =
            TaskRepository::get(store, &row.task_id).map_err(ClarifyFailure::Store)?;
        if let Some(task_row) = task_row {
            let event = athena_persistence::NewEvent {
                event_id: athena_types::EventId::new(),
                schema_version: 1,
                event_type: "ClarificationExpired".into(),
                occurred_at: now.to_string(),
                task_id: Some(row.task_id.clone()),
                plan_step_id: None,
                agent_id: None,
                instance_id: None,
                skill_id: None,
                correlation_id: correlation_id.to_string(),
                causation_id: None,
                actor: "system".into(),
                payload_json: serde_json::json!({
                    "clarification_id": row.id.as_str(),
                    "question_digest": digest(&row.question),
                })
                .to_string(),
            };
            store
                .transition_task(&row.task_id, task_row.state, TaskState::Failed, &event)
                .map_err(ClarifyFailure::Store)?;
        }
        tasks.push(row.task_id.clone());
    }
    Ok(tasks)
}

/// Why the store-side fail surfaced.
#[derive(Debug, thiserror::Error)]
pub enum ClarifyFailure {
    /// The table or the store refused.
    #[error("{0}")]
    Store(#[from] PersistenceError),
    /// The rule refused.
    #[error("{0}")]
    Fault(#[from] ClarifyFault),
    /// The owning Task is unknown.
    #[error("the owning task {0:?} does not exist")]
    UnknownTask(String),
    /// The clarification is unknown.
    #[error("the clarification {0:?} does not exist")]
    UnknownClarification(String),
    /// The row cannot answer (already answered / expired / deadline past).
    #[error("the clarification cannot answer: {0}")]
    NotAnswerable(String),
}

/// Whether one deadline stamp is past `now` (lexical over RFC 3339; the store writes
/// uniform stamps).
#[must_use]
pub fn deadline_passed(expires_at: &str, now: &str) -> bool {
    expires_at.as_bytes() <= now.as_bytes()
}

/// The reflex that keeps a `now` stamp from out-sorting the check (a caller passing
/// equal stamps fails the ask).
fn now_least(expires_at: &str, now: &str) -> String {
    if expires_at.as_bytes() <= now.as_bytes() {
        expires_at.to_string()
    } else {
        now.to_string()
    }
}

#[cfg(test)]
mod clarify_tests {
    use super::{ClarifyFailure, answer, ask_task, deadline_passed, expiry_sweep};
    use athena_persistence::repository::{ClarificationRepository, TaskRepository};
    use athena_types::{ClarificationState, Task, TaskId, TaskState};

    fn task_row() -> Task {
        Task {
            id: TaskId::new(),
            parent_id: None,
            title: "Clarify".into(),
            instruction: "build the report".into(),
            workspace: "/work/clarify".into(),
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
    fn the_ask_opens_and_answers_resume_the_task() {
        let mut store = athena_persistence::AthenaStore::in_memory().expect("store");
        let task = task_row();
        TaskRepository::insert(&mut store, &task).expect("seeded");

        // Silence answered at the deadline edge is still legal, but the ask itself needs
        // a future deadline.
        let clarification = ask_task(
            &mut store,
            &task.id,
            "which shape does the report take?",
            "2026-10-18T00:00:00.000Z",
            "2026-10-17T00:00:00.000Z",
            "cor-clarify",
        )
        .expect("asked");
        assert_eq!(clarification.state, ClarificationState::Requested);

        // The Task is at WAITING_INPUT with the ClarificationRequested Event appended
        // (the store's own transition+Event pair).
        let after = TaskRepository::get(&store, &task.id)
            .expect("read")
            .expect("present");
        assert_eq!(after.state, TaskState::WaitingInput);

        // The answer resumes the Task.
        answer(
            &mut store,
            &clarification.id,
            "the one-page form",
            "2026-10-17T06:00:00.000Z",
            "cor-clarify",
            false,
        )
        .expect("answered");
        let resumed = TaskRepository::get(&store, &task.id)
            .expect("read")
            .expect("present");
        assert_eq!(resumed.state, TaskState::Running);

        // A second answer refuses (the row is already answered).
        assert!(matches!(
            answer(
                &mut store,
                &clarification.id,
                "again",
                "2026-10-17T07:00:00.000Z",
                "cor-clarify",
                false,
            ),
            Err(ClarifyFailure::NotAnswerable(_))
        ));
    }

    #[test]
    fn the_expired_silence_fails_the_task_never_answers_it() {
        let mut store = athena_persistence::AthenaStore::in_memory().expect("store");
        let task = task_row();
        TaskRepository::insert(&mut store, &task).expect("seeded");

        // Ask, with a deadline that passes quickly.
        let clarification = ask_task(
            &mut store,
            &task.id,
            "shape?",
            "2026-10-17T01:00:00.000Z",
            "2026-10-17T00:00:00.000Z",
            "cor-clarify",
        )
        .expect("asked");

        // The sweep at a later stamp: the Task goes FAILED — silence was never an answer.
        let swept =
            expiry_sweep(&mut store, "2026-10-17T02:00:00.000Z", "cor-clarify").expect("swept");
        assert_eq!(swept, vec![task.id.clone()]);
        let failed = TaskRepository::get(&store, &task.id)
            .expect("read")
            .expect("present");
        assert_eq!(failed.state, TaskState::Failed);
        let row = ClarificationRepository::get(&store, &clarification.id)
            .expect("read")
            .expect("present");
        assert_eq!(
            row.state,
            ClarificationState::Expired,
            "no fabricated answer"
        );

        // And a late answer cannot arrive: the sweep already disposed.
        assert!(matches!(
            answer(
                &mut store,
                &clarification.id,
                "late",
                "2026-10-17T02:30:00.000Z",
                "cor-clarify",
                false,
            ),
            Err(ClarifyFailure::NotAnswerable(_)) | Err(ClarifyFailure::Store(_))
        ));
    }

    #[test]
    fn the_ask_refuses_an_empty_question_and_a_past_deadline() {
        let mut store = athena_persistence::AthenaStore::in_memory().expect("store");
        let task = task_row();
        TaskRepository::insert(&mut store, &task).expect("seeded");
        assert!(matches!(
            ask_task(
                &mut store,
                &task.id,
                "   ",
                "2026-10-18T00:00:00.000Z",
                "2026-10-17T00:00:00.000Z",
                "cor-x"
            ),
            Err(ClarifyFailure::Fault(_))
        ));
        assert!(matches!(
            ask_task(
                &mut store,
                &task.id,
                "shape?",
                "2026-10-16T00:00:00.000Z",
                "2026-10-17T00:00:00.000Z",
                "cor-x"
            ),
            Err(ClarifyFailure::Fault(_))
        ));
        assert!(deadline_passed(
            "2026-10-17T00:00:00.000Z",
            "2026-10-17T00:00:01.000Z"
        ));
    }
}
