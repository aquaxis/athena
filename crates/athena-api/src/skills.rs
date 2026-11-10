//! The Skill API (X-32's §22.7 surface; spec §12.4 / §22.7 / §29.2; `ATH-API-014`/
//! `015`/`026`, `ATH-SKILL-009`/`020`, `ATH-STATE-025`/`026`):
//!
//! - **The §22.7 wire form** — per-version faces only. Reads: `GET /skills` (the
//!   catalog rows), `GET /skills/{id}` (every version of one id), `GET
//!   /skills/{id}/versions/{version}` (one pinned row) and the `search` face. Writes
//!   (all `admin`, `ATH-API-015`): registration rides `POST /skills`; the lifecycle
//!   verbs ride `POST|DELETE /skills/{id}/versions/{version}[/<verb>]`. A version-
//!   omitting operation is not offered (`ATH-API-026` — the early header-carried
//!   `/decision` twin carried its pin outside the path the spec fixes, and its CLI
//!   caller sent a body the handler never read, so X-32 retired it together with the
//!   CLI's per-version verbs).
//! - **The §17.3 table is the only transition authority**: enable accepts
//!   `AVAILABLE`/`DISABLED`, disable accepts `ENABLED`, update accepts
//!   `ENABLED`/`DISABLED`/`AVAILABLE`, validate (re-validation) accepts `FAILED`,
//!   removal accepts `AVAILABLE`/`ENABLED`/`DISABLED`/`FAILED`; everything else is
//!   409 `SKILL_STATE_TRANSITION_ILLEGAL`. Each leg appends its §17.3 Event
//!   (`SkillEnabled` / `SkillDisabled` / `SkillUpdating` / `SkillValidating` /
//!   `SkillRemoved`) in the store's transaction window (D-1) with `actor: "human"` —
//!   Core Agent never drives a lifecycle transition (`ATH-STATE-025`).
//! - **Register is a fresh-pin act, not a transition** (`ATH-STATE-026`'s pin, §17.3):
//!   a body whose `(id, version)` the Catalog already holds refuses 409
//!   `SKILL_ID_VERSION_CONFLICT` — an upsert would fabricate a §17.3 edge the
//!   Admin/API actor never owns (e.g. `UPDATING → AVAILABLE` belongs to the Skill
//!   Manager's `SkillUpdated` alone). The id `search` refuses 400
//!   `SKILL_ID_RESERVED`: it would shadow the static `GET /skills/search` route in
//!   the same `{skill_id}` segment. The search face percent-decodes its query value
//!   once (`%XX`; `+` stays a literal plus) so encoded capabilities reach the
//!   Catalog's exact-text comparison — the CLI encodes its side symmetrically.
//! - **Request vs completion** — enable / disable / removal complete at the API (200);
//!   update and validate only *record the request* (202): the acquisition and
//!   validation legs belong to the Skill Manager / Skill Validator (§12.5), so the
//!   answer carries the row's real in-flight state (`UPDATING` / `VALIDATING`) and
//!   never a fabricated completion — the same request/completion split the instance
//!   stop face rides (`ATH-API-024`'s `Stopping` + 202 precedent).
//! - **No `ENABLED` before validation** (`ATH-SKILL-009`, `ATH-API-014`): a
//!   registration payload whose state is already `ENABLED` refuses.

use athena_core::protocol::ErrorClass;
use athena_core::{EventRepository, NewEvent, SkillRepository};
use athena_types::{EventId, Skill, SkillId, SkillState};
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

use crate::errors::{ApiError, correlation_of};
use crate::routes::{AppState, now_text};

/// The wall-clock spread for the correlation mint (the mint's own counter guarantees
/// the uniqueness; the seed only spreads the starts).
fn wall_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(u64::MAX)
}

/// One §22.2 refusal, class-carried (R-API-07): the skills surface answers the same
/// envelope the other groups answer.
fn refusal(
    headers: &HeaderMap,
    class: ErrorClass,
    status: StatusCode,
    code: &str,
    message: impl Into<String>,
    detail: Value,
) -> Response {
    ApiError::of_status(
        class,
        status,
        code,
        message,
        detail,
        correlation_of(headers, wall_seed()),
    )
    .into_response()
}

