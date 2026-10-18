//! The Approval API (P4-19): `GET /api/v1/approvals`, `GET /{id}`, `POST /{id}/approve`,
//! `POST /{id}/reject` — role `operator` per the §22 row, with the approver identity
//! recorded (`ATH-HITL-006`) and terminal approvals refusing further answers (409).
//!
//! The decisions live in Core Host (`athena_core::approval_manager`): the descriptors,
//! the §17.4 state-machine rows (P2-02's `athena-state` table via the store), and
//! P4-15's consumption gate — the API only carries the human's statement plus their
//! identity trail. The deletion audit rides `ApprovalGranted` / `ApprovalRejected`
//! Events appended in the same store transaction window as the row update (D-1).

use athena_core::{ApprovalRepository, EventRepository, NewEvent};
use athena_types::{ApprovalId, ApprovalState};
use axum::{Json, http::StatusCode, response::IntoResponse, response::Response};
use serde_json::{Value, json};

use crate::errors::ApiError;
use crate::tasks::SharedStore;

/// List approvals, optionally filtered by state (`GET /api/v1/approvals?state=`).
///
/// # Errors
/// Unknown state ⇒ 400; store faults ⇒ 500.
#[allow(clippy::needless_pass_by_value)]
pub fn list(store: &SharedStore, state: Option<&str>) -> Result<Response, ApiError> {
    let guard = store.lock().expect("store");
    let rows = match state {
        None => {
            let mut rows = ApprovalRepository::list_by_state(&*guard, ApprovalState::Requested)
                .map_err(|e| crate::tasks::store_fault(e, ""))?;
            rows.extend(
                ApprovalRepository::list_by_state(&*guard, ApprovalState::Granted)
                    .map_err(|e| crate::tasks::store_fault(e, ""))?,
            );
            rows.extend(
                ApprovalRepository::list_by_state(&*guard, ApprovalState::Consumed)
                    .map_err(|e| crate::tasks::store_fault(e, ""))?,
            );
            rows.extend(
                ApprovalRepository::list_by_state(&*guard, ApprovalState::Rejected)
                    .map_err(|e| crate::tasks::store_fault(e, ""))?,
            );
            rows.extend(
                ApprovalRepository::list_by_state(&*guard, ApprovalState::Expired)
                    .map_err(|e| crate::tasks::store_fault(e, ""))?,
            );
            rows
        }
        Some(raw) => {
            let state = state_of(raw)?;
            ApprovalRepository::list_by_state(&*guard, state)
                .map_err(|e| crate::tasks::store_fault(e, ""))?
        }
    };
    let items: Vec<Value> = rows.iter().map(body_approval).collect();
    Ok((
        StatusCode::OK,
        Json(json!({ "items": items, "next_cursor": Value::Null })),
    )
        .into_response())
}

/// Read one approval (`GET /api/v1/approvals/{id}`).
///
/// # Errors
/// Malformed id or absent row ⇒ 404; store faults ⇒ 500.
#[allow(clippy::needless_pass_by_value)]
pub fn read(store: &SharedStore, id: &str, correlation_id: &str) -> Result<Response, ApiError> {
    let Some(approval_id) = ApprovalId::parse(id) else {
        return Err(ApprovalApiFault::malformed(id, correlation_id));
    };
    let guard = store.lock().expect("store");
    let row = ApprovalRepository::get(&*guard, &approval_id)
        .map_err(|e| crate::tasks::store_fault(e, correlation_id))?
        .ok_or_else(|| ApprovalApiFault::absent(id, correlation_id))?;
    Ok((StatusCode::OK, Json(body_approval(&row))).into_response())
}

