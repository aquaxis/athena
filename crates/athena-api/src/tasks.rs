//! The Task surface (spec §22.3, P1-40): create / read / list / run / cancel / events.
//!
//! Rules the handlers live under:
//! - **Create** requires `instruction` + `workspace` (400 without); a workspace already
//!   leased to a non-terminal Task answers 409 (R-DATA-005, ath-API-023); `auto_run`
//!   queues at creation, so a later `run` is a 409 (R-API-17).
//! - **Run / cancel** ride the exact §17.1 rows: `athena-state` resolves the table, and
//!   `SqliteStore::transition_task` writes transition + Event atomically (ATH-PERS-003).
//!   An out-of-table move is a 409, never a fabrication.
//! - **Events** replay per Task through the store, cursor-paginated (P1-39).
//! - **Idempotency**: a `POST /tasks` presenting `Idempotency-Key` replays its recorded
//!   body identically (P1-39's `apply`).
//! - The other §22.3–§22.9 groups (Conversation, Plan read-only, agents / instances,
//!   Runtime, Skill, approvals, `/events/stream`) follow the same shape; their *write*
//!   halves belong to their phase owners per D-1, so those handlers answer 501 visibly
//!   rather than pretending.

use athena_core::protocol::ErrorClass;
use athena_core::{NewEvent, TaskRepository, TaskState, TaskTrigger, WorkspaceLeaseRepository};
use athena_types::{EventId, Task, TaskId};
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// The store holder the handlers share (`SqliteStore` rides the allowed edge list
/// through `athena-core`'s re-export).
pub type SharedStore = Arc<Mutex<athena_core::SqliteStore>>;

/// One row serialized per §22.3's field map.
#[must_use]
pub fn body_task(task: &Task) -> Value {
    json!({
        "id": task.id.as_str(),
        "title": task.title,
        "instruction": task.instruction,
        "workspace": task.workspace,
        "state": task.state,
        "priority": task.priority,
        "created_by": task.created_by,
        "created_at": task.created_at,
    })
}

/// A fresh Task of `state` from a creation payload (validated by the caller's shape
/// checks before the row is written).
#[must_use]
fn task_from(payload: &Value, caller_name: &str, state: TaskState, now: &str) -> Task {
    let empty = String::new();
    Task {
        id: TaskId::new(),
        parent_id: None,
        title: payload
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or(&empty)
            .into(),
        instruction: payload
            .get("instruction")
            .and_then(Value::as_str)
            .unwrap_or(&empty)
            .into(),
        workspace: payload
            .get("workspace")
            .and_then(Value::as_str)
            .unwrap_or(&empty)
            .into(),
        goal: None,
        constraints: vec![],
        completion_criteria: vec![],
        state,
        priority: payload
            .get("priority")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or(athena_types::Priority::Normal),
        plan_id: None,
        risk_level: athena_types::RiskLevel::Medium,
        attempt_count: 0,
        max_attempts: 3,
        iteration_count: 0,
        max_iterations: 50,
        deadline: None,
        created_by: caller_name.into(),
        created_at: now.into(),
        updated_at: now.into(),
        finished_at: None,
    }
}