fn store_unavailable(headers: &HeaderMap) -> Response {
    refusal(
        headers,
        ErrorClass::InternalError,
        StatusCode::SERVICE_UNAVAILABLE,
        "CORE_STORE_UNAVAILABLE",
        "the Core store is not open to the API",
        json!({}),
    )
}

fn store_fault(headers: &HeaderMap, error: impl std::fmt::Display) -> Response {
    refusal(
        headers,
        ErrorClass::InternalError,
        StatusCode::INTERNAL_SERVER_ERROR,
        "SKILL_STORE_FAULT",
        "the skill store refused the operation",
        json!(error.to_string()),
    )
}

fn open_store(state: &AppState) -> Option<Arc<Mutex<athena_core::AthenaStore>>> {
    state.rows.clone()
}

/// `POST /api/v1/skills` — register a skill version. Mandatory `(id, version)`
/// (`ATH-SKILL-020`); a payload state of `ENABLED` refuses (`ATH-SKILL-009` /
/// `ATH-API-014`).
pub async fn register_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Response {
    // Two body shapes are understood: the manifest-carrying wrapper the CLI's `skill
    // install` sends (`{"manifest_yaml": …}` — the manifest pins `(id, version)`
    // itself, `ATH-SKILL-020` / `ATH-SKILL-003`, so the parse precedes any pin check)
    // and a full Skill-entity body (other clients — the pin rides the top-level
    // fields, and its absence is the `SKILL_ID_VERSION_REQUIRED` refusal).
    let skill: Skill = match payload.get("manifest_yaml").and_then(Value::as_str) {
        Some(yaml_text) => match athena_core::skills_bridge::manifest_to_record(yaml_text) {
            Ok(skill) => skill,
            Err(detail) => {
                return refusal(
                    &headers,
                    ErrorClass::ValidationError,
                    StatusCode::BAD_REQUEST,
                    "MANIFEST_INVALID",
                    "the manifest YAML does not assemble into a Skill record",
                    json!(detail.to_string()),
                );
            }
        },
        None => {
            let (Some(id), Some(version)) =
                (field_of(&payload, "id"), field_of(&payload, "version"))
            else {
                return refusal(
                    &headers,
                    ErrorClass::ValidationError,
                    StatusCode::BAD_REQUEST,
                    "SKILL_ID_VERSION_REQUIRED",
                    "the (id, version) pin is mandatory (ATH-SKILL-020 / ATH-API-026)",
                    json!({"fields": ["id", "version"]}),
                );
            };
            let _ = (&id, &version);
            if payload_state_is(&payload, SkillState::Enabled) {
                return refusal(
                    &headers,
                    ErrorClass::ValidationError,
                    StatusCode::CONFLICT,
                    "SKILL_ENABLED_REQUIRES_VALIDATION",
                    "a registration may not declare ENABLED before validation",
                    json!({"rule": "ATH-SKILL-009 / ATH-API-014"}),
                );
            }
            match serde_json::from_value(payload.clone()) {
                Ok(skill) => skill,
                Err(detail) => {
                    return refusal(
                        &headers,
                        ErrorClass::ValidationError,
                        StatusCode::BAD_REQUEST,
                        "SKILL_PAYLOAD_INVALID",
                        "the body is not a Skill entity",
                        json!(detail.to_string()),
                    );
                }
            }
        }
    };
    let Some(store) = open_store(&state) else {
        return store_unavailable(&headers);
    };
    // The id `search` is reserved: `GET /skills/search` is a static route in the same
    // segment `GET /skills/{skill_id}` matches, and matchit prefers the static branch —
    // a registered `search` row would be unreachable through its own read face.
    if skill.id.as_str() == "search" {
        return refusal(
            &headers,
            ErrorClass::ValidationError,
            StatusCode::BAD_REQUEST,
            "SKILL_ID_RESERVED",
            "the id `search` is reserved (it would shadow the static /skills/search route)",
            json!({"id": "search"}),
        );
    }
    // Register is a fresh-pin act, not a §17.3 transition (`ATH-STATE-026`'s pin): a
    // pin the Catalog already holds refuses — an upsert here would fabricate an edge
    // the Admin/API actor does not own (e.g. re-registering over an `UPDATING` row
    // would answer the Skill Manager's `SkillUpdated` completion in its place).
    {
        let guard = store.lock().expect("store lock");
        if matches!(
            SkillRepository::get(&*guard, &skill.id, &skill.version),
            Ok(Some(_))
        ) {
            return refusal(
                &headers,
                ErrorClass::ValidationError,
                StatusCode::CONFLICT,
                "SKILL_ID_VERSION_CONFLICT",
                "the (id, version) pin is already registered (register is a fresh-pin act, not an update)",
                json!({"id": skill.id.as_str(), "version": skill.version}),
            );
        }
    }
    let now = now_text();
    let mut record = skill;
    record.state = SkillState::Available; // registration lands AVAILABLE (§17.3 pair)
    record.registered_at = now.clone();
    record.updated_at = now;
    if let Err(error) = {
        let mut guard = store.lock().expect("store lock");
        SkillRepository::upsert(&mut *guard, &record)
    } {
        return store_fault(&headers, error);
    }
    (
        StatusCode::CREATED,
        Json(json!({
            "id": record.id.as_str().to_string(),
            "version": record.version,
            "state": record.state.as_str().to_string()
        })),
    )
        .into_response()
}

