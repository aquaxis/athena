//! The Skill API (P5-13; spec §12.4 / §29.2 / `ATH-API-014` / `ATH-SKILL-009`):
//!
//! - **Mandatory `(id, version)`** — every mutating call names the exact version
//!   (`ATH-SKILL-020`'s pin face); a body missing either is a `400`.
//! - **The `admin` role** — registration / enable / disable are `admin` operations
//!   (`ATH-API-015`, `ATH-STATE-025`; the routes crate's middleware layers already
//!   enforce this — these handlers ride the `Role::Admin` router).
//! - **No `ENABLED` before validation** (`ATH-SKILL-009`, `ATH-API-014`): a
//!   registration payload whose state is already `ENABLED` refuses; the enable
//!   endpoint refuses rows not in `AVAILABLE` / `DISABLED` — the only predecessors
//!   named in the §17.3 table.

use athena_core::SkillRepository;
use athena_types::{Skill, SkillId, SkillState};
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

use crate::routes::{AppState, now_text};

fn fault(status: StatusCode, code: &str, detail: Value) -> Response {
    (
        status,
        Json(json!({"error": {"code": code, "detail": detail}})),
    )
        .into_response()
}

fn open_store(state: &AppState) -> Option<Arc<Mutex<athena_core::SqliteStore>>> {
    state.rows.clone()
}

/// `POST /api/v1/skills` — register a skill version. Mandatory `(id, version)`;
/// a payload state of `ENABLED` refuses (`ATH-SKILL-009` / `ATH-API-014`).
pub async fn register_skill(State(state): State<AppState>, Json(payload): Json<Value>) -> Response {
    let (Some(id), Some(version)) = (field_of(&payload, "id"), field_of(&payload, "version"))
    else {
        return fault(
            StatusCode::BAD_REQUEST,
            "SKILL_ID_VERSION_REQUIRED",
            json!({"fields": ["id", "version"]}),
        );
    };
    let _ = (&id, &version);
    if payload_state_is(&payload, SkillState::Enabled) {
        return fault(
            StatusCode::CONFLICT,
            "SKILL_ENABLED_REQUIRES_VALIDATION",
            json!({"rule": "ATH-SKILL-009 / ATH-API-014"}),
        );
    }
    // Two body shapes are understood: the manifest-carrying wrapper the CLI sends
    // (`{"manifest_yaml": …}`) and a full Skill-entity body (other clients).
    let skill: Skill = match payload.get("manifest_yaml").and_then(Value::as_str) {
        Some(yaml_text) => match athena_core::skills_bridge::manifest_to_record(yaml_text) {
            Ok(skill) => skill,
            Err(detail) => {
                return fault(
                    StatusCode::BAD_REQUEST,
                    "MANIFEST_INVALID",
                    json!(detail.to_string()),
                );
            }
        },
        None => match serde_json::from_value(payload.clone()) {
            Ok(skill) => skill,
            Err(detail) => {
                return fault(
                    StatusCode::BAD_REQUEST,
                    "SKILL_PAYLOAD_INVALID",
                    json!(detail.to_string()),
                );
            }
        },
    };
    let Some(store) = open_store(&state) else {
        return fault(
            StatusCode::SERVICE_UNAVAILABLE,
            "CORE_STORE_UNAVAILABLE",
            json!({}),
        );
    };
    let now = now_text();
    let mut record = skill;
    record.state = SkillState::Available; // registration lands AVAILABLE (§17.3 pair)
    record.registered_at = now.clone();
    record.updated_at = now;
    let mut guard = store.lock().expect("store lock");
    match SkillRepository::upsert(&mut *guard, &record) {
        Ok(()) => (
            StatusCode::CREATED,
            Json(json!({
                "id": record.id.as_str().to_string(),
                "version": record.version,
                "state": record.state.as_str().to_string()
            })),
        )
            .into_response(),
        Err(error) => fault(
            StatusCode::INTERNAL_SERVER_ERROR,
            "SKILL_STORE_FAULT",
            json!(error.to_string()),
        ),
    }
}