/// Create a Task (POST /tasks): shape-check, lease the workspace, optionally queue.
///
/// # Errors
/// 400 for a missing `instruction` / `workspace`, 409 for a leased workspace, 500 on a
/// store fault.
pub fn create_task(
    store: &SharedStore,
    caller_name: &str,
    payload: &Value,
    now: &str,
    correlation_id: &str,
) -> Result<Response, crate::errors::ApiError> {
    let empty = String::new();
    let instruction = payload
        .get("instruction")
        .and_then(Value::as_str)
        .unwrap_or(&empty);
    let workspace = payload
        .get("workspace")
        .and_then(Value::as_str)
        .unwrap_or(&empty);
    if workspace.is_empty() {
        return Err(shape_violation(
            "TASK_WORKSPACE_REQUIRED",
            "`workspace` is required",
        ));
    }
    if instruction.is_empty() {
        return Err(shape_violation(
            "TASK_INSTRUCTION_REQUIRED",
            "`instruction` is required (the verbatim text is kept, ATH-DATA-004)",
        ));
    }
    let auto_run = payload
        .get("auto_run")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut guard = store.lock().expect("task store poisoned");
    if WorkspaceLeaseRepository::holder(&*guard, workspace)
        .map_err(|e| store_fault(e, correlation_id))?
        .is_some()
    {
        return Err(leased(workspace, correlation_id));
    }

    let task = task_from(payload, caller_name, TaskState::Created, now);
    let task_id = task.id.clone();
    guard
        .create_task_with_event(&task, &EventId::new(), now, correlation_id)
        .map_err(|e| store_fault(e, correlation_id))?;
    WorkspaceLeaseRepository::acquire(&mut *guard, workspace, &task_id)
        .map_err(|e| store_fault(e, correlation_id))?;
    if auto_run {
        // The lease and the transition ride the Task's lifetime; the store's atomic
        // transition and event pair carries `TaskQueued` (ATH-PERS-003).
        WorkspaceLeaseRepository::acquire(&mut *guard, workspace, &task_id)
            .map_err(|e| store_fault(e, correlation_id))?;
        let queued = NewEvent {
            event_id: EventId::new(),
            schema_version: 1,
            event_type: "TaskQueued".into(),
            occurred_at: now.into(),
            task_id: Some(task_id.clone()),
            plan_step_id: None,
            agent_id: None,
            instance_id: None,
            skill_id: None,
            correlation_id: correlation_id.into(),
            causation_id: None,
            actor: "human".into(),
            payload_json: json!({ "workspace": workspace }).to_string(),
        };
        transition_to_queued(&mut guard, &task_id, &queued)
            .map_err(|e| store_fault(e, correlation_id))?;
    }

    // ATH-API-021: when the create payload names a conversation, the Task relates —
    // the relation forwards only (the wiring records the link; an absorbing failure
    // surfaces as the API's own fact, never swallowed).
    if let Some(conversation_id) = payload.get("conversation_id").and_then(Value::as_str) {
        crate::conversations::link_conversation_to_task(&mut guard, conversation_id, &task_id)?;
    }

    let mut body = body_task(&task);
    body["id"] = json!(task_id.as_str());
    Ok((StatusCode::CREATED, Json(body)).into_response())
}

/// Read a Task (GET /tasks/{id}): 404 when absent, whole.
///
/// # Errors
/// 404 / 500 through [`ApiError`].
pub fn read_task(store: &SharedStore, task: &TaskId) -> Result<Response, crate::errors::ApiError> {
    let guard = store.lock().expect("task store poisoned");
    let found = TaskRepository::get(&*guard, task).map_err(|e| store_fault(e, ""))?;
    match found {
        Some(row) => Ok((StatusCode::OK, Json(body_task(&row))).into_response()),
        None => Err(not_found(task)),
    }
}

/// List Tasks, cursor-paginated (GET /tasks).
///
/// # Errors
/// Store faults, whole.
pub fn list_tasks(
    store: &SharedStore,
    page: &crate::pagination::Query,
) -> Result<Response, crate::errors::ApiError> {
    let guard = store.lock().expect("task store poisoned");
    let start = page.cursor.unwrap_or(0);
    let items = TaskRepository::list_page(&*guard, start, page.limit)
        .map_err(|e| store_fault(e, ""))?;
    let next_cursor: Option<u64> = (page.limit > 0 && items.len() == page.limit as usize)
        .then(|| start + page.limit);
    Ok((
        StatusCode::OK,
        Json(json!({ "items": items, "next_cursor": next_cursor })),
    )
        .into_response())
}

/// Run a Task (POST /tasks/{id}/run): `CREATED → QUEUED` (`Submitted`), 409 otherwise.
///
/// # Errors
/// 404 when absent, 409 for out-of-table moves, 500 on store faults.
pub fn run_task(
    store: &SharedStore,
    task: &TaskId,
    now: &str,
    correlation_id: &str,
) -> Result<Response, crate::errors::ApiError> {
    transition(store, task, TaskTrigger::Submitted, now, correlation_id)
}

