//! The Memory API (P4-09, `ATH-MEM-011`): reference and deletion over the same store
//! the Core Host owns — `GET /api/v1/memory` lists (optionally by `tier` / `scope`),
//! `DELETE /api/v1/memory/{id}` removes one record and appends the
//! `MemoryRecordDeleted` Event as its audit record (`ATH-MEM-006`).
//!
//! Role: `operator` per the spec's §22 route row. The listing is a *projection* of the
//! `memory_records` table through `MemoryRepository` (`ATH-MEM-012`): no separate file,
//! no vector store, and the contents carry the row's stored text as masked at write
//! time already (R-POL-13; the hygiene pass belongs to the writers, not the readers).

use crate::errors::ApiError;
use crate::tasks::SharedStore;
use athena_core::{EventRepository, MemoryRepository, NewEvent};
use athena_types::{MemoryId, MemoryTier};
use axum::{Json, http::StatusCode as HttpStatus, response::IntoResponse, response::Response};
use serde_json::json;

/// List memory rows: no filter returns every tier; `tier` narrows the tier; `scope`
/// further narrows the scope (`GET /api/v1/memory?tier=&scope=`).
///
/// # Errors
/// Unknown `tier` ⇒ 400 (`ValidationError`); store faults ⇒ 500.
#[allow(clippy::needless_pass_by_value)]
pub fn list(
    store: &SharedStore,
    page: &crate::pagination::Query,
    tier: Option<&str>,
    scope: Option<&str>,
) -> Result<Response, ApiError> {
    let tier = tier.map(tier_of).transpose()?;
    let guard = store.lock().expect("memory store poisoned");
    let rows = match (tier, scope) {
        (Some(t), Some(sc)) => {
            MemoryRepository::list(&*guard, t, sc).map_err(|e| crate::tasks::store_fault(e, ""))?
        }
        (Some(t), None) => by_tier(&guard, t)?,
        (None, Some(sc)) => filter_scope(&guard, sc)?,
        (None, None) => {
            let mut rows = by_tier(&guard, MemoryTier::Short)?;
            rows.extend(by_tier(&guard, MemoryTier::Working)?);
            rows.extend(by_tier(&guard, MemoryTier::Long)?);
            rows
        }
    };
    let page = crate::pagination::slice(&rows, page);
    Ok((
        HttpStatus::OK,
        Json(json!({ "items": page.items, "next_cursor": page.next_cursor })),
    )
        .into_response())
}

/// Delete one record (`DELETE /api/v1/memory/{id}`): the removal answer plus the
/// `MemoryRecordDeleted` Event appended as the audit record (`ATH-MEM-006`) in the same
/// store (D-1's record-through-the-Manager route; the caller's correlation rides the
/// Event).
///
/// # Errors
/// Malformed id ⇒ 404 (`TASK_NOT_FOUND`-style memory code); absent row ⇒ 404; store
/// faults ⇒ 500.
#[allow(clippy::needless_pass_by_value)]
pub fn delete(
    store: &SharedStore,
    id: &str,
    now: &str,
    correlation_id: &str,
) -> Result<Response, ApiError> {
    let Some(memory_id) = MemoryId::parse(id) else {
        return Err(ApiError::of_status(
            athena_core::protocol::ErrorClass::ValidationError,
            HttpStatus::NOT_FOUND,
            "MEMORY_NOT_FOUND",
            "malformed memory id on the path",
            json!({}),
            correlation_id.to_string(),
        ));
    };
    let mut guard = store.lock().expect("memory store poisoned");
    let record = MemoryRepository::get(&*guard, &memory_id)
        .map_err(|e| crate::tasks::store_fault(e, correlation_id))?
        .ok_or_else(|| {
            ApiError::of_status(
                athena_core::protocol::ErrorClass::ValidationError,
                HttpStatus::NOT_FOUND,
                "MEMORY_NOT_FOUND",
                "no such memory record",
                json!({}),
                correlation_id.to_string(),
            )
        })?;
    MemoryRepository::delete(&mut *guard, &memory_id)
        .map_err(|e| crate::tasks::store_fault(e, correlation_id))?;
    // The Mitchell audit record rides the same transaction boundary the caller commits —
    // the store appends it after the row is gone, so a reader can never see both.
    let event = NewEvent {
        event_id: athena_types::EventId::new(),
        schema_version: 1,
        event_type: "MemoryRecordDeleted".into(),
        occurred_at: now.to_string(),
        task_id: record.origin.task_id.clone(),
        plan_step_id: None,
        agent_id: None,
        instance_id: None,
        skill_id: None,
        correlation_id: correlation_id.to_string(),
        causation_id: None,
        actor: "system".into(),
        payload_json: serde_json::json!({
            "memory_id": memory_id.as_str(),
        })
        .to_string(),
    };
    EventRepository::append(&mut *guard, &event)
        .map_err(|e| crate::tasks::store_fault(e, correlation_id))?;
    Ok((
        HttpStatus::OK,
        Json(json!({
            "memory_id": memory_id.as_str(),
            "tier": tier_wire(record.tier),
            "scope": record.scope_id,
        })),
    )
        .into_response())
}

fn by_tier(
    store: &athena_core::SqliteStore,
    tier: MemoryTier,
) -> Result<Vec<athena_types::MemoryRecord>, ApiError> {
    MemoryRepository::list_by_tier(store, tier).map_err(|e| crate::tasks::store_fault(e, ""))
}

fn filter_scope(
    store: &athena_core::SqliteStore,
    scope: &str,
) -> Result<Vec<athena_types::MemoryRecord>, ApiError> {
    let mut rows: Vec<athena_types::MemoryRecord> = Vec::new();
    for tier in [MemoryTier::Short, MemoryTier::Working, MemoryTier::Long] {
        for record in MemoryRepository::list_by_tier(store, tier)
            .map_err(|e| crate::tasks::store_fault(e, ""))?
        {
            if record.scope_id == scope {
                rows.push(record);
            }
        }
    }
    Ok(rows)
}

fn tier_of(raw: &str) -> Result<MemoryTier, ApiError> {
    match raw {
        "short" => Ok(MemoryTier::Short),
        "working" => Ok(MemoryTier::Working),
        "long" => Ok(MemoryTier::Long),
        other => Err(ApiError::of_status(
            athena_core::protocol::ErrorClass::ValidationError,
            HttpStatus::BAD_REQUEST,
            "MEMORY_TIER_UNKNOWN",
            format!("unknown memory tier {other:?}; expected short | working | long"),
            json!({}),
            "",
        )),
    }
}

fn tier_wire(tier: MemoryTier) -> &'static str {
    match tier {
        MemoryTier::Short => "short",
        MemoryTier::Working => "working",
        MemoryTier::Long => "long",
    }
}
