//! The `/api/v1` HTTP surface (spec §22).
//!
//! Route groups sit under one `/api/v1` scope; each is gated by the role its
//! specification row demands (ATH-SEC-004, R-API-08) — the flavoured layer sits
//! `route_layer`-deep so the check rides *after* the router matches, and every refusal
//! answers the §22.2 envelope.
//!
//! Authentication failures/secrets that line up with ATH-SEC-006 are recorded by the
//! caller (the binary's audit): the middleware itself stays stateless here, and every
//! refusal carries the classification (401 unauthenticated, 403 under-privileged) which
//! spec §22.2's HTTP mapping prescribes.
//!
//! `GET /healthz` rides the router *outside* any layer and is the single unauthenticated
//! endpoint (R-API-03): it answers a literal `ok`, exposing no internal state.
//!
//! P1-37 is the skeleton: the handler bodies stubbed to a stable response shape; specific
//! endpoints (the Task / Plan / Event groups) land with `P1-40` and the tests there.

use crate::auth::{AuthMode, TokenRegistry};
use crate::roles::Role;
use axum::{
    Json, Router,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::from_fn_with_state,
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// The shared route state: the registry plus the mode.
#[derive(Clone)]
pub struct AppState {
    /// The token registry under var/.
    pub registry: Arc<TokenRegistry>,
    /// Which auth mode is active.
    pub mode: AuthMode,
    /// The bind address, exposed only to the *caller* bind check (never to the routes).
    pub host: String,
    /// The open store the Task API reads and writes (the Core Host opened it at boot).
    pub rows: Option<Arc<Mutex<athena_core::AthenaStore>>>,
}

/// A call-time seed for the correlation mint (locked monotonicity is fine here: the
/// counter in `errors::correlation_of` guarantees uniqueness).
fn request_wall_clock_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(u64::MAX)
}

/// Build the router: `healthz` anonymous; `/api/v1` gated by per-group role layers.
///
/// The layers stack per group: the routes come first and the layer applies to exactly
/// those, so the role hierarchy `viewer ⊂ operator ⊂ admin` (ATH-SEC-003) is the routing
/// tree's own structure.
#[must_use = "a router that is never served answers nothing"]
pub fn build_router(state: AppState) -> Router {
    // `healthz` first, no layer.
    let health = Router::new().route("/healthz", get(healthz));

    // The role-gated scope; per group the routes come first and the role layer applies to
    // exactly those (ATH-SEC-004).
    let viewer: Router<AppState> = Router::new()
        .merge(
            Router::new()
                .route(
                    "/api/v1/skills",
                    axum::routing::get(crate::skills::list_skills),
                )
                .route(
                    "/api/v1/skills/search",
                    axum::routing::get(crate::skills::search_skills),
                )
                .route(
                    "/api/v1/conversations/{conversation_id}/messages",
                    axum::routing::get(crate::conversations::list_utterances),
                ),
        )
        .route("/api/v1/status", get(status_stub))
        .route("/api/v1/tasks", get(list_task_rows))
        .route("/api/v1/tasks/{task_id}", get(read_task_row))
        .route("/api/v1/tasks/{task_id}/events", get(task_events_row))
        .route("/api/v1/metrics", get(metrics_row))
        .route("/api/v1/events/stream", get(events_stream_row));
    let operator: Router<AppState> = Router::new()
        .route(
            "/api/v1/conversations",
            axum::routing::post(crate::conversations::open_conversation),
        )
        .route(
            "/api/v1/conversations/{conversation_id}/messages",
            axum::routing::post(crate::conversations::post_utterance),
        )
        .route("/api/v1/memory", get(list_memory_rows))
        .route(
            "/api/v1/memory/{memory_id}",
            axum::routing::delete(delete_memory_row),
        )
        .route(
            "/api/v1/tasks/{task_id}/answer",
            axum::routing::post(answer_task_row),
        )
        .route("/api/v1/tasks", axum::routing::post(create_task_row))
        .route(
            "/api/v1/tasks/{task_id}/run",
            axum::routing::post(run_task_row),
        )
        .route(
            "/api/v1/tasks/{task_id}/cancel",
            axum::routing::post(cancel_task_row),
        )
        .route("/api/v1/approvals", get(list_approvals_row))
        .route("/api/v1/approvals/{id}", get(read_approval_row))
        .route(
            "/api/v1/approvals/{id}/approve",
            axum::routing::post(approve_or_reject_row),
        )
        .route(
            "/api/v1/approvals/{id}/reject",
            axum::routing::post(approve_or_reject_row),
        );
    let admin: Router<AppState> = Router::new()
        .route(
            "/api/v1/skills",
            axum::routing::post(crate::skills::register_skill),
        )
        .route(
            "/api/v1/skills/{skill_id}/decision",
            axum::routing::post(crate::skills::enable_disable_skill),
        );
    health
        .merge(endpoint_layer(&state, Role::Viewer, viewer))
        .merge(endpoint_layer(&state, Role::Operator, operator))
        .merge(endpoint_layer(&state, Role::Admin, admin))
        .with_state(state.clone())
}

