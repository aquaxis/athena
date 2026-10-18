//! The Conversation API (P6-02; `ATH-API-021`): pre-Task natural-language dialogue.
//!
//! `POST /api/v1/conversations` opens one (201; the id is the handle);
//! `POST /api/v1/conversations/{id}/messages` records a human utterance (the Core
//! Agent's replies ride the same recording through the wiring); and **Task creation
//! links the conversation** when the create payload names one
//! (`Conversation.task_id` relates once — the relation forwards only; the linked
//! conversation is never re-pointed backward).

use crate::errors::ApiError;
use crate::routes::{AppState, now_text};
use athena_core::ConversationRepository;
use athena_core::protocol::ErrorClass;
use athena_types::{Conversation, ConversationId, TaskId, Utterance, UtteranceKind, UtteranceRole};
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

fn fault(status: StatusCode, code: &str, detail: Value) -> Response {
    (
        status,
        Json(json!({"error": {"code": code, "detail": detail}})),
    )
        .into_response()
}

fn open(state: &AppState) -> Option<Arc<Mutex<athena_core::SqliteStore>>> {
    state.rows.clone()
}

/// `POST /api/v1/conversations` — open a conversation (201; its id is the handle).
pub async fn open_conversation(State(state): State<AppState>) -> Response {
    let Some(store) = open(&state) else {
        return fault(
            StatusCode::SERVICE_UNAVAILABLE,
            "CORE_STORE_UNAVAILABLE",
            json!({}),
        );
    };
    let mut guard = store.lock().expect("store lock");
    let conversation = Conversation {
        id: ConversationId::new(),
        task_id: None,
        created_at: now_text(),
    };
    match ConversationRepository::insert(&mut *guard, &conversation) {
        Ok(()) => (
            StatusCode::CREATED,
            Json(json!({
                "id": conversation.id.as_str().to_string(),
                "task_id": Option::<String>::None,
                "created_at": conversation.created_at
            })),
        )
            .into_response(),
        Err(error) => fault(
            StatusCode::INTERNAL_SERVER_ERROR,
            "CONVERSATION_STORE_FAULT",
            json!(error.to_string()),
        ),
    }
}

/// `POST /api/v1/conversations/{id}/messages` — record a human utterance
/// (`{"body": …}`, optionally `{"clarification_id": …}`). The conversation must exist
/// (404, never a silent new one); the record is immutable.
pub async fn post_utterance(
    axum::extract::Path(conversation_id): axum::extract::Path<String>,
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Response {
    let body = payload
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if body.trim().is_empty() {
        return fault(
            StatusCode::BAD_REQUEST,
            "UTTERANCE_BODY_REQUIRED",
            json!({"fields": ["body"]}),
        );
    }
    let Some(store) = open(&state) else {
        return fault(
            StatusCode::SERVICE_UNAVAILABLE,
            "CORE_STORE_UNAVAILABLE",
            json!({}),
        );
    };
    let mut guard = store.lock().expect("store lock");
    let Some(existing) = ConversationId::parse(conversation_id.clone()) else {
        return fault(
            StatusCode::NOT_FOUND,
            "CONVERSATION_ABSENT",
            json!({"id": conversation_id}),
        );
    };
    match ConversationRepository::get(&*guard, &existing) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return fault(
                StatusCode::NOT_FOUND,
                "CONVERSATION_ABSENT",
                json!({"id": conversation_id}),
            );
        }
        Err(error) => {
            return fault(
                StatusCode::INTERNAL_SERVER_ERROR,
                "CONVERSATION_STORE_FAULT",
                json!(error.to_string()),
            );
        }
    }
    let utterance = Utterance {
        id: athena_types::UtteranceId::new(),
        conversation_id: existing,
        role: UtteranceRole::Human,
        kind: UtteranceKind::Message,
        body: body.to_string(),
        clarification_id: payload
            .get("clarification_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        created_at: now_text(),
    };
    match ConversationRepository::append_utterance(&mut *guard, &utterance) {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(json!({
                "id": utterance.id.as_str().to_string(),
                "conversation_id": utterance.conversation_id.as_str().to_string()
            })),
        )
            .into_response(),
        Err(error) => fault(
            StatusCode::INTERNAL_SERVER_ERROR,
            "CONVERSATION_STORE_FAULT",
            json!(error.to_string()),
        ),
    }
}

/// `GET /api/v1/conversations/{id}/messages` — the recorded turns in order.
pub async fn list_utterances(
    axum::extract::Path(conversation_id): axum::extract::Path<String>,
    State(state): State<AppState>,
) -> Response {
    let Some(store) = open(&state) else {
        return fault(
            StatusCode::SERVICE_UNAVAILABLE,
            "CORE_STORE_UNAVAILABLE",
            json!({}),
        );
    };
    let Some(existing) = ConversationId::parse(conversation_id.clone()) else {
        return fault(
            StatusCode::NOT_FOUND,
            "CONVERSATION_ABSENT",
            json!({"id": conversation_id}),
        );
    };
    let guard = store.lock().expect("store lock");
    match ConversationRepository::list_utterances(&*guard, &existing) {
        Ok(rows) => Json(json!({"utterances": rows})).into_response(),
        Err(error) => fault(
            StatusCode::INTERNAL_SERVER_ERROR,
            "CONVERSATION_STORE_FAULT",
            json!(error.to_string()),
        ),
    }
}

/// The Task-create link face: when a Task's create payload names a `conversation_id`
/// and the conversation carries no Task yet, the Task relates — the relation
/// forwards only. `POST /api/v1/tasks` calls this after the row lands.
///
/// # Errors
/// `ApiError` with the class's default mapping.
pub fn link_conversation_to_task(
    guard: &mut athena_core::SqliteStore,
    conversation_id: &str,
    task: &TaskId,
) -> Result<(), ApiError> {
    let Some(id) = ConversationId::parse(conversation_id.to_string()) else {
        return Ok(()); // no conversation named: nothing to link
    };
    let existing = ConversationRepository::get(guard, &id).map_err(|error| {
        ApiError::of_status(
            ErrorClass::InternalError,
            StatusCode::INTERNAL_SERVER_ERROR,
            "CONVERSATION_STORE_FAULT",
            error.to_string(),
            json!({}),
            String::new(),
        )
    })?;
    match existing {
        Some(conversation) if conversation.task_id.is_none() => {
            ConversationRepository::link_task(guard, &id, task).map_err(|error| {
                ApiError::of_status(
                    ErrorClass::InternalError,
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "CONVERSATION_LINK_FAULT",
                    error.to_string(),
                    json!({}),
                    String::new(),
                )
            })
        }
        _ => Ok(()), // absorbing (already linked, or the conversation is absent)
    }
}