/// `GET /api/v1/skills` — the Catalog rows (the states merge; the admin face).
pub async fn list_skills(State(state): State<AppState>) -> Response {
    let Some(store) = open_store(&state) else {
        return fault(
            StatusCode::SERVICE_UNAVAILABLE,
            "CORE_STORE_UNAVAILABLE",
            json!({}),
        );
    };
    let guard = store.lock().expect("store lock");
    let mut rows: Vec<Skill> = Vec::new();
    for state in SkillState::ALL {
        match SkillRepository::list_by_state(&*guard, *state) {
            Ok(mut one) => rows.append(&mut one),
            Err(error) => {
                return fault(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "SKILL_STORE_FAULT",
                    json!(error.to_string()),
                );
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

/// `GET /api/v1/skills/{id}` — every version of one id.
pub async fn list_skill_versions(
    axum::extract::Path(skill_id): axum::extract::Path<String>,
    State(state): State<AppState>,
) -> Response {
    let Some(store) = open_store(&state) else {
        return fault(
            StatusCode::SERVICE_UNAVAILABLE,
            "CORE_STORE_UNAVAILABLE",
            json!({}),
        );
    };
    let guard = store.lock().expect("store lock");
    match SkillRepository::list_versions(&*guard, &SkillId::new(&skill_id)) {
        Ok(rows) => Json(json!({"id": skill_id, "versions": rows})).into_response(),
        Err(error) => fault(
            StatusCode::INTERNAL_SERVER_ERROR,
            "SKILL_STORE_FAULT",
            json!(error.to_string()),
        ),
    }
}

/// `GET /api/v1/skills/search?capability=…` — the Catalog search (`ATH-SKILL-006`),
/// over the ENABLED candidate set only (`ATH-STATE-008`).
pub async fn search_skills(
    State(state): State<AppState>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> Response {
    let query: Option<String> = raw;
    let capability = query
        .as_ref()
        .and_then(|query| {
            query.split('&').find_map(|pair| {
                let (name, value) = pair.split_once('=')?;
                name.eq_ignore_ascii_case("capability")
                    .then(|| value.to_string())
            })
        })
        .unwrap_or_default();
    if capability.trim().is_empty() {
        return fault(
            StatusCode::BAD_REQUEST,
            "CAPABILITY_REQUIRED",
            json!({"param": "capability"}),
        );
    }
    let Some(store) = open_store(&state) else {
        return fault(
            StatusCode::SERVICE_UNAVAILABLE,
            "CORE_STORE_UNAVAILABLE",
            json!({}),
        );
    };
    let guard = store.lock().expect("store lock");
    match SkillRepository::search_by_capability(&*guard, &capability) {
        Ok(rows) => Json(json!({"capability": capability, "candidates": rows})).into_response(),
        Err(error) => fault(
            StatusCode::INTERNAL_SERVER_ERROR,
            "SKILL_STORE_FAULT",
            json!(error.to_string()),
        ),
    }
}

/// The enable / disable twin over the `x-athena-skill-decision` header. Both are
/// admin operations; the enable refuses rows not `AVAILABLE` / `DISABLED`
/// (`ATH-SKILL-009` no-ENABLED-before-validation shape, §17.3 table).
pub async fn enable_disable_skill(
    axum::extract::Path(skill_id): axum::extract::Path<String>,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Response {
    use athena_types::{ApprovalState as _NoUse, SkillState as _S};
    let _ = &_NoUse::Requested;
    let enable = headers
        .get("x-athena-skill-decision")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.eq_ignore_ascii_case("enable"))
        .unwrap_or(true);
    let version = headers
        .get("x-athena-skill-version")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    if version.trim().is_empty() {
        // Mandatory (id, version) — the pin never floats.
        return fault(
            StatusCode::BAD_REQUEST,
            "SKILL_ID_VERSION_REQUIRED",
            json!({"fields": ["version"]}),
        );
    }
    let Some(store) = open_store(&state) else {
        return fault(
            StatusCode::SERVICE_UNAVAILABLE,
            "CORE_STORE_UNAVAILABLE",
            json!({}),
        );
    };
    let mut guard = store.lock().expect("store lock");
    let row = match SkillRepository::get(&*guard, &SkillId::new(&skill_id), &version) {
        Ok(Some(row)) => row,
        Ok(None) => {
            return fault(
                StatusCode::NOT_FOUND,
                "SKILL_VERSION_ABSENT",
                json!({"id": skill_id, "version": version}),
            );
        }
        Err(error) => {
            return fault(
                StatusCode::INTERNAL_SERVER_ERROR,
                "SKILL_STORE_FAULT",
                json!(error.to_string()),
            );
        }
    };
    // The §17.3 row check — no ENABLED before validation:
    let legal = if enable {
        matches!(row.state, _S::Available | _S::Disabled)
    } else {
        matches!(row.state, _S::Enabled)
    };
    if !legal {
        return fault(
            StatusCode::CONFLICT,
            "SKILL_STATE_TRANSITION_ILLEGAL",
            json!({
                "from": row.state.as_str(),
                "operation": if enable { "enable" } else { "disable" },
                "table": "spec section 17.3"
            }),
        );
    }
    let mut next = row.clone();
    next.state = if enable { _S::Enabled } else { _S::Disabled };
    next.updated_at = now_text();
    match SkillRepository::upsert(&mut *guard, &next) {
        Ok(()) => Json(json!({
            "id": skill_id,
            "version": version,
            "state": next.state.as_str().to_string()
        }))
        .into_response(),
        Err(error) => fault(
            StatusCode::INTERNAL_SERVER_ERROR,
            "SKILL_STORE_FAULT",
            json!(error.to_string()),
        ),
    }
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