/// A layer requiring one role; the token resolves before the handler runs
/// (ATH-SEC-004: the token's role against the endpoint's requirement).
fn endpoint_layer(state: &AppState, required: Role, routes: Router<AppState>) -> Router<AppState> {
    use axum::{middleware::Next, response::Response};
    async fn enforce(
        State(state): State<AppState>,
        required: Role,
        request: Request,
        next: Next,
    ) -> Response {
        let correlation =
            crate::errors::correlation_of(request.headers(), request_wall_clock_seed());
        // `auth.mode: none` skips the token check and carries `admin`: the mode exists
        // for a single-user loopback dev host (ATH-CFG-006) — the bind re-check confined
        // it to 127.0.0.1 at boot, so no request outside the trust domain can reach it.
        let caller = if state.mode == AuthMode::None {
            Ok(crate::auth::ApiCaller {
                name: "none-mode-loopback".into(),
                role: Role::Admin,
            })
        } else {
            crate::auth::from_request(request.headers(), state.registry.as_ref(), &correlation)
        };
        match caller {
            Ok(caller) if caller.satisfies(required) => {
                let mut response = next.run(request).await;
                if let Ok(value) = axum::http::HeaderValue::from_str(&correlation) {
                    response
                        .headers_mut()
                        .insert(crate::errors::CORRELATION_HEADER, value);
                }
                response
            }
            Ok(_below_role) => forbidden(required, &correlation).into_response(),
            Err(error) => error.into_response(),
        }
    }

    match required {
        Role::Viewer => routes.route_layer(from_fn_with_state(
            state.clone(),
            |state: State<AppState>, request: Request, next: Next| async move {
                enforce(state, Role::Viewer, request, next).await
            },
        )),
        Role::Operator => routes.route_layer(from_fn_with_state(
            state.clone(),
            |state: State<AppState>, request: Request, next: Next| async move {
                enforce(state, Role::Operator, request, next).await
            },
        )),
        Role::Admin => routes.route_layer(from_fn_with_state(
            state.clone(),
            |state: State<AppState>, request: Request, next: Next| async move {
                enforce(state, Role::Admin, request, next).await
            },
        )),
    }
}

/// The anonymous health check (R-API-03): a constant, no internals.
async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

fn forbidden(required: Role, correlation_id: &str) -> crate::errors::ApiError {
    crate::errors::ApiError::role_insufficient(required.as_str(), correlation_id)
}

/// The status stub: the skeleton's liveliness record before the counters land.
async fn status_stub(State(_state): State<AppState>) -> Json<Value> {
    Json(json!({
        "version": "0.1.0",
    }))
}

/// The Task store on the route state (a lazily built shared store; the Core Host
/// process opens the real database at boot — the tests use in-memory stores).
pub(crate) fn task_rows(state: &AppState) -> Option<Arc<Mutex<athena_core::AthenaStore>>> {
    state.rows.clone()
}

async fn answer_task_row(
    axum::extract::Path(task_id): axum::extract::Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Json<Value>,
) -> Response {
    let Some(rows) = task_rows(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":{"code":"CORE_STORE_UNAVAILABLE"}})),
        )
            .into_response();
    };
    let Some(task) = athena_types::TaskId::parse(task_id) else {
        return crate::errors::ApiError::of_status(
            athena_core::protocol::ErrorClass::ValidationError,
            StatusCode::NOT_FOUND,
            "TASK_NOT_FOUND",
            "malformed task id on the path",
            json!({}),
            "",
        )
        .into_response();
    };
    let correlation = crate::errors::correlation_of(&headers, request_wall_clock_seed());
    let now = crate::routes::now_text();
    let answer = payload.get("answer").and_then(Value::as_str).unwrap_or("");
    let replan = payload
        .get("requires_plan_change")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    crate::tasks::answer_task(&rows, &task, answer, replan, &now, &correlation)
        .unwrap_or_else(|error| error.into_response())
}