/// `GET /api/v1/skills` — the Catalog rows (the states merge; the viewer face).
pub async fn list_skills(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(store) = open_store(&state) else {
        return store_unavailable(&headers);
    };
    let mut rows: Vec<Skill> = Vec::new();
    {
        let guard = store.lock().expect("store lock");
        for skill_state in SkillState::ALL {
            match SkillRepository::list_by_state(&*guard, *skill_state) {
                Ok(mut one) => rows.append(&mut one),
                Err(error) => return store_fault(&headers, error),
            }
        }
    }
    rows.sort_by(|a, b| {
        a.id.as_str()
            .cmp(b.id.as_str())
            .then(a.version.cmp(&b.version))
    });
    Json(json!({"skills": rows})).into_response()
}

/// `GET /api/v1/skills/{id}` — every version of one id (§22.7 "版一覧を含む").
pub async fn list_skill_versions(
    axum::extract::Path(skill_id): axum::extract::Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let Some(store) = open_store(&state) else {
        return store_unavailable(&headers);
    };
    let guard = store.lock().expect("store lock");
    match SkillRepository::list_versions(&*guard, &SkillId::new(&skill_id)) {
        Ok(rows) => Json(json!({"id": skill_id, "versions": rows})).into_response(),
        Err(error) => store_fault(&headers, error),
    }
}

/// `GET /api/v1/skills/{id}/versions/{version}` — one pinned row; 404 when absent.
pub async fn read_skill_version(
    axum::extract::Path((skill_id, version)): axum::extract::Path<(String, String)>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let Some(store) = open_store(&state) else {
        return store_unavailable(&headers);
    };
    let guard = store.lock().expect("store lock");
    match SkillRepository::get(&*guard, &SkillId::new(&skill_id), &version) {
        Ok(Some(row)) => Json(row).into_response(),
        Ok(None) => pinned_absent(&headers, &skill_id, &version),
        Err(error) => store_fault(&headers, error),
    }
}

fn pinned_absent(headers: &HeaderMap, skill_id: &str, version: &str) -> Response {
    refusal(
        headers,
        ErrorClass::ValidationError,
        StatusCode::NOT_FOUND,
        "SKILL_VERSION_ABSENT",
        "no skill row answers the (id, version) pin",
        json!({"id": skill_id, "version": version}),
    )
}

/// Percent-decode one query value (`%XX` byte escapes; malformed escapes and non-UTF-8
/// bytes degrade via [`String::from_utf8_lossy`], the read face never refuses on them).
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'%' if at + 2 < bytes.len() => {
                let high = (bytes[at + 1] as char).to_digit(16);
                let low = (bytes[at + 2] as char).to_digit(16);
                if let (Some(high), Some(low)) = (high, low) {
                    out.push(u8::try_from(high * 16 + low).unwrap_or(b'%'));
                    at += 3;
                } else {
                    out.push(b'%');
                    at += 1;
                }
            }
            byte => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `GET /api/v1/skills/search?capability=…` — the Catalog search (`ATH-SKILL-006`),