/// Cancel a Task (POST /tasks/{id}/cancel): `CREATED → CANCELLED`
/// (`CancelledBeforeStart`) or `→ CANCELLING` (`CancelRequested`), 409 otherwise.
///
/// # Errors
/// 404 when absent, 409 out-of-table, 500 store fault.
pub fn cancel_task(
    store: &SharedStore,
    task: &TaskId,
    now: &str,
    correlation_id: &str,
) -> Result<Response, crate::errors::ApiError> {
    {
        let guard = store.lock().expect("task store poisoned");
        let row = TaskRepository::get(&*guard, task)
            .map_err(|e| store_fault(e, correlation_id))?
            .ok_or_else(|| not_found(task))?;
        if row.state == TaskState::Created {
            return transition(
                store,
                task,
                TaskTrigger::CancelledBeforeStart,
                now,
                correlation_id,
            );
        }
    }
    transition(
        store,
        task,
        TaskTrigger::CancelRequested,
        now,
        correlation_id,
    )
}

/// Events of one Task (GET /tasks/{id}/events): §23 replay, cursor-paginated.
///
/// # Errors
/// 404 when absent, 500 on store faults.
pub fn task_events(
    store: &SharedStore,
    task: &TaskId,
    page: &crate::pagination::Query,
) -> Result<Response, crate::errors::ApiError> {
    use athena_core::{EventRepository, TaskRepository};
    let guard = store.lock().expect("task store poisoned");
    TaskRepository::get(&*guard, task)
        .map_err(|e| store_fault(e, ""))?
        .ok_or_else(|| not_found(task))?;
    let events = EventRepository::list_for_task(&*guard, task).map_err(|e| store_fault(e, ""))?;
    let page = crate::pagination::slice(&events, page);
    let items: Vec<Value> = page
        .items
        .iter()
        .map(|event| {
            json!({
                "event_id": event.event_id.as_str(),
                "type": event.event_type,
                "occurred_at": event.occurred_at,
                "sequence": event.sequence,
            })
        })
        .collect();
    Ok((
        StatusCode::OK,
        Json(json!({ "items": items, "next_cursor": page.next_cursor })),
    )
        .into_response())
}

/// Clarification answer (POST /tasks/{id}/answer): routes through
/// `athena_core::clarifications` — the row goes `answered` and the Task transitions
/// (`WAITING_INPUT → RUNNING`, or `REPLANNING` when the answer forces a plan change).
///
/// # Errors
/// Unknown task ⇒ 404 (`TASK_NOT_FOUND`); unanswerable ⇒ 409 (`CLARIFICATION_REFUSED`);
/// store faults ⇒ 500.
#[allow(clippy::needless_pass_by_value)]
pub fn answer_task(
    store: &SharedStore,
    task: &TaskId,
    answer: &str,
    requires_plan_change: bool,
    now: &str,
    correlation_id: &str,
) -> Result<Response, crate::errors::ApiError> {
    // The Task's pending clarifications: answering resumes the latest one.
    let mut guard = store.lock().expect("store");
    let pending = athena_core::ClarificationRepository::pending_for_task(&*guard, task)
        .map_err(|e| store_fault(e, correlation_id))?;
    let Some(row) = pending.last().cloned() else {
        return Err(crate::errors::ApiError::of_status(
            athena_core::protocol::ErrorClass::ValidationError,
            StatusCode::NOT_FOUND,
            "TASK_NOT_FOUND",
            "the task has no open clarification to answer",
            serde_json::json!({}),
            correlation_id,
        ));
    };
    athena_core::clarifications::answer(
        &mut guard,
        &row.id,
        answer,
        now,
        correlation_id,
        requires_plan_change,
    )
    .map(|_| {
        (
            StatusCode::OK,
            Json(json!({
                "task_id": task.as_str(),
                "clarification_id": row.id.as_str(),
                "requires_plan_change": requires_plan_change,
            })),
        )
            .into_response()
    })
    .map_err(|failure| match failure {
        athena_core::clarifications::ClarifyFailure::NotAnswerable(reason)
        | athena_core::clarifications::ClarifyFailure::UnknownTask(reason)
        | athena_core::clarifications::ClarifyFailure::UnknownClarification(reason) => {
            crate::errors::ApiError::of_status(
                athena_core::protocol::ErrorClass::ValidationError,
                StatusCode::CONFLICT,
                "CLARIFICATION_REFUSED",
                reason,
                serde_json::json!({}),
                correlation_id,
            )
        }
        athena_core::clarifications::ClarifyFailure::Fault(fault) => {
            crate::errors::ApiError::of_status(
                athena_core::protocol::ErrorClass::ValidationError,
                StatusCode::BAD_REQUEST,
                "CLARIFICATION_FAULT",
                fault.to_string(),
                serde_json::json!({}),
                correlation_id,
            )
        }
        other => store_fault(
            athena_core::persistence_internal(other.to_string()),
            correlation_id,
        ),
    })
}