async fn list_memory_rows(
    axum::extract::RawQuery(query): axum::extract::RawQuery,
    State(state): State<AppState>,
) -> Response {
    let Some(rows) = task_rows(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":{"code":"CORE_STORE_UNAVAILABLE"}})),
        )
            .into_response();
    };
    let query = query.as_deref().unwrap_or("");
    let page = crate::pagination::Query::parse(query);
    let params: Vec<(String, String)> = form_urlencoded_pairs(query);
    let tier = params
        .iter()
        .find(|(key, _)| key == "tier")
        .map(|(_, value)| value.clone());
    let scope = params
        .iter()
        .find(|(key, _)| key == "scope")
        .map(|(_, value)| value.clone());
    crate::memory::list(&rows, &page, tier.as_deref(), scope.as_deref())
        .unwrap_or_else(|error| error.into_response())
}

async fn delete_memory_row(
    axum::extract::Path(memory_id): axum::extract::Path<String>,
    State(state): State<AppState>,
    request: Request,
) -> Response {
    let Some(rows) = task_rows(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":{"code":"CORE_STORE_UNAVAILABLE"}})),
        )
            .into_response();
    };
    let correlation = crate::errors::correlation_of(request.headers(), request_wall_clock_seed());
    let now = crate::routes::now_text();
    crate::memory::delete(&rows, &memory_id, &now, &correlation)
        .unwrap_or_else(|error| error.into_response())
}

