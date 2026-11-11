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
//! `GET /api/v1/healthz` rides the router *outside* any layer and is the single
//! unauthenticated endpoint (R-API-03, ATH-API-018): it answers a literal `ok`, exposing
//! no internal state.
//!
//! P1-37 is the skeleton: the handler bodies stubbed to a stable response shape; specific
//! endpoints (the Task / Plan / Event groups) land with `P1-40` and the tests there.

use crate::auth::{AuthMode, TokenRegistry};
use crate::roles::Role;
use axum::{
    Json, Router,
    extract::{Extension, Request, State},
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
    /// The Core Agent's standing (X-29): the supervision poll's facts, published every
    /// poll so `/status` names instance #0's liveness, its restart count and the
    /// restart-window head-room (the never-invent discipline: `None` until the poll has
    /// observed once, and the route renders its absence rather than a fabricated zero).
    pub standing: crate::standing::StandingCell,
    /// The resolved effective configuration (X-34): the memory write route's tier gate
    /// and retention ceiling read `memory.long_term.*` from here.
    pub config: athena_core::Config,
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

/// Build the router: `/api/v1/healthz` anonymous; the `/api/v1` groups each gated by their
/// required-role layer.
///
/// The layers stack per group: the routes come first and the layer applies to exactly
/// those, so the role hierarchy `viewer ⊂ operator ⊂ admin` (ATH-SEC-003) is the routing
/// tree's own structure.
#[must_use = "a router that is never served answers nothing"]
pub fn build_router(state: AppState) -> Router {
    // `/api/v1/healthz` first, no layer (R-API-03: the single unauthenticated endpoint).
    let health = Router::new().route("/api/v1/healthz", get(healthz));

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
                    "/api/v1/skills/{skill_id}",
                    axum::routing::get(crate::skills::list_skill_versions),
                )
                .route(
                    "/api/v1/skills/{skill_id}/versions/{version}",
                    axum::routing::get(crate::skills::read_skill_version),
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
        .route("/api/v1/tasks/{task_id}/plan", get(task_plan_row))
        .route("/api/v1/tasks/{task_id}/results", get(task_results_row))
        .route("/api/v1/tasks/{task_id}/messages", get(task_messages_row))
        .route("/api/v1/agents", get(agents_list_row))
        .route("/api/v1/agents/{agent_id}", get(agent_read_row))
        .route("/api/v1/instances", get(instances_list_row))
        .route("/api/v1/instances/{instance_id}", get(instance_read_row))
        .route(
            "/api/v1/instances/{instance_id}/messages",
            get(instance_messages_row),
        )
        .route("/api/v1/hierarchies/{task_id}", get(hierarchy_read_row))
        .route("/api/v1/tool-executions", get(tool_executions_row))
        .route("/api/v1/evaluations", get(evaluations_row))
        .route("/api/v1/runtimes", get(runtimes_list_row))
        .route("/api/v1/runtimes/{runtime_id}", get(runtime_read_row))
        .route("/api/v1/events", get(events_list_row))
        .route(
            "/api/v1/tasks/{task_id}/clarification",
            get(task_clarification_row),
        )
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
        .route("/api/v1/memory", axum::routing::post(create_memory_row))
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
            "/api/v1/skills/{skill_id}/versions/{version}/enable",
            axum::routing::post(crate::skills::enable_skill_version),
        )
        .route(
            "/api/v1/skills/{skill_id}/versions/{version}/disable",
            axum::routing::post(crate::skills::disable_skill_version),
        )
        .route(
            "/api/v1/skills/{skill_id}/versions/{version}/update",
            axum::routing::post(crate::skills::update_skill_version),
        )
        .route(
            "/api/v1/skills/{skill_id}/versions/{version}/validate",
            axum::routing::post(crate::skills::validate_skill_version),
        )
        .route(
            "/api/v1/skills/{skill_id}/versions/{version}",
            axum::routing::delete(crate::skills::remove_skill_version),
        )
        .route(
            // The spec's only write face on the instance group (ATH-API-024): the admin
            // emergency stop, never an instance *start*.
            "/api/v1/instances/{instance_id}/stop",
            axum::routing::post(stop_instance_row),
        );
    health
        .merge(endpoint_layer(&state, Role::Viewer, viewer))
        .merge(endpoint_layer(&state, Role::Operator, operator))
        .merge(endpoint_layer(&state, Role::Admin, admin))
        .layer(axum::middleware::from_fn(cors_layer))
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
                // The resolved identity rides into the handlers (`ATH-SEC-005`'s audit
                // name; X-34's memory write names the caller's registered token, not
                // `"bearer"`).
                let mut request = request;
                request.extensions_mut().insert(caller);
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

/// The CORS preface for one response (X-30): the Studio's page (origin
/// `http://127.0.0.1:5173`, spec §4.3.2) fetches the API on another port, so a browser
/// needs the cross-origin permission read of its screen. The layer is confined to the
/// same loopback rule the API binds under (`ATH-OPS-004`, `ATH-CFG-006`): only a
/// `127.0.0.1` / `localhost` origin is answered, everything else is left un-permitted.
///
/// Non-browser callers (the CLI, the token verb) stay unaffected: their requests carry
/// no `Origin` header and the preface simply does not apply.
fn cors_preface(origin: Option<&str>, request_method: &str) -> Vec<(&'static str, String)> {
    let Some(origin) = origin else {
        return Vec::new();
    };
    let permitted = origin == "http://127.0.0.1:5173"
        || origin == "http://localhost:5173"
        || origin == "http://127.0.0.1:5174";
    if !permitted {
        return Vec::new();
    }
    let mut headers = vec![
        ("access-control-allow-origin", origin.to_string()),
        (
            "access-control-allow-methods",
            "GET, POST, DELETE, OPTIONS".to_string(),
        ),
        (
            "access-control-allow-headers",
            "authorization, content-type, idempotency-key".to_string(),
        ),
        ("vary", "origin".to_string()),
    ];
    if request_method.eq_ignore_ascii_case("OPTIONS") {
        headers.push(("access-control-max-age", "600".to_string()));
    }
    headers
}

/// The CORS middleware (X-30): stamps the preface on every answer and answers the
/// browser's preflight (`OPTIONS`) itself — 204 with the allow headers, no handler run.
/// Registered outermost so `healthz` and every role-gated route answer uniformly, and
/// so a preflight never needs a token (the browser asks before the credential exists).
async fn cors_layer(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::Response as AxumResponse;
    let origin = request
        .headers()
        .get(axum::http::header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let method = request.method().as_str().to_string();
    if method.eq_ignore_ascii_case("OPTIONS") && origin.is_some() {
        let mut response = AxumResponse::builder()
            .status(axum::http::StatusCode::NO_CONTENT)
            .body(axum::body::Body::empty())
            .expect("a bodyless 204 builds");
        for (name, value) in cors_preface(origin.as_deref(), &method) {
            if let Ok(parsed) = axum::http::HeaderValue::from_str(&value) {
                response.headers_mut().insert(name, parsed);
            }
        }
        return response;
    }
    let mut response = next.run(request).await;
    for (name, value) in cors_preface(origin.as_deref(), &method) {
        if let Ok(parsed) = axum::http::HeaderValue::from_str(&value) {
            response.headers_mut().insert(name, parsed);
        }
    }
    response
}

/// The status counters (X-29; spec §22): the daemon's standing, read from the store.
///
/// The answer carries the facts an operator needs to see whether the system is doing
/// work — the Task population by state, the queue length, the instances that exist, and
/// the approvals waiting for a human. Every value is counted from the store, so it is the
/// same truth the API serves elsewhere; when no store is attached the counters are absent
/// rather than invented (a skeleton that reports fabricated zeros would hide a boot
/// fault). No build metadata is read (design §2.7's no built-in version string
/// discipline) — the release identity is the git tag.
async fn status_stub(State(state): State<AppState>) -> Json<Value> {
    // The Core Agent's standing (X-29) is store-independent: the supervision poll's
    // facts render even when no store is attached, while the Task / instance counters —
    // which read the store — stay absent rather than invented (ATH-CORE-038).
    let core_agent = match state.standing.lock() {
        Ok(guard) => match guard.as_ref() {
            Some(observed) => json!({
                "alive": observed.alive,
                "restarts": observed.restarts,
                "restart_limit": observed.restart_limit,
                "runtime_instance_id": observed.runtime_instance_id,
                "observed_at": observed.observed_at,
            }),
            None => json!({ "liveness": "unobserved" }),
        },
        Err(_) => json!({ "liveness": "unobservable" }),
    };
    let Some(rows) = task_rows(&state) else {
        return Json(json!({ "status": "ready", "core_agent": core_agent }));
    };
    let Ok(guard) = rows.lock() else {
        return Json(json!({
            "status": "degraded",
            "detail": "store lock unavailable",
            "core_agent": core_agent,
        }));
    };

    // The Task population, by state. A store fault is reported as such rather than
    // silently rendered as zero.
    let mut tasks_by_state = serde_json::Map::new();
    let mut total_tasks: u64 = 0;
    for task_state in athena_core::TaskState::ALL {
        match athena_core::TaskRepository::list_by_state(&*guard, *task_state, u64::MAX) {
            Ok(found) => {
                total_tasks += found.len() as u64;
                tasks_by_state.insert(task_state.as_str().to_string(), json!(found.len() as u64));
            }
            Err(error) => {
                return Json(json!({
                    "status": "degraded",
                    "detail": format!("counting tasks failed: {error}"),
                    "core_agent": core_agent,
                }));
            }
        }
    }
    let queued = tasks_by_state
        .get(athena_core::TaskState::Queued.as_str())
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let running = tasks_by_state
        .get(athena_core::TaskState::Running.as_str())
        .and_then(Value::as_u64)
        .unwrap_or(0);

    // The instances, and how many are working (BUSY) or waiting on a human.
    let instances = athena_core::AgentRepository::list_all_instances(&*guard).unwrap_or_default();
    let mut instances_busy: u64 = 0;
    let mut instances_waiting: u64 = 0;
    for instance in &instances {
        match instance.state {
            athena_types::AgentInstanceState::Busy => instances_busy += 1,
            athena_types::AgentInstanceState::Waiting
            | athena_types::AgentInstanceState::Suspended => instances_waiting += 1,
            _ => {}
        }
    }

    // The approvals still awaiting an answer (the human-facing backlog).
    let approvals_pending = athena_core::ApprovalRepository::list_by_state(
        &*guard,
        athena_types::ApprovalState::Requested,
    )
    .map(|found| found.len() as u64)
    .unwrap_or(0);

    Json(json!({
        "status": "ready",
        "core_agent": core_agent,
        "tasks": {
            "total": total_tasks,
            "queued": queued,
            "running": running,
            "by_state": tasks_by_state,
        },
        "instances": {
            "total": instances.len() as u64,
            "busy": instances_busy,
            "waiting": instances_waiting,
        },
        "approvals_pending": approvals_pending,
    }))
}

/// The Task store on the route state (a lazily built shared store; the Core Host
/// process opens the real database at boot — the tests use in-memory stores).
pub(crate) fn task_rows(state: &AppState) -> Option<Arc<Mutex<athena_core::AthenaStore>>> {
    state.rows.clone()
}

/// The Task's pending clarification (X-27's `athena chat` leg): the question text the
/// conversation renders, with its deadline — the same row `POST …/answer` consumes, so
/// reading it is read-only (`viewer`) and answering stays the operator write.
///
/// No open clarification ⇒ 404 (`CLARIFICATION_NONE`); store faults ⇒ 500.
async fn task_clarification_row(
    axum::extract::Path(task_id): axum::extract::Path<String>,
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
    crate::tasks::read_clarification(&rows, &task, &correlation).into_response()
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

async fn create_memory_row(
    State(state): State<AppState>,
    Extension(caller): Extension<crate::auth::ApiCaller>,
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
    let correlation = crate::errors::correlation_of(&headers, request_wall_clock_seed());
    let now = crate::routes::now_text();
    // The caller's registered name is the write's audit identity (`HumanInstruction
    // { identity }`): the layer resolved the bearer and inserted the caller, so a
    // layer-covered route reads its extension with no re-hash; `none`-mode loopback
    // carries the name the middleware minted.
    let caller_identity = caller.name;
    crate::memory::create(
        &rows,
        &state.config,
        &payload,
        &caller_identity,
        &now,
        &correlation,
    )
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
    axum::extract::RawQuery(query): axum::extract::RawQuery,
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
    // The §22.9 wire keys are read through the shared grammar (X-38): `?task_id=`
    // narrows to one Task's rows, `?types=` (comma-separated) narrows to the named
    // types; a `task_id` that fails the newtype parse is the API face's 400.
    let raw = query.as_deref().unwrap_or("");
    let pairs = form_urlencoded_pairs(raw);
    let filter = athena_core::event_filter::EventFilter::parse(&pairs);
    // A `?task_id=` that fails the newtype parse is the API face's 400 (the list
    // face answers the same shape); without this the stream would silently serve
    // the unfiltered register.
    if pairs
        .iter()
        .any(|(key, value)| key == "task_id" && athena_types::TaskId::parse(value).is_none())
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": {
                    "code": "TASK_NOT_FOUND",
                    "message": "malformed task id on the stream filter"
                }
            })),
        )
            .into_response();
    }
    if let Some(task) = &filter.task {
        {
            let guard = rows.lock().expect("store");
            // Existence, not row count: an absent Task is 404 (`task_present`);
            // `list_for_task`'s `Ok(empty)` is not the same fact.
            if let Err(error) = crate::agents::task_present(&guard, task) {
                return error.into_response();
            }
        }
    }
    // The client's `Last-Event-ID` header is the gap anchor (ATH-EVENT-020: reconnection
    // fetches by `sequence`); a `?since=` in the query rides the same anchor when given.
    // A bounded tail is served at connect; a follow-up reconnect fetches the next gap —
    // the register is the store, never a dropped event.
    let since = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .or(filter.since)
        .unwrap_or(0);
    let guard = rows.lock().expect("store");
    let events = EventRepository::list_since(&*guard, since, u64::MAX)
        .unwrap_or_default()
        .into_iter()
        .filter(|event| filter.matches(event))
        .collect::<Vec<_>>();
    // A bounded tail remains the delivery shape; the newest `500` filtered rows serve.
    let taken = events.len().saturating_sub(500).min(events.len());
    let served = &events[taken..];
    let frames = served
        .iter()
        .map(|event| {
            SseEvent::default().id(event.sequence.to_string()).data(
                serde_json::json!({
                    "event_id": event.event_id.as_str(),
                    "type": event.event_type,
                    "task_id": event.task_id.as_ref().map(athena_types::TaskId::as_str),
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
    let state_filter = crate::routes::form_urlencoded_pairs(query.as_deref().unwrap_or(""))
        .into_iter()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value);
    crate::approvals::list(&rows, state_filter.as_deref())
        .unwrap_or_else(|error| error.into_response())
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

/// The shared query-pair face the §22.5/§22.9 read handlers ride: `RawQuery` is the raw
/// string, so the pairs are split here once (the same shape `list_approvals_row` uses).
async fn agents_list_row(
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
    crate::agents::list_definitions(&rows, &page).unwrap_or_else(|error| error.into_response())
}

async fn agent_read_row(
    axum::extract::Path(agent_id): axum::extract::Path<String>,
    State(state): State<AppState>,
) -> Response {
    let Some(rows) = task_rows(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":{"code":"CORE_STORE_UNAVAILABLE"}})),
        )
            .into_response();
    };
    crate::agents::read_definition(&rows, &agent_id).unwrap_or_else(|error| error.into_response())
}

async fn instances_list_row(
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
    crate::agents::list_instances(&rows, &page).unwrap_or_else(|error| error.into_response())
}

async fn instance_read_row(
    axum::extract::Path(instance_id): axum::extract::Path<String>,
    State(state): State<AppState>,
) -> Response {
    let Some(rows) = task_rows(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":{"code":"CORE_STORE_UNAVAILABLE"}})),
        )
            .into_response();
    };
    crate::agents::read_instance(&rows, &instance_id).unwrap_or_else(|error| error.into_response())
}

async fn instance_messages_row(
    axum::extract::Path(instance_id): axum::extract::Path<String>,
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
    crate::agents::list_instance_messages(&rows, &instance_id, &page)
        .unwrap_or_else(|error| error.into_response())
}

async fn hierarchy_read_row(
    axum::extract::Path(task_id): axum::extract::Path<String>,
    State(state): State<AppState>,
) -> Response {
    let Some(rows) = task_rows(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":{"code":"CORE_STORE_UNAVAILABLE"}})),
        )
            .into_response();
    };
    crate::agents::read_hierarchy(&rows, &task_id).unwrap_or_else(|error| error.into_response())
}

async fn tool_executions_row(
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
    crate::agents::list_tool_executions(&rows, &page).unwrap_or_else(|error| error.into_response())
}

async fn evaluations_row(
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
    crate::agents::list_evaluations(&rows, &page).unwrap_or_else(|error| error.into_response())
}

async fn runtimes_list_row(
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
    crate::agents::list_runtimes(&rows, &page).unwrap_or_else(|error| error.into_response())
}

async fn runtime_read_row(
    axum::extract::Path(runtime_id): axum::extract::Path<String>,
    State(state): State<AppState>,
) -> Response {
    let Some(rows) = task_rows(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":{"code":"CORE_STORE_UNAVAILABLE"}})),
        )
            .into_response();
    };
    crate::agents::read_runtime(&rows, &runtime_id).unwrap_or_else(|error| error.into_response())
}