/// over the ENABLED candidate set only (`ATH-STATE-008`).
pub async fn search_skills(
    State(state): State<AppState>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
    headers: HeaderMap,
) -> Response {
    let query: Option<String> = raw;
    // The capability rides the wire percent-encoded (`capability=%E5%9B%9E…`, `+` for
    // the raw space): the Catalog's search is *exact text* (`ATH-SKILL-006`), so the
    // raw value is decoded once here. `+` is left as a literal plus (the query's
    // `application/x-www-form-urlencoded` cross meaning is not the §22.7 wire form).
    let capability = query
        .as_ref()
        .and_then(|query| {
            query.split('&').find_map(|pair| {
                let (name, value) = pair.split_once('=')?;
                name.eq_ignore_ascii_case("capability")
                    .then(|| percent_decode(value))
            })
        })
        .unwrap_or_default();
    if capability.trim().is_empty() {
        return refusal(
            &headers,
            ErrorClass::ValidationError,
            StatusCode::BAD_REQUEST,
            "CAPABILITY_REQUIRED",
            "the capability parameter is required",
            json!({"param": "capability"}),
        );
    }
    let Some(store) = open_store(&state) else {
        return store_unavailable(&headers);
    };
    let guard = store.lock().expect("store lock");
    match SkillRepository::search_by_capability(&*guard, &capability) {
        Ok(rows) => Json(json!({"capability": capability, "candidates": rows})).into_response(),
        Err(error) => store_fault(&headers, error),
    }
}

// --- §22.7 write faces: the §17.3 table is the only transition authority ---------------------

/// The §17.3 edge walk for one pinned `(id, version)`: load the row, check the accepted
/// predecessor set, apply the `to` state, append the edge's Event — the row write and
/// the Event ride one store lock (D-1). `status` names the completion face: 200 when
/// the API itself completes the leg (enable / disable / removal), 202 when the leg
/// only *records the request* and a Runtime actor owns the completion (update → the
/// Skill Manager, validate → the Skill Validator; §12.5). The argument list mirrors
/// one §17.3 row (accepted states, target, event, operation, completion status), so
/// each call site reads as the table row it walks.
#[allow(clippy::too_many_arguments)]
fn transition_pinned(
    store: Option<Arc<Mutex<athena_core::AthenaStore>>>,
    headers: &HeaderMap,
    skill_id: &str,
    version: &str,
    accepted: &[SkillState],
    to: SkillState,
    event_type: &str,
    operation: &str,
    status: StatusCode,
) -> Response {
    let Some(store) = store else {
        return store_unavailable(headers);
    };
    let mut guard = store.lock().expect("store lock");
    let row = match SkillRepository::get(&*guard, &SkillId::new(skill_id), version) {
        Ok(Some(row)) => row,
        Ok(None) => return pinned_absent(headers, skill_id, version),
        Err(error) => return store_fault(headers, error),
    };
    if !accepted.contains(&row.state) {
        return refusal(
            headers,
            ErrorClass::ValidationError,
            StatusCode::CONFLICT,
            "SKILL_STATE_TRANSITION_ILLEGAL",
            format!(
                "state {} does not offer the {operation} edge",
                row.state.as_str()
            ),
            json!({
                "from": row.state.as_str(),
                "requested": to.as_str(),
                "table": "spec section 17.3"
            }),
        );
    }
    let mut next = row.clone();
    next.state = to;
    next.updated_at = now_text();
    let event = NewEvent {
        event_id: EventId::new(),
        schema_version: 1,
        event_type: event_type.to_string(),
        occurred_at: now_text(),
        task_id: None,
        plan_step_id: None,
        agent_id: None,
        instance_id: None,
        skill_id: Some(row.id.clone()),
        correlation_id: correlation_of(headers, wall_seed()),
        causation_id: None,
        actor: "human".into(),
        payload_json: json!({
            "id": row.id.as_str(),
            "version": row.version,
            "from": row.state.as_str(),
            "to": to.as_str(),
            "operation": operation
        })
        .to_string(),
    };
    if let Err(error) = SkillRepository::upsert(&mut *guard, &next) {
        return store_fault(headers, error);
    }
    if let Err(error) = EventRepository::append(&mut *guard, &event) {
        return store_fault(headers, error);
    }
    (status, Json(state_body(&next))).into_response()
}