fn form_urlencoded_pairs(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter_map(|pair| {
            let mut parts = pair.split('=');
            let key = parts.next()?;
            let value = parts.next().unwrap_or("");
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

async fn list_task_rows(
    axum::extract::RawQuery(query): axum::extract::RawQuery,
    State(state): State<AppState>,
) -> Response {
    let Some(rows) = task_rows(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":{"code":"CORE_STORE_UNAVAILABLE"}})),
        )
            .into_response();
    };
    let page = crate::pagination::Query::parse(query.as_deref().unwrap_or(""));
    match crate::tasks::list_tasks(&rows, &page) {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn read_task_row(
    axum::extract::Path(task): axum::extract::Path<String>,
    State(state): State<AppState>,
) -> Response {
    let Some(rows) = task_rows(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":{"code":"CORE_STORE_UNAVAILABLE"}})),
        )
            .into_response();
    };
    let Some(task_id) = athena_types::TaskId::parse(task) else {
        return crate::errors::ApiError::of_status(
            athena_core::protocol::ErrorClass::ValidationError,
            StatusCode::NOT_FOUND,
            "TASK_NOT_FOUND",
            "malformed task id on the path",
            json!({}),
            "",
        )
        .into_response();
    };
    match crate::tasks::read_task(&rows, &task_id) {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn create_task_row(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    payload: Json<Value>,
) -> Response {
    let Some(rows) = task_rows(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":{"code":"CORE_STORE_UNAVAILABLE"}})),
        )
            .into_response();
    };
    let correlation = crate::errors::correlation_of(&headers, request_wall_clock_seed());
    let now = crate::routes::now_text();
    match crate::tasks::create_task(&rows, "operator", &payload, &now, &correlation) {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn task_events_row(
    axum::extract::Path(task): axum::extract::Path<String>,
    axum::extract::RawQuery(query): axum::extract::RawQuery,
    State(state): State<AppState>,
) -> Response {
    let Some(task_id) = athena_types::TaskId::parse(task) else {
        return crate::errors::ApiError::of_status(
            athena_core::protocol::ErrorClass::ValidationError,
            StatusCode::NOT_FOUND,
            "TASK_NOT_FOUND",
            "malformed task id on the path",
            json!({}),
            "",
        )
        .into_response();
    };
    let Some(rows) = task_rows(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":{"code":"CORE_STORE_UNAVAILABLE"}})),
        )
            .into_response();
    };
    let page = crate::pagination::Query::parse(query.as_deref().unwrap_or(""));
    match crate::tasks::task_events(&rows, &task_id, &page) {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn events_stream_row(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Response {
    use athena_core::EventRepository;
    use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
    let Some(rows) = task_rows(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":{"code":"CORE_STORE_UNAVAILABLE"}})),
        )
            .into_response();
    };
    // The client's `Last-Event-ID` header is the gap anchor (ATH-EVENT-020: reconnection
    // fetches by `sequence`). A bounded tail is served at connect; a follow-up reconnect
    // fetches the next gap — the register is the store, never a dropped event.
    let since = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let guard = rows.lock().expect("store");
    let events = EventRepository::list_since(&*guard, since, 500).unwrap_or_default();
    let frames = events
        .iter()
        .map(|event| {
            SseEvent::default().id(event.sequence.to_string()).data(
                serde_json::json!({
                    "event_id": event.event_id.as_str(),
                    "type": event.event_type,
                    "occurred_at": event.occurred_at,
                    "sequence": event.sequence,
                    "correlation_id": event.correlation_id,
                })
                .to_string(),
            )
        })
        .collect::<Vec<_>>();
    let sse_frames =
        futures::stream::iter(frames.into_iter().map(Ok::<_, std::convert::Infallible>));
    Sse::new(sse_frames)
        .keep_alive(KeepAlive::default())
        .into_response()
}

async fn metrics_row(State(state): State<AppState>) -> Response {
    let Some(rows) = task_rows(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            String::from("core store unavailable"),
        )
            .into_response();
    };
    let rendered = {
        let guard = rows.lock().expect("metrics store");
        crate::metrics::render_from(&guard)
    };
    match rendered {
        Ok(text) => (
            StatusCode::OK,
            [("content-type", "text/plain; version=0.0.4")],
            text,
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            String::from("render failed"),
        )
            .into_response(),
    }
}

async fn run_task_row(
    axum::extract::Path(task): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
    State(state): State<AppState>,
) -> Response {
    let Some(task_id) = athena_types::TaskId::parse(task) else {
        return crate::errors::ApiError::of_status(
            athena_core::protocol::ErrorClass::ValidationError,
            StatusCode::NOT_FOUND,
            "TASK_NOT_FOUND",
            "malformed task id on the path",
            json!({}),
            "",
        )
        .into_response();
    };
    let correlation = crate::errors::correlation_of(&headers, request_wall_clock_seed());
    let now = crate::routes::now_text();
    let Some(rows) = task_rows(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":{"code":"CORE_STORE_UNAVAILABLE"}})),
        )
            .into_response();
    };
    match crate::tasks::run_task(&rows, &task_id, &now, &correlation) {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn cancel_task_row(
    axum::extract::Path(task): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
    State(state): State<AppState>,
) -> Response {
    let Some(task_id) = athena_types::TaskId::parse(task) else {
        return crate::errors::ApiError::of_status(
            athena_core::protocol::ErrorClass::ValidationError,
            StatusCode::NOT_FOUND,
            "TASK_NOT_FOUND",
            "malformed task id on the path",
            json!({}),
            "",
        )
        .into_response();
    };
    let Some(rows) = task_rows(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":{"code":"CORE_STORE_UNAVAILABLE"}})),
        )
            .into_response();
    };
    let correlation = crate::errors::correlation_of(&headers, request_wall_clock_seed());
    let now = crate::routes::now_text();
    match crate::tasks::cancel_task(&rows, &task_id, &now, &correlation) {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// Time of a record write, RFC 3339.
pub(crate) fn now_text() -> String {
    athena_core::now_rfc3339().to_string()
}

/// Approve placeholder.
async fn list_approvals_row(
    axum::extract::RawQuery(query): axum::extract::RawQuery,
    State(state): State<AppState>,
) -> Response {
    let Some(rows) = task_rows(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":{"code":"CORE_STORE_UNAVAILABLE"}})),
        )
            .into_response();
    };
    let query = query.as_deref().unwrap_or("");
    let params: Vec<(String, String)> = query
        .split('&')
        .filter_map(|pair| {
            let mut parts = pair.split('=');
            Some((
                parts.next()?.to_string(),
                parts.next().unwrap_or("").to_string(),
            ))
        })
        .collect();
    let state = params
        .iter()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.as_str());
    crate::approvals::list(&rows, state).unwrap_or_else(|error| error.into_response())
}

async fn read_approval_row(
    axum::extract::Path(id): axum::extract::Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let Some(rows) = task_rows(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":{"code":"CORE_STORE_UNAVAILABLE"}})),
        )
            .into_response();
    };
    let correlation = crate::errors::correlation_of(&headers, request_wall_clock_seed());
    crate::approvals::read(&rows, &id, &correlation).unwrap_or_else(|error| error.into_response())
}

async fn approve_or_reject_row(
    axum::extract::Path(id): axum::extract::Path<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let Some(rows) = task_rows(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":{"code":"CORE_STORE_UNAVAILABLE"}})),
        )
            .into_response();
    };
    let correlation = crate::errors::correlation_of(&headers, request_wall_clock_seed());
    let approve = headers
        .get("x-athena-decision")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.eq_ignore_ascii_case("approve"))
        .unwrap_or_else(|| true);
    let now = crate::routes::now_text();
    let caller = crate::auth::from_request(&headers, state.registry.as_ref(), &correlation);
    let Ok(caller) = caller else {
        return crate::auth::from_request(&headers, state.registry.as_ref(), &correlation)
            .err()
            .unwrap()
            .into_response();
    };
    let approver = format!("{}:{}", caller.name, caller.role.as_str());
    crate::approvals::answer(&rows, &id, approve, &approver, &now, &correlation)
        .unwrap_or_else(|error| error.into_response())
}

/// The `Authorization: Bearer <token>` header parse (spec §22.1's form).
#[must_use]
pub fn bearer_of(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}