/// The human's answer (`approve` / `reject`): the row transitions through the §17.4
/// table with the approver identity recorded (`ATH-HITL-006`); terminal rows refuse
/// (409); the `ApprovalGranted` / `ApprovalRejected` Event is the audit trail, appended
/// in the same store window (D-1).
///
/// # Errors
/// Malformed id / absent row ⇒ 404; terminal (not `REQUESTED`) ⇒ 409; store faults ⇒ 500.
#[allow(clippy::needless_pass_by_value)]
pub fn answer(
    store: &SharedStore,
    id: &str,
    approve: bool,
    approver: &str,
    now: &str,
    correlation_id: &str,
) -> Result<Response, ApiError> {
    let Some(approval_id) = ApprovalId::parse(id) else {
        return Err(ApprovalApiFault::malformed(id, correlation_id));
    };
    let mut guard = store.lock().expect("store");
    let row = ApprovalRepository::get(&*guard, &approval_id)
        .map_err(|e| crate::tasks::store_fault(e, correlation_id))?
        .ok_or_else(|| ApprovalApiFault::absent(id, correlation_id))?;
    if row.state != ApprovalState::Requested {
        return Err(ApiError::of_status(
            athena_core::protocol::ErrorClass::ValidationError,
            StatusCode::CONFLICT,
            "APPROVAL_TERMINAL",
            format!("the approval is already {}", row.state.as_str()),
            json!({}),
            correlation_id,
        ));
    }
    let (state, event_type) = if approve {
        (ApprovalState::Granted, "ApprovalGranted")
    } else {
        (ApprovalState::Rejected, "ApprovalRejected")
    };
    ApprovalRepository::update_state(
        &mut *guard,
        &approval_id,
        state,
        Some(approver),
        match &row.scope {
            athena_types::ApprovalScope::OperationOnly { .. } => 0,
            athena_types::ApprovalScope::Pattern { .. } => row.uses_remaining.max(1),
        },
    )
    .map_err(|e| crate::tasks::store_fault(e, correlation_id))?;
    // The audit Event: `ApprovalGranted` / `ApprovalRejected` with the descriptor
    // reference (`ATH-EVENT-007`: references only; the descriptor rides the row).
    let event = NewEvent {
        event_id: athena_types::EventId::new(),
        schema_version: 1,
        event_type: event_type.into(),
        occurred_at: now.to_string(),
        task_id: Some(row.task_id.clone()),
        plan_step_id: None,
        agent_id: Some(row.requested_by.clone()),
        instance_id: Some(row.requested_by_instance_id.clone()),
        skill_id: None,
        correlation_id: correlation_id.to_string(),
        causation_id: None,
        actor: "human".into(),
        payload_json: json!({
            "approval_id": approval_id.as_str(),
            "operation_descriptor": row.operation_descriptor,
            "responded_by": approver,
        })
        .to_string(),
    };
    EventRepository::append(&mut *guard, &event)
        .map_err(|e| crate::tasks::store_fault(e, correlation_id))?;
    Ok((
        StatusCode::OK,
        Json(json!({
            "approval_id": approval_id.as_str(),
            "state": state.as_str(),
            "responded_by": approver,
        })),
    )
        .into_response())
}

fn body_approval(row: &athena_types::Approval) -> Value {
    json!({
        "id": row.id.as_str(),
        "task_id": row.task_id.as_str(),
        "requested_by": row.requested_by.as_str(),
        "operation": row.operation,
        "operation_descriptor": row.operation_descriptor,
        "reason_untrusted": row.reason,
        "impact_untrusted": row.impact,
        "risk": row.risk_level.as_str(),
        "state": row.state.as_str(),
        "uses_remaining": row.uses_remaining,
        "expires_at": row.expires_at,
        "responded_by": row.responded_by,
        "responded_at": row.responded_at,
    })
}

fn state_of(raw: &str) -> Result<ApprovalState, ApiError> {
    match raw.to_ascii_lowercase().as_str() {
        "requested" => Ok(ApprovalState::Requested),
        "granted" => Ok(ApprovalState::Granted),
        "rejected" => Ok(ApprovalState::Rejected),
        "expired" => Ok(ApprovalState::Expired),
        "consumed" => Ok(ApprovalState::Consumed),
        other => Err(ApiError::of_status(
            athena_core::protocol::ErrorClass::ValidationError,
            StatusCode::BAD_REQUEST,
            "APPROVAL_STATE_UNKNOWN",
            format!("unknown approval state {other:?}"),
            json!({}),
            "",
        )),
    }
}

/// The API's own fault faces.
struct ApprovalApiFault;

impl ApprovalApiFault {
    fn malformed(id: &str, correlation_id: &str) -> ApiError {
        ApiError::of_status(
            athena_core::protocol::ErrorClass::ValidationError,
            StatusCode::NOT_FOUND,
            "APPROVAL_NOT_FOUND",
            format!("malformed approval id {id:?}"),
            json!({}),
            correlation_id,
        )
    }

    fn absent(id: &str, correlation_id: &str) -> ApiError {
        ApiError::of_status(
            athena_core::protocol::ErrorClass::ValidationError,
            StatusCode::NOT_FOUND,
            "APPROVAL_NOT_FOUND",
            format!("no approval {id:?}"),
            json!({}),
            correlation_id,
        )
    }
}