// --- internals ---------------------------------------------------------------------------

fn transition(
    store: &SharedStore,
    task: &TaskId,
    trigger: TaskTrigger,
    now: &str,
    correlation_id: &str,
) -> Result<Response, crate::errors::ApiError> {
    let mut guard = store.lock().expect("task store poisoned");
    let row = TaskRepository::get(&*guard, task)
        .map_err(|e| store_fault(e, correlation_id))?
        .ok_or_else(|| not_found(task))?;
    let resolved = athena_core::task_transition(row.state, trigger)
        .ok_or_else(|| out_of_table(task, row.state))?;
    let event = NewEvent {
        event_id: EventId::new(),
        schema_version: 1,
        event_type: resolved.event.into(),
        occurred_at: now.into(),
        task_id: Some(task.clone()),
        plan_step_id: None,
        agent_id: None,
        instance_id: None,
        skill_id: None,
        correlation_id: correlation_id.into(),
        causation_id: None,
        actor: "human".into(),
        payload_json: json!({ "requester": "api" }).to_string(),
    };
    let stored = guard
        .transition_task(task, row.state, resolved.to, &event)
        .map_err(|e| store_fault(e, correlation_id))?;
    Ok((
        StatusCode::OK,
        Json(json!({
            "task_id": task.as_str(),
            "state": resolved.to,
            "event_id": stored.event_id.as_str(),
        })),
    )
        .into_response())
}

fn transition_to_queued(
    guard: &mut athena_core::SqliteStore,
    task: &TaskId,
    event: &NewEvent,
) -> Result<athena_core::StoredEvent, athena_core::PersistenceError> {
    guard.transition_task(task, TaskState::Created, TaskState::Queued, event)
}

fn not_found(task: &TaskId) -> crate::errors::ApiError {
    crate::errors::ApiError::of_status(
        ErrorClass::ValidationError,
        StatusCode::NOT_FOUND,
        "TASK_NOT_FOUND",
        format!("no Task {task:?}"),
        json!({}),
        "",
    )
}

fn leased(workspace: &str, correlation_id: &str) -> crate::errors::ApiError {
    crate::errors::ApiError::of_status(
        ErrorClass::ValidationError,
        StatusCode::CONFLICT,
        "WORKSPACE_LEASED",
        "the workspace already holds a non-terminal Task",
        json!({ "workspace": workspace }),
        correlation_id,
    )
}

fn out_of_table(task: &TaskId, state: TaskState) -> crate::errors::ApiError {
    crate::errors::ApiError::of_status(
        ErrorClass::ValidationError,
        StatusCode::CONFLICT,
        "TASK_TRANSITION_REFUSED",
        format!("the §17.1 table refuses the move from {:?}", state),
        json!({ "task": task.as_str(), "current_state": state }),
        "",
    )
}

fn shape_violation(code: &'static str, message: &'static str) -> crate::errors::ApiError {
    crate::errors::ApiError::of_class(ErrorClass::ValidationError, code, message, json!({}), "")
}

pub(crate) fn store_fault(
    error: athena_core::PersistenceError,
    correlation_id: &str,
) -> crate::errors::ApiError {
    crate::errors::ApiError::of_class(
        ErrorClass::InternalError,
        "STORE_FAULT",
        error.to_string(),
        json!({}),
        correlation_id,
    )
}
