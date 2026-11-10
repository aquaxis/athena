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
use athena_core::{EventRepository, MemoryRepository, NewEvent, TaskRepository, mask_text};
use athena_types::{MemoryId, MemoryTier};
use axum::{Json, http::StatusCode as HttpStatus, response::IntoResponse, response::Response};
use serde_json::Value;
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

/// Create one Long Term Memory record (`POST /api/v1/memory`, X-34): the human
/// instruction requester face of `ATH-MEM-003`. The body carries `content` (required),
/// `workspace` (required — its normalized absolute path derives the `ProjectId` scope,
/// `R-DAT-02`), and at least one provenance anchor (`task_id` and/or `event_id`,
/// `ATH-MEM-005`). The hygiene pass runs here on the writer's side (`R-POL-13`), the
/// `long_term::review` + `write_with_retention` gate enforces the explicit-requester /
/// provenance / disabled-tier / per-scopeceiling rules, and the record's creation is
/// completed by the `MemoryRecordCreated` Event appended in the same pass (D-1's
/// record-through-the-repository route — the same shape the deletion face records).
///
/// # Errors
/// Empty `content`/`workspace` or an unparseable anchor ⇒ 400 (`ValidationError`);
/// the named Task / Event anchor absent ⇒ 404; the tier disabled by configuration ⇒ 403
/// `MEMORY_LONG_TERM_DISABLED`; the per-scope ceiling reached ⇒ 409
/// `MEMORY_CEILING_REACHED`; store faults ⇒ 500.
#[allow(clippy::too_many_lines, reason = "one explicit gate per refusal face")]
pub fn create(
    store: &SharedStore,
    config: &athena_core::Config,
    payload: &Value,
    caller_identity: &str,
    now: &str,
    correlation_id: &str,
) -> Result<Response, ApiError> {
    use athena_core::memory_manager::long_term;

    let empty = String::new();
    let content = payload
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or(&empty);
    let workspace = payload
        .get("workspace")
        .and_then(Value::as_str)
        .unwrap_or(&empty);
    if content.trim().is_empty() {
        return Err(ApiError::of_status(
            athena_core::protocol::ErrorClass::ValidationError,
            HttpStatus::BAD_REQUEST,
            "MEMORY_CONTENT_REQUIRED",
            "`content` is required and non-empty (a Long write is an explicit judgement)",
            json!({}),
            correlation_id.to_string(),
        ));
    }
    if workspace.trim().is_empty() {
        return Err(ApiError::of_status(
            athena_core::protocol::ErrorClass::ValidationError,
            HttpStatus::BAD_REQUEST,
            "MEMORY_SCOPE_REQUIRED",
            "`workspace` is required (the record's ProjectId scope derives from it, R-DAT-02)",
            json!({}),
            correlation_id.to_string(),
        ));
    }
    // The anchors: parse both faces first (malformed ⇒ 400), then require at least one
    // (`ATH-MEM-005`), then require the referenced rows to exist (404 — provenance that
    // names nothing real would be worse than none).
    let task_anchor = match payload.get("task_id").and_then(Value::as_str) {
        Some(raw) if !raw.trim().is_empty() => {
            Some(athena_types::TaskId::parse(raw).ok_or_else(|| {
                ApiError::of_status(
                    athena_core::protocol::ErrorClass::ValidationError,
                    HttpStatus::BAD_REQUEST,
                    "MEMORY_TASK_ID_MALFORMED",
                    "`task_id` is not a well-formed Task id",
                    json!({ "task_id": raw }),
                    correlation_id.to_string(),
                )
            })?)
        }
        _ => None,
    };
    let event_anchor = match payload.get("event_id").and_then(Value::as_str) {
        Some(raw) if !raw.trim().is_empty() => {
            Some(athena_types::EventId::parse(raw).ok_or_else(|| {
                ApiError::of_status(
                    athena_core::protocol::ErrorClass::ValidationError,
                    HttpStatus::BAD_REQUEST,
                    "MEMORY_EVENT_ID_MALFORMED",
                    "`event_id` is not a well-formed Event id",
                    json!({ "event_id": raw }),
                    correlation_id.to_string(),
                )
            })?)
        }
        _ => None,
    };
    if task_anchor.is_none() && event_anchor.is_none() {
        return Err(ApiError::of_status(
            athena_core::protocol::ErrorClass::ValidationError,
            HttpStatus::BAD_REQUEST,
            "MEMORY_ANCHOR_REQUIRED",
            "a Long record carries provenance: pass `task_id` and/or `event_id` (ATH-MEM-005)",
            json!({}),
            correlation_id.to_string(),
        ));
    }
    if let Some(task_id) = &task_anchor {
        let present = {
            let guard = store.lock().expect("memory store poisoned");
            TaskRepository::get(&*guard, task_id)
                .map_err(|e| crate::tasks::store_fault(e, correlation_id))?
                .is_some()
        };
        if !present {
            return Err(ApiError::of_status(
                athena_core::protocol::ErrorClass::ValidationError,
                HttpStatus::NOT_FOUND,
                "MEMORY_ANCHOR_NOT_FOUND",
                "the anchored Task does not exist",
                json!({ "task_id": task_id.as_str() }),
                correlation_id.to_string(),
            ));
        }
    }
    if let Some(event_id) = &event_anchor {
        let present = {
            let guard = store.lock().expect("memory store poisoned");
            EventRepository::get(&*guard, event_id)
                .map_err(|e| crate::tasks::store_fault(e, correlation_id))?
                .is_some()
        };
        if !present {
            return Err(ApiError::of_status(
                athena_core::protocol::ErrorClass::ValidationError,
                HttpStatus::NOT_FOUND,
                "MEMORY_ANCHOR_NOT_FOUND",
                "the anchored Event does not exist",
                json!({ "event_id": event_id.as_str() }),
                correlation_id.to_string(),
            ));
        }
    }
    // The scope: the deterministic `ProjectId` derivation (R-DAT-02) over the workspace.
    let scope = long_term::project_scope_of(workspace);
    // The hygiene pass belongs to the writer (R-POL-13): mask before persistence.
    let hygiene = mask_text(content);
    let provenance = athena_types::MemoryOrigin {
        task_id: task_anchor.clone(),
        event_id: event_anchor.clone(),
    };
    let requester = long_term::Requester::HumanInstruction {
        identity: caller_identity.to_string(),
    };
    let staged = long_term::review(
        &requester,
        &hygiene,
        &scope,
        config.memory.long_term.enabled,
        provenance,
    )
    .map_err(|fault| match fault {
        long_term::WriteFault::Implicit(_) => crate::tasks::store_fault(
            athena_core::persistence_internal(fault.to_string()),
            correlation_id,
        ),
        long_term::WriteFault::ProvenanceMissing => ApiError::of_status(
            athena_core::protocol::ErrorClass::ValidationError,
            HttpStatus::BAD_REQUEST,
            "MEMORY_ANCHOR_REQUIRED",
            "a Long record carries provenance (task and event anchors, ATH-MEM-005)",
            json!({}),
            correlation_id.to_string(),
        ),
        long_term::WriteFault::TierDisabled => ApiError::of_status(
            athena_core::protocol::ErrorClass::PolicyError,
            HttpStatus::FORBIDDEN,
            "MEMORY_LONG_TERM_DISABLED",
            "memory.long_term.enabled is false: the tier refuses writes",
            json!({}),
            correlation_id.to_string(),
        ),
    })?;
    {
        let mut guard = store.lock().expect("memory store poisoned");
        long_term::write_with_retention(&mut guard, &staged, config.memory.long_term.max_records)
            .map_err(|fault| match fault {
            long_term::RetentionFault::Store(error) => {
                crate::tasks::store_fault(error, correlation_id)
            }
            long_term::RetentionFault::CeilingReached {
                used,
                ceiling,
                scope,
            } => ApiError::of_status(
                athena_core::protocol::ErrorClass::PolicyError,
                HttpStatus::CONFLICT,
                "MEMORY_CEILING_REACHED",
                format!(
                    "the long-term tier is at its ceiling ({used}/{ceiling} for the scope): \
                             removal is an explicit judgement, never a silent digest (ATH-MEM-002)"
                ),
                json!({ "used": used, "ceiling": ceiling, "scope": scope }),
                correlation_id.to_string(),
            ),
        })?;
        // The `MemoryRecordCreated` Event materializes behind the write (D-1): the facts
        // stay reference-only (`ATH-EVENT-007` — the content never rides an Event).
        let (memory_id, _, scope_wire, created_at) = long_term::created_event_facts(&staged);
        let event = NewEvent {
            event_id: athena_types::EventId::new(),
            schema_version: 1,
            event_type: "MemoryRecordCreated".into(),
            occurred_at: now.to_string(),
            task_id: task_anchor.clone(),
            plan_step_id: None,
            agent_id: None,
            instance_id: None,
            skill_id: None,
            correlation_id: correlation_id.to_string(),
            causation_id: None,
            actor: "human".into(),
            payload_json: json!({
                "memory_id": memory_id,
                "scope": scope_wire,
                "created_at": created_at,
            })
            .to_string(),
        };
        EventRepository::append(&mut *guard, &event)
            .map_err(|e| crate::tasks::store_fault(e, correlation_id))?;
    }
    Ok((
        HttpStatus::CREATED,
        Json(json!({
            "memory_id": staged.id.as_str(),
            "tier": tier_wire(staged.tier),
            "scope": staged.scope_id,
            "created_at": staged.created_at,
        })),
    )
        .into_response())
}

fn by_tier(
    store: &athena_core::AthenaStore,
    tier: MemoryTier,
) -> Result<Vec<athena_types::MemoryRecord>, ApiError> {
    MemoryRepository::list_by_tier(store, tier).map_err(|e| crate::tasks::store_fault(e, ""))
}

fn filter_scope(
    store: &athena_core::AthenaStore,
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