async fn events_list_row(
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
    // The §22.9 list keys (`?task_id=&type=&since=&limit=&cursor=`) parse through the
    // shared grammar (X-38); `limit=&cursor=` stay pagination's own keys. The list
    // face's singular `type=` narrows to one named row type.
    let raw = query.as_deref().unwrap_or("");
    let page = crate::pagination::Query::parse(raw);
    let pairs = form_urlencoded_pairs(raw);
    let task_id = pairs
        .iter()
        .find(|(key, _)| key == "task_id")
        .map(|(_, value)| value.as_str());
    let type_wire = pairs
        .iter()
        .find(|(key, _)| key == "type")
        .map(|(_, value)| value.as_str());
    let since = pairs
        .iter()
        .find(|(key, _)| key == "since")
        .and_then(|(_, value)| value.parse::<u64>().ok());
    crate::agents::list_events(&rows, &page, task_id, type_wire, since)
        .unwrap_or_else(|error| error.into_response())
}

async fn task_plan_row(
    axum::extract::Path(task_id): axum::extract::Path<String>,
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
    let raw = query.as_deref().unwrap_or("");
    let revision = form_urlencoded_pairs(raw)
        .iter()
        .find(|(key, _)| key == "revision")
        .and_then(|(_, value)| value.parse::<u64>().ok());
    crate::agents::read_task_plan(&rows, &task_id, revision)
        .unwrap_or_else(|error| error.into_response())
}

async fn task_results_row(
    axum::extract::Path(task_id): axum::extract::Path<String>,
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
    crate::agents::list_task_results(&rows, &task_id, &page)
        .unwrap_or_else(|error| error.into_response())
}

async fn task_messages_row(
    axum::extract::Path(task_id): axum::extract::Path<String>,
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
    crate::agents::list_task_messages(&rows, &task_id, &page)
        .unwrap_or_else(|error| error.into_response())
}

async fn stop_instance_row(
    axum::extract::Path(instance_id): axum::extract::Path<String>,
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
    let now = crate::routes::now_text();
    crate::agents::stop_instance(&rows, &instance_id, &now, &correlation)
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