/// The state face every lifecycle write answers.
fn state_body(row: &Skill) -> Value {
    json!({
        "id": row.id.as_str(),
        "version": row.version,
        "state": row.state.as_str()
    })
}

/// `POST /api/v1/skills/{id}/versions/{version}/enable` (§22.7, admin): the §17.3
/// edges AVAILABLE → ENABLED and DISABLED → ENABLED (`SkillEnabled`).
pub async fn enable_skill_version(
    axum::extract::Path((skill_id, version)): axum::extract::Path<(String, String)>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    transition_pinned(
        open_store(&state),
        &headers,
        &skill_id,
        &version,
        &[SkillState::Available, SkillState::Disabled],
        SkillState::Enabled,
        "SkillEnabled",
        "enable",
        StatusCode::OK,
    )
}

/// `POST …/disable` (§22.7, admin): the §17.3 edge ENABLED → DISABLED
/// (`SkillDisabled`).
pub async fn disable_skill_version(
    axum::extract::Path((skill_id, version)): axum::extract::Path<(String, String)>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    transition_pinned(
        open_store(&state),
        &headers,
        &skill_id,
        &version,
        &[SkillState::Enabled],
        SkillState::Disabled,
        "SkillDisabled",
        "disable",
        StatusCode::OK,
    )
}

/// `POST …/update` (§22.7, admin): the §17.3 edges ENABLED / DISABLED / AVAILABLE →
/// UPDATING (`SkillUpdating`), answered 202 — the API records the update request; the
/// acquisition and validation legs ride the Skill Manager (§12.5).
pub async fn update_skill_version(
    axum::extract::Path((skill_id, version)): axum::extract::Path<(String, String)>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    transition_pinned(
        open_store(&state),
        &headers,
        &skill_id,
        &version,
        &[
            SkillState::Enabled,
            SkillState::Disabled,
            SkillState::Available,
        ],
        SkillState::Updating,
        "SkillUpdating",
        "update",
        StatusCode::ACCEPTED,
    )
}

/// `POST …/validate` (§22.7, admin): the §17.3 edge FAILED → VALIDATING (再検証,
/// `SkillValidating`), answered 202 — the API records the re-validation request; the
/// validation run rides the Skill Validator (§12.5).
pub async fn validate_skill_version(
    axum::extract::Path((skill_id, version)): axum::extract::Path<(String, String)>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    transition_pinned(
        open_store(&state),
        &headers,
        &skill_id,
        &version,
        &[SkillState::Failed],
        SkillState::Validating,
        "SkillValidating",
        "validate",
        StatusCode::ACCEPTED,
    )
}

/// `DELETE /api/v1/skills/{id}/versions/{version}` (§22.7, admin): the §17.3 removal
/// edges — AVAILABLE / ENABLED / DISABLED / FAILED → REMOVED (`SkillRemoved`).
/// `REMOVED` is the terminal state; the row remains as the lifecycle record.
pub async fn remove_skill_version(
    axum::extract::Path((skill_id, version)): axum::extract::Path<(String, String)>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    transition_pinned(
        open_store(&state),
        &headers,
        &skill_id,
        &version,
        &[
            SkillState::Available,
            SkillState::Enabled,
            SkillState::Disabled,
            SkillState::Failed,
        ],
        SkillState::Removed,
        "SkillRemoved",
        "remove",
        StatusCode::OK,
    )
}

fn field_of(payload: &Value, field: &str) -> Option<String> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn payload_state_is(payload: &Value, state: SkillState) -> bool {
    payload
        .get("state")
        .and_then(Value::as_str)
        .is_some_and(|raw| raw.eq_ignore_ascii_case(state.as_str()))
}

#[cfg(test)]
#[path = "skills/skills_api_tests.rs"]
mod skills_api_tests;
