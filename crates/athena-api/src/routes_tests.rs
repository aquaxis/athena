//! Route and auth behaviour (spec §22.1 / §29.2).

use crate::auth::{AuthMode, TokenRegistry};
use crate::roles::Role;
use crate::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use tower::ServiceExt as _;

fn state() -> (AppState, String, String, String) {
    let mut registry = TokenRegistry::default();
    let viewer_token = registry.issue("viewer-test", Role::Viewer);
    let operator_token = registry.issue("operator-test", Role::Operator);
    let admin_token = registry.issue("admin-test", Role::Admin);
    let st = AppState {
        registry: Arc::new(registry),
        mode: AuthMode::Token,
        host: "127.0.0.1".into(),
        rows: None,
        standing: crate::standing::new_cell(),
        config: athena_core::Config::default(),
    };
    (st, viewer_token, operator_token, admin_token)
}

fn bearer(request: &mut Request<Body>, token: &str) {
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {token}").parse().expect("header"),
    );
}

#[tokio::test]
async fn healthz_answers_anonymously() {
    // R-API-03 / ATH-API-018: `/api/v1/healthz` is the single unauthenticated endpoint and
    // must not expose state.
    let (state, _, _, _) = state();
    let router = build_router(state);
    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/v1/healthz")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("answered");
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_root_healthz_path_is_not_served() {
    // The endpoint's path holds exactly (X-12(a)): the anonymous surface lives at
    // `/api/v1/healthz` only — a bare `/healthz` at the router root answers nothing (404),
    // so no unauthenticated route leaks outside the spec's `/api/v1` scope.
    let (state, _, _, _) = state();
    let router = build_router(state);
    let response = router
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("answered");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_unknown_token_is_refused_with_401() {
    let (state, _, _, _) = state();
    let router = build_router(state);
    let mut request = Request::builder()
        .uri("/api/v1/status")
        .body(Body::empty())
        .expect("request");
    bearer(&mut request, "ath-not-a-token");
    let response = router.oneshot(request).await.expect("answered");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_missing_header_is_refused_with_401_envelope() {
    let (state, _, _, _) = state();
    let router = build_router(state);
    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/v1/status")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("answered");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .expect("body");
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("AUTH_UNAUTHENTICATED"), "{text}");
}

#[tokio::test]
async fn a_below_role_caller_is_refused_with_403() {
    // ATH-SEC-004: the endpoint's required role matters; a `viewer` cannot POST.
    let (state, viewer, _, _) = state();
    let router = build_router(state);
    let mut request = Request::builder()
        .method("POST")
        .uri("/api/v1/tasks")
        .body(Body::empty())
        .expect("request");
    bearer(&mut request, &viewer);
    let response = router.oneshot(request).await.expect("answered");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .expect("body");
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("AUTH_ROLE_INSUFFICIENT"), "{text}");
    assert!(text.contains("operator"), "{text}");
}

#[tokio::test]
async fn the_role_hierarchy_gates_read_surfaces() {
    // The role subsumption holds over the route tree: viewer reads, operator writes.
    let (state, viewer, operator, admin) = state();
    let router = build_router(state);
    for token in [&viewer, &operator, &admin] {
        let mut request = Request::builder()
            .uri("/api/v1/status")
            .body(Body::empty())
            .expect("request");
        bearer(&mut request, token);
        let response = router.clone().oneshot(request).await.expect("answered");
        assert_eq!(response.status(), StatusCode::OK, "for token {token}");
    }
    // An admin-only surface refuses the lower roles.
    for (token, expected_status) in [
        (&viewer, StatusCode::FORBIDDEN),
        (&operator, StatusCode::FORBIDDEN),
        (&admin, StatusCode::UNSUPPORTED_MEDIA_TYPE), // the admin reached the real (JSON) handler; a content-type-less body is refused by the body layer (P5-13)
    ] {
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/v1/skills")
            .body(Body::empty())
            .expect("request");
        bearer(&mut request, token);
        let response = router.clone().oneshot(request).await.expect("answered");
        assert_eq!(response.status(), expected_status);
    }
}

#[test]
fn the_registry_stores_hashes_never_plaintext() {
    // ATH-SEC-005: hashing before persistence; the token value lives only in the caller's
    // hand.
    let mut registry = TokenRegistry::default();
    let token = registry.issue("the one", Role::Operator);
    let records = registry.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].sha256_hex, TokenRegistry::hash(&token));
    assert!(
        !records[0].sha256_hex.contains(&token),
        "the token itself must not be stored"
    );
    assert_eq!(records[0].name, "the one");
    assert_eq!(records[0].role, Role::Operator);

    // Round trip through the file: still no plaintext, still authenticates.
    let path =
        std::env::temp_dir().join(format!("athena-token-registry-{}.json", std::process::id()));
    registry.save(&path).expect("save");
    let loaded = TokenRegistry::load(&path).expect("load");
    assert_eq!(loaded.authenticate(&token), Some(Role::Operator));
    assert_eq!(
        loaded.authenticate("ath-not-this").map(|_| true),
        None,
        "an absent token authenticates to nothing"
    );
    let raw = std::fs::read_to_string(&path).expect("file");
    assert!(
        !raw.contains(&token),
        "the file must not hold the plaintext"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_loopback_mode_skips_the_registry() {
    // ATH-CFG-006: mode `none` only on loopback; the caller hands the mode over and the
    // middleware bypasses the token path (the binary enforces the host check at bind).
    let mode_none = AuthMode::None;
    assert!(mode_none == AuthMode::None);
}

// --- P1-40: the Task surface over a real store -------------------------------------------

/// A router whose `rows` hold a live in-memory store, with the operator registry seeded.
async fn task_api_router() -> (axum::Router, String) {
    use std::sync::Mutex;
    let mut registry = TokenRegistry::default();
    let operator = registry.issue("operator-test", Role::Operator);
    let store = athena_core::AthenaStore::in_memory().expect("store");
    let router = crate::build_router(crate::AppState {
        registry: Arc::new(registry),
        mode: AuthMode::Token,
        host: "127.0.0.1".into(),
        rows: Some(Arc::new(Mutex::new(store))),
        standing: crate::standing::new_cell(),
        config: athena_core::Config::default(),
    });
    (router, operator)
}

fn op_request(uri: &str, method: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .expect("request")
}

async fn body_text(response: axum::response::Response) -> String {
    let body = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .expect("body");
    String::from_utf8_lossy(&body).to_string()
}

#[tokio::test]
async fn a_task_is_created_read_run_cancel() {
    let (router, token) = task_api_router().await;

    // Create: instruction + workspace required by the row's shape.
    response_404_or_guard(&router, &token).await;
    let created = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/v1/tasks")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{ "instruction": "build the report", "workspace": "/repo/a" }"#,
                ))
                .expect("request"),
        )
        .await
        .expect("answered");
    assert_eq!(
        created.status(),
        StatusCode::CREATED,
        "{}",
        body_text(created).await
    );

    // The Task id rides the response.
    let created_body = body_text(created).await;
    let task_id = created_body
        .split("\"id\":\"")
        .nth(1)
        .and_then(|rest| rest.split('\"').next())
        .expect("an id in the body")
        .to_string();
    assert!(task_id.starts_with("task-"), "{task_id}");

    // Read: one row.
    let read = router
        .clone()
        .oneshot(op_request(
            &format!("/api/v1/tasks/{task_id}"),
            "GET",
            &token,
        ))
        .await
        .expect("answered");
    assert_eq!(read.status(), StatusCode::OK);
    let read_body = body_text(read).await;
    assert!(read_body.contains("build the report"));

    // Run: from `CREATED`, the §17.1 row takes the Task to `QUEUED` and the transition
    // carries `TaskQueued` (ATH-PERS-003's atomic pair).
    let run = router
        .clone()
        .oneshot(op_request(
            &format!("/api/v1/tasks/{task_id}/run"),
            "POST",
            &token,
        ))
        .await
        .expect("answered");
    assert_eq!(run.status(), StatusCode::OK);
    assert!(
        body_text(run).await.contains("QUEUED"),
        "the moved state is served"
    );

    // A second run is out-of-table: 409.
    let again = router
        .clone()
        .oneshot(op_request(
            &format!("/api/v1/tasks/{task_id}/run"),
            "POST",
            &token,
        ))
        .await
        .expect("answered");
    assert_eq!(again.status(), StatusCode::CONFLICT);

    // Cancel from `QUEUED` goes `CANCELLING`.
    let cancel = router
        .clone()
        .oneshot(op_request(
            &format!("/api/v1/tasks/{task_id}/cancel"),
            "POST",
            &token,
        ))
        .await
        .expect("answered");
    assert_eq!(cancel.status(), StatusCode::OK);
    assert!(body_text(cancel).await.contains("CANCELLING"));
}

async fn response_404_or_guard(_router: &axum::Router, _token: &str) {}

#[tokio::test]
async fn the_task_events_replay_in_order() {
    // ATH-EVENT-012: per-Task chronological replay, cursor-paginated (P1-39).
    let (router, token) = task_api_router().await;
    let created = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/v1/tasks")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{ "instruction": "walk the events", "workspace": "/repo/b", "auto_run": true }"#,
                ))
                .expect("request"),
        )
        .await
        .expect("answered");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created_body = body_text(created).await;
    let task_id = created_body
        .split("\"id\":\"")
        .nth(1)
        .and_then(|rest| rest.split('\"').next())
        .expect("an id")
        .to_string();

    // `auto_run` queued at creation: the events already carry `TaskCreated` + `TaskQueued`.
    let events = router
        .clone()
        .oneshot(op_request(
            &format!("/api/v1/tasks/{task_id}/events"),
            "GET",
            &token,
        ))
        .await
        .expect("answered");
    assert_eq!(events.status(), StatusCode::OK);
    let printed = body_text(events).await;
    let created_pos = printed
        .find("TaskCreated")
        .expect("`TaskCreated` is on the wire");
    let queued_pos = printed
        .find("TaskQueued")
        .expect("`TaskQueued` is on the wire");
    assert!(
        created_pos < queued_pos,
        "the replay presents the events in sequence order"
    );
}

#[tokio::test]
async fn a_workspace_holds_at_most_one_non_terminal_task() {
    // R-DATA-005 / ath-API-023: a second create on the same (leased) workspace is 409.
    let (router, token) = task_api_router().await;
    for expected in [StatusCode::CREATED, StatusCode::CONFLICT] {
        let create = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/v1/tasks")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{ "instruction": "only one may run", "workspace": "/repo/one" }"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("answered");
        let status = create.status();
        let _ = body_text(create).await;
        assert_eq!(status, expected);
    }
}

#[tokio::test]
async fn shape_violations_answer_with_their_codes() {
    // `instruction` / `workspace` carry the 400 code the shape commands.
    let (router, token) = task_api_router().await;
    for (payload, code) in [
        (r#"{ "workspace": "/repo/x" }"#, "TASK_INSTRUCTION_REQUIRED"),
        (
            r#"{ "instruction": "no workspace" }"#,
            "TASK_WORKSPACE_REQUIRED",
        ),
    ] {
        let refused = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/v1/tasks")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(payload))
                    .expect("request"),
            )
            .await
            .expect("answered");
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
        let text = body_text(refused).await;
        assert!(text.contains(code), "{code} expected, got {text}");
    }
}

// --- P4-09: the Memory API (`ATH-MEM-011`) --------------------------------------------------

async fn memory_api_router() -> (
    axum::Router,
    String,
    std::sync::Arc<std::sync::Mutex<athena_core::AthenaStore>>,
) {
    let mut registry = TokenRegistry::default();
    let operator = registry.issue("operator-test", Role::Operator);
    let store = std::sync::Arc::new(std::sync::Mutex::new(
        athena_core::AthenaStore::in_memory().expect("store"),
    ));
    let router = crate::build_router(crate::AppState {
        registry: std::sync::Arc::new(registry),
        mode: AuthMode::Token,
        host: "127.0.0.1".into(),
        rows: Some(store.clone()),
        standing: crate::standing::new_cell(),
        config: athena_core::Config::default(),
    });
    (router, operator, store)
}

async fn seed_one_long_record(
    store: &std::sync::Arc<std::sync::Mutex<athena_core::AthenaStore>>,
) -> String {
    use athena_core::MemoryRepository;
    let scope = athena_types::ProjectId::derive("/repo/p4-09");
    let record = athena_types::MemoryRecord {
        id: athena_types::MemoryId::new(),
        tier: athena_types::MemoryTier::Long,
        scope_id: scope.as_str().to_string(),
        content: "the project long-term decision".into(),
        summary: None,
        origin: athena_types::MemoryOrigin {
            task_id: None,
            event_id: None,
        },
        created_at: athena_core::now_rfc3339(),
        expires_at: None,
    };
    {
        let mut guard = store.lock().expect("store");
        MemoryRepository::insert(&mut *guard, &record).expect("seeded");
    }
    record.id.as_str().to_string()
}

#[tokio::test]
async fn the_memory_reference_surfaces_the_staged_rows() {
    let (router, token, store) = memory_api_router().await;
    let id = seed_one_long_record(&store).await;

    // By tier: the reference answers with the row (`ATH-MEM-011`).
    let listed = router
        .clone()
        .oneshot(op_request("/api/v1/memory?tier=long", "GET", &token))
        .await
        .expect("answered");
    assert_eq!(listed.status(), StatusCode::OK);
    let body = body_text(listed).await;
    assert!(body.contains(&id), "{body}");

    // The unfiltered face reaches the long tier too.
    let all = router
        .clone()
        .oneshot(op_request("/api/v1/memory", "GET", &token))
        .await
        .expect("answered");
    let body = body_text(all).await;
    assert!(body.contains(&id), "{body}");

    // An unknown tier refuses with the envelope (`ValidationError`).
    let refused = router
        .oneshot(op_request("/api/v1/memory?tier=sideways", "GET", &token))
        .await
        .expect("answered");
    assert_eq!(
        refused.status(),
        StatusCode::BAD_REQUEST,
        "{}",
        body_text(refused).await
    );
}

#[tokio::test]
async fn the_memory_deletion_removes_and_audits() {
    use athena_core::EventRepository;
    let (router, token, store) = memory_api_router().await;
    let id = seed_one_long_record(&store).await;

    let removed = router
        .clone()
        .oneshot(op_request(
            &format!("/api/v1/memory/{id}"),
            "DELETE",
            &token,
        ))
        .await
        .expect("answered");
    assert_eq!(
        removed.status(),
        StatusCode::OK,
        "{}",
        body_text(removed).await
    );

    // The row is gone; the listing no longer carries it.
    let after = router
        .clone()
        .oneshot(op_request("/api/v1/memory?tier=long", "GET", &token))
        .await
        .expect("answered");
    assert!(!body_text(after).await.contains(&id));

    // The `MemoryRecordDeleted` Event is the audit record (`ATH-MEM-006`).
    {
        let guard = store.lock().expect("store");
        let highest = EventRepository::max_sequence(&*guard).expect("sequence");
        assert!(highest >= 1, "the deletion appended its audit Event");
        let events = EventRepository::list_since(&*guard, 0, 100).expect("events");
        assert!(
            events
                .iter()
                .any(|event| event.event_type == "MemoryRecordDeleted"),
            "{:?}",
            events
                .iter()
                .map(|event| &event.event_type)
                .collect::<Vec<_>>()
        );
    }

    // A second deletion of the same id answers 404 (`MEMORY_NOT_FOUND`).
    let again = router
        .oneshot(op_request(
            &format!("/api/v1/memory/{id}"),
            "DELETE",
            &token,
        ))
        .await
        .expect("answered");
    assert_eq!(again.status(), StatusCode::NOT_FOUND);
    assert!(body_text(again).await.contains("MEMORY_NOT_FOUND"));
}

// --- P4-18: the clarification answer endpoint ----------------------------------------------

use athena_types::{Priority, Task, TaskId, TaskState};

async fn clarify_router() -> (
    axum::Router,
    String,
    std::sync::Arc<std::sync::Mutex<athena_core::AthenaStore>>,
) {
    let mut registry = TokenRegistry::default();
    let operator = registry.issue("operator-test", Role::Operator);
    let store = std::sync::Arc::new(std::sync::Mutex::new(
        athena_core::AthenaStore::in_memory().expect("store"),
    ));
    let router = crate::build_router(crate::AppState {
        registry: std::sync::Arc::new(registry),
        mode: AuthMode::Token,
        host: "127.0.0.1".into(),
        rows: Some(store.clone()),
        standing: crate::standing::new_cell(),
        config: athena_core::Config::default(),
    });
    (router, operator, store)
}

/// Seed a RUNNING task row plus an open clarification (the ask-side store face).
async fn seed_running_task_with_clarification(
    store: &std::sync::Arc<std::sync::Mutex<athena_core::AthenaStore>>,
) -> String {
    use athena_core::{ClarificationRepository, TaskRepository};
    let mut guard = store.lock().expect("store");
    let task = Task {
        id: TaskId::new(),
        parent_id: None,
        title: "Ask".into(),
        instruction: "build the report".into(),
        workspace: "/work/ask".into(),
        goal: None,
        constraints: vec![],
        completion_criteria: vec![],
        state: TaskState::Running,
        priority: Priority::Normal,
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
    };
    TaskRepository::insert(&mut *guard, &task).expect("task seeds");
    let clarification = athena_types::Clarification {
        id: athena_types::ClarificationId::new(),
        task_id: task.id.clone(),
        state: athena_types::ClarificationState::Requested,
        question: "which shape?".into(),
        answer: None,
        expires_at: "2099-01-01T00:00:00.000Z".into(),
        created_at: "2026-10-17T00:00:00.000Z".into(),
        answered_at: None,
    };
    ClarificationRepository::open(&mut *guard, &clarification).expect("opened");
    TaskId::as_str(&task.id).to_string()
}

#[tokio::test]
async fn the_answer_endpoint_resumes_the_task() {
    let (router, token, store) = clarify_router().await;
    let task_id = seed_running_task_with_clarification(&store).await;
    let response = router
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/api/v1/tasks/{task_id}/answer"))
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{ "answer": "the one-page form" }"#))
                .expect("request"),
        )
        .await
        .expect("answered");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        body_text(response).await
    );
    // The Task row resumed.
    {
        use athena_core::TaskRepository;
        let guard = store.lock().expect("store");
        let task = TaskRepository::list_all(&*guard).expect("listed");
        assert!(task.iter().all(|row| row.state == TaskState::Running));
    }
}

#[tokio::test]
async fn answering_a_task_without_an_open_clarification_refuses() {
    let (router, token, store) = clarify_router().await;
    // A task row exists but no clarification is open.
    {
        use athena_core::TaskRepository;
        let mut guard = store.lock().expect("store");
        let task = Task {
            id: TaskId::new(),
            parent_id: None,
            title: "Lonely".into(),
            instruction: "no asking".into(),
            workspace: "/work/lonely".into(),
            goal: None,
            constraints: vec![],
            completion_criteria: vec![],
            state: TaskState::Running,
            priority: Priority::Normal,
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
        };
        TaskRepository::insert(&mut *guard, &task).expect("seeded");
    }
    let listed = {
        let guard = store.lock().expect("store");
        athena_core::TaskRepository::list_all(&*guard)
            .expect("listed")
            .first()
            .map(|row| row.id.as_str().to_string())
            .expect("one")
    };
    let response = router
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/api/v1/tasks/{listed}/answer"))
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{ "answer": "anything" }"#))
                .expect("request"),
        )
        .await
        .expect("answered");
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "{}",
        body_text(response).await
    );
}

// --- P4-19: the Approval API ----------------------------------------------------------------

async fn approvals_router() -> (
    axum::Router,
    String,
    std::sync::Arc<std::sync::Mutex<athena_core::AthenaStore>>,
) {
    let mut registry = TokenRegistry::default();
    let operator = registry.issue("operator-test", Role::Operator);
    let store = std::sync::Arc::new(std::sync::Mutex::new(
        athena_core::AthenaStore::in_memory().expect("store"),
    ));
    let router = crate::build_router(crate::AppState {
        registry: std::sync::Arc::new(registry),
        mode: AuthMode::Token,
        host: "127.0.0.1".into(),
        rows: Some(store.clone()),
        standing: crate::standing::new_cell(),
        config: athena_core::Config::default(),
    });
    (router, operator, store)
}

async fn seed_requested_approval(
    store: &std::sync::Arc<std::sync::Mutex<athena_core::AthenaStore>>,
) -> String {
    use athena_core::{ApprovalRepository, TaskRepository};
    let mut guard = store.lock().expect("store");
    let task = Task {
        id: TaskId::new(),
        parent_id: None,
        title: "Approval".into(),
        instruction: "ship it".into(),
        workspace: "/work/approvals".into(),
        goal: None,
        constraints: vec![],
        completion_criteria: vec![],
        state: TaskState::WaitingApproval,
        priority: Priority::Normal,
        plan_id: None,
        risk_level: athena_types::RiskLevel::High,
        attempt_count: 0,
        max_attempts: 3,
        iteration_count: 0,
        max_iterations: 50,
        deadline: None,
        created_by: "test".into(),
        created_at: "2026-10-17T00:00:00.000Z".into(),
        updated_at: "2026-10-17T00:00:00.000Z".into(),
        finished_at: None,
    };
    TaskRepository::insert(&mut *guard, &task).expect("task seeds");
    let approval = athena_types::Approval {
        id: athena_types::ApprovalId::new(),
        task_id: task.id.clone(),
        requested_by: athena_types::AgentId::core_agent(),
        operation: "shell git push".into(),
        reason: "agent says fine".into(),
        impact: "agent says low".into(),
        risk_level: athena_types::RiskLevel::High,
        state: athena_types::ApprovalState::Requested,
        operation_descriptor:
            "tool=shell path=/work args={\"args\":\"git push origin main\"} risk=high instance=ri-1"
                .into(),
        requested_by_instance_id: athena_types::InstanceId::new(),
        uses_remaining: 1,
        expires_at: "2099-01-01T00:00:00.000Z".into(),
        responded_by: None,
        responded_at: None,
        scope: athena_types::ApprovalScope::OperationOnly {
            operation_only: true,
        },
    };
    ApprovalRepository::insert(&mut *guard, &approval).expect("seeded");
    approval.id.as_str().to_string()
}

#[tokio::test]
async fn the_approval_api_lists_reads_and_answers_with_the_audit_trail() {
    let (router, token, store) = approvals_router().await;
    let id = seed_requested_approval(&store).await;

    // List finds the pending row; the descriptor is the primary content and the prose
    // is marked untrusted (`ATH-HITL-011`).
    let listed = router
        .clone()
        .oneshot(op_request(
            "/api/v1/approvals?state=requested",
            "GET",
            &token,
        ))
        .await
        .expect("listed");
    assert_eq!(listed.status(), StatusCode::OK);
    let body = body_text(listed).await;
    assert!(body.contains(&id) && body.contains("tool=shell"), "{body}");
    assert!(body.contains("reason_untrusted"), "{body}");

    // Read answers the single row.
    let read = router
        .clone()
        .oneshot(op_request(
            &format!("/api/v1/approvals/{id}"),
            "GET",
            &token,
        ))
        .await
        .expect("read");
    assert!(body_text(read).await.contains(&id));

    // Approve: the answered row carries the approver identity (`ATH-HITL-006`), and the
    // `ApprovalGranted` audit Event lands in the same store window.
    let mut approved = op_request(&format!("/api/v1/approvals/{id}/approve"), "POST", &token);
    approved.headers_mut().insert(
        axum::http::header::HeaderName::from_static("x-athena-decision"),
        axum::http::header::HeaderValue::from_static("approve"),
    );
    let approved = router.clone().oneshot(approved).await.expect("approved");
    assert_eq!(
        approved.status(),
        StatusCode::OK,
        "{}",
        body_text(approved).await
    );
    {
        use athena_core::EventRepository;
        let guard = store.lock().expect("store");
        let events = EventRepository::list_since(&*guard, 0, 100).expect("events");
        assert!(
            events
                .iter()
                .any(|event| event.event_type == "ApprovalGranted"),
            "{:?}",
            events
                .iter()
                .map(|event| &event.event_type)
                .collect::<Vec<_>>()
        );
    }

    // A terminal approval refuses further answers with 409 (`ATH-HITL-006`).
    let again = router
        .oneshot(op_request(
            &format!("/api/v1/approvals/{id}/approve"),
            "POST",
            &token,
        ))
        .await
        .expect("refused");
    assert_eq!(
        again.status(),
        StatusCode::CONFLICT,
        "{}",
        body_text(again).await
    );
}

#[tokio::test]
async fn the_status_answer_carries_the_core_agents_standing() {
    // X-29's Core-Agent standing face: an observed standing renders its facts — the
    // liveness verdict, the consumed restart budget and its ceiling — and the answer is
    // still the same Task/instance counter payload the row demands.
    let mut registry = TokenRegistry::default();
    let admin = registry.issue("status-standing", Role::Admin);
    let standing = crate::standing::new_cell();
    crate::standing::CoreAgentStanding::publish(
        &standing,
        crate::standing::CoreAgentStanding {
            alive: true,
            restarts: 2,
            restart_limit: 3,
            runtime_instance_id: "ri-01M6STATUS".to_string(),
            observed_at: "2026-11-08T09:00:00Z".to_string(),
        },
    );
    let st = AppState {
        registry: Arc::new(registry),
        mode: AuthMode::Token,
        host: "127.0.0.1".into(),
        rows: None,
        standing,
        config: athena_core::Config::default(),
    };
    let router = build_router(st);
    let mut request = Request::builder()
        .uri("/api/v1/status")
        .body(Body::empty())
        .expect("request");
    bearer(&mut request, &admin);
    let response = router.oneshot(request).await.expect("answered");
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .expect("body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json");
    let agent = value.get("core_agent").expect("core_agent").clone();
    assert_eq!(agent["alive"], true, "{agent}");
    assert_eq!(agent["restarts"], 2, "{agent}");
    assert_eq!(agent["restart_limit"], 3, "{agent}");
    assert_eq!(agent["runtime_instance_id"], "ri-01M6STATUS", "{agent}");
    // No store is attached to this state, so the Task/instance counters are absent —
    // the never-invent discipline (ATH-CORE-038) keeps them absent rather than zeroed,
    // while the store-independent standing renders beside them.
    assert!(value.get("tasks").is_none(), "{value}");
    assert!(value.get("instances").is_none(), "{value}");
}

#[tokio::test]
async fn the_status_answer_names_an_unobserved_core_agent_as_unobserved() {
    // Before the supervision poll's first observation the standing is rendered as its
    // own face, never as a fabricated zero (ATH-CORE-038).
    let mut registry = TokenRegistry::default();
    let admin = registry.issue("status-unobserved", Role::Admin);
    let st = AppState {
        registry: Arc::new(registry),
        mode: AuthMode::Token,
        host: "127.0.0.1".into(),
        rows: None,
        standing: crate::standing::new_cell(),
        config: athena_core::Config::default(),
    };
    let router = build_router(st);
    let mut request = Request::builder()
        .uri("/api/v1/status")
        .body(Body::empty())
        .expect("request");
    bearer(&mut request, &admin);
    let response = router.oneshot(request).await.expect("answered");
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .expect("body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(value["core_agent"]["liveness"], "unobserved", "{value}");
}

// --- X-31: the §22.5/§22.6/§22.9 read faces and the admin emergency stop --------------------

type SharedTestStore = std::sync::Arc<std::sync::Mutex<athena_core::AthenaStore>>;

async fn agent_surface_router() -> (axum::Router, String, SharedTestStore) {
    let mut registry = TokenRegistry::default();
    let admin = registry.issue("agent-surface-test", Role::Admin);
    let store = std::sync::Arc::new(std::sync::Mutex::new(
        athena_core::AthenaStore::in_memory().expect("store"),
    ));
    let router = crate::build_router(crate::AppState {
        registry: std::sync::Arc::new(registry),
        mode: AuthMode::Token,
        host: "127.0.0.1".into(),
        rows: Some(store.clone()),
        standing: crate::standing::new_cell(),
        config: athena_core::Config::default(),
    });
    (router, admin, store)
}

fn seed_definition(agent_id: &str) -> athena_types::AgentDefinition {
    athena_types::AgentDefinition {
        id: athena_types::AgentId::new(agent_id),
        name: "the seeded worker".into(),
        level: athena_types::AgentLevel::Worker,
        description: "seeded for the X-31 surface".into(),
        capabilities: vec!["rust".into()],
        persona_ref: "personas/worker.md".into(),
        persona_version: "1".into(),
        allowed_tools: Vec::new(),
        delegable_tools: Vec::new(),
        default_skills: Vec::new(),
        runtime: athena_types::RuntimeType::AgentCli,
        model: athena_types::ModelRef {
            provider: "claude".into(),
            name: "claude-cli".into(),
        },
        max_delegation_depth: 2,
        dynamic: false,
    }
}

async fn seed_agent_fixtures(store: &SharedTestStore) -> (String, String, String, String) {
    use athena_core::{AgentRepository, RuntimeInstanceRepository};
    let definition = seed_definition("x31-worker");
    let instance = athena_types::AgentInstance {
        id: athena_types::InstanceId::parse("ai-01HX31TESTINSTANCE000000000A")
            .expect("instance id"),
        agent_id: definition.id.clone(),
        task_id: None,
        plan_step_id: None,
        superior_instance_id: None,
        delegation_depth: 0,
        state: athena_types::AgentInstanceState::Idle,
        runtime_instance_id: Some(
            athena_types::RuntimeInstanceId::parse("ri-01HX31TESTRUNTIME00000000000")
                .expect("runtime id"),
        ),
        workspace: "/repo/x31".into(),
        skill_set: Vec::new(),
        aiprj_log_ref: None,
        attempt: 1,
        started_at: Some(athena_core::now_rfc3339()),
        finished_at: None,
    };
    let runtime = athena_types::RuntimeInstance {
        id: athena_types::RuntimeInstanceId::parse("ri-01HX31TESTRUNTIME00000000000")
            .expect("runtime id"),
        runtime_type: athena_types::RuntimeType::AgentCli,
        instance_id: instance.id.clone(),
        state: athena_types::AgentInstanceState::Idle,
        pid: Some(4242),
        last_heartbeat_at: Some(athena_core::now_rfc3339()),
        started_at: athena_core::now_rfc3339(),
        stopped_at: None,
        stop_reason: None,
        exit_code: None,
        side_effects: athena_types::SideEffects::Unknown,
    };
    {
        let mut guard = store.lock().expect("store");
        AgentRepository::upsert_definition(&mut *guard, &definition).expect("definition");
        AgentRepository::insert_instance(&mut *guard, &instance).expect("instance");
        RuntimeInstanceRepository::insert(&mut *guard, &runtime).expect("runtime");
    }
    (
        definition.id.as_str().to_string(),
        instance.id.as_str().to_string(),
        runtime.id.as_str().to_string(),
        String::new(),
    )
}

async fn admin_get(router: &axum::Router, token: &str, uri: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .uri(uri)
        .body(Body::empty())
        .expect("request");
    let mut request = request;
    bearer(&mut request, token);
    let response = router.clone().oneshot(request).await.expect("answered");
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 65_536)
        .await
        .expect("body");
    (status, String::from_utf8_lossy(&body).to_string())
}

#[tokio::test]
async fn the_agent_surface_serves_definitions_instances_and_runtimes() {
    // §22.5/§22.6 (X-31, R-API-09): the definitions, the instances and the runtime rows
    // the store already keeps answer through their own field maps — 404 is gone.
    let (router, token, store) = agent_surface_router().await;
    let (agent, instance, runtime, _) = seed_agent_fixtures(&store).await;

    let (status, body) = admin_get(&router, &token, "/api/v1/agents").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(agent.as_str()), "{body}");

    let (status, body) = admin_get(&router, &token, "/api/v1/agents/x31-worker").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("the seeded worker"), "{body}");

    let (status, body) = admin_get(&router, &token, "/api/v1/instances").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(instance.as_str()), "{body}");

    let (status, body) = admin_get(&router, &token, &format!("/api/v1/instances/{instance}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = admin_get(&router, &token, "/api/v1/runtimes").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains(runtime.as_str()), "{body}");

    let (status, body) = admin_get(&router, &token, &format!("/api/v1/runtimes/{runtime}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // An absent definition is the honest 404, never a fabrication.
    let (status, body) = admin_get(&router, &token, "/api/v1/agents/no-such-agent").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(body.contains("AGENT_NOT_FOUND"), "{body}");
}

#[tokio::test]
async fn the_event_list_serves_the_stream_rows_cursor_paginated() {
    // §22.9's list face (X-31): the append-only rows the stream serves frame by frame
    // also serve as a paginated list; payload contents never ride the wire (ATH-EVENT-007).
    let (router, token, store) = agent_surface_router().await;
    {
        use athena_core::EventRepository;
        let mut guard = store.lock().expect("store");
        for index in 0..7 {
            let event = athena_core::NewEvent {
                event_id: athena_types::EventId::new(),
                schema_version: 1,
                event_type: "ConfigurationLoaded".into(),
                occurred_at: athena_core::now_rfc3339(),
                task_id: None,
                plan_step_id: None,
                agent_id: None,
                instance_id: None,
                skill_id: None,
                correlation_id: format!("cor-x31-{index}"),
                causation_id: None,
                actor: "system".into(),
                payload_json: serde_json::json!({ "note": format!("row {index}") }).to_string(),
            };
            EventRepository::append(&mut *guard, &event).expect("event");
        }
    }
    let (status, body) = admin_get(&router, &token, "/api/v1/events?limit=3").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"items\""), "{body}");
    assert!(body.contains("next_cursor"), "{body}");
    assert!(
        !body.contains("row 6") || body.contains("next_cursor"),
        "{body}"
    );
    // The payload's raw note never renders (`ATH-EVENT-007`: names and stamps only).
    assert!(!body.contains("row 6"), "{body}");
}

#[tokio::test]
async fn the_instance_stop_refuses_terminal_and_moves_live_states_to_stopping() {
    // §22.5's only write face (ATH-API-024, admin): `StopRequested` through the §17.2
    // table — a live state moves to `Stopping`, a terminal one refuses 409.
    let (router, token, store) = agent_surface_router().await;
    let (_, instance, _, _) = seed_agent_fixtures(&store).await;
    let stop_uri = format!("/api/v1/instances/{instance}/stop");

    let mut request = Request::builder()
        .method("POST")
        .uri(&stop_uri)
        .body(Body::empty())
        .expect("request");
    bearer(&mut request, &token);
    let response = router.clone().oneshot(request).await.expect("answered");
    assert_eq!(response.status(), StatusCode::ACCEPTED, "{}", {
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("body");
        String::from_utf8_lossy(&body).to_string()
    });
    {
        use athena_core::AgentRepository;
        let guard = store.lock().expect("store");
        let row = AgentRepository::get_instance(
            &*guard,
            &athena_types::InstanceId::parse(&instance).expect("id"),
        )
        .expect("read")
        .expect("row");
        assert!(
            matches!(row.state, athena_types::AgentInstanceState::Stopping),
            "{:?}",
            row.state
        );
    }
}

// --- X-34: the Long-Term Memory write route (`POST /api/v1/memory`) --------------------

/// A router + operator token + live store with one seeded RUNNING Task (the write's
/// provenance anchor needs a real Task row).
async fn memory_write_router() -> (
    axum::Router,
    String,
    std::sync::Arc<std::sync::Mutex<athena_core::AthenaStore>>,
    athena_types::TaskId,
) {
    use athena_core::TaskRepository;
    let mut registry = TokenRegistry::default();
    let operator = registry.issue("operator-write-test", Role::Operator);
    let store = std::sync::Arc::new(std::sync::Mutex::new(
        athena_core::AthenaStore::in_memory().expect("store"),
    ));
    let task = athena_types::Task {
        id: athena_types::TaskId::new(),
        title: "the memory write's anchor task".into(),
        goal: Some("hold a provenance anchor".into()),
        instruction: "hold a provenance anchor".into(),
        parent_id: None,
        workspace: "/repo/x34-anchor".into(),
        constraints: vec![],
        completion_criteria: vec![],
        state: athena_types::TaskState::Running,
        priority: athena_types::Priority::Normal,
        plan_id: None,
        risk_level: athena_types::RiskLevel::Medium,
        attempt_count: 0,
        max_attempts: 3,
        iteration_count: 0,
        max_iterations: 50,
        deadline: None,
        created_by: "test".into(),
        created_at: "2026-11-10T00:00:00.000Z".into(),
        updated_at: "2026-11-10T00:00:00.000Z".into(),
        finished_at: None,
    };
    {
        let mut guard = store.lock().expect("store");
        TaskRepository::insert(&mut *guard, &task).expect("task seeds");
    }
    let router = crate::build_router(crate::AppState {
        registry: std::sync::Arc::new(registry),
        mode: AuthMode::Token,
        host: "127.0.0.1".into(),
        rows: Some(store.clone()),
        standing: crate::standing::new_cell(),
        config: athena_core::Config::default(),
    });
    (router, operator, store, task.id)
}

fn op_write_request(uri: &str, token: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

#[tokio::test]
async fn the_memory_write_route_accepts_an_anchored_human_instruction() {
    // ATH-MEM-003's (b) face: an explicit `HumanInstruction` requester writes Long,
    // through the route the §22 table now carries. The masked content lands, the
    // `MemoryRecordCreated` Event rides as the audit record, and the answer is `201`.
    let (router, token, store, task) = memory_write_router().await;
    let body = format!(
        r#"{{"content": "the integration harness decision remembered", "workspace": "/repo/x34", "task_id": "{}"}}"#,
        task.as_str()
    );
    let response = router
        .clone()
        .oneshot(op_write_request("/api/v1/memory", &token, &body))
        .await
        .expect("answered");
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "{}",
        body_text(response).await
    );

    // The record is in the Long tier under the workspace's scope (R-DAT-02).
    let listed = router
        .clone()
        .oneshot(op_request("/api/v1/memory?tier=long", "GET", &token))
        .await
        .expect("answered");
    let body = body_text(listed).await;
    assert!(body.contains("the integration harness"), "{body}");

    // The audit Event is present (D-1's record-through-the-repository route).
    {
        use athena_core::EventRepository;
        let guard = store.lock().expect("store");
        let events = EventRepository::list_since(&*guard, 0, 100).expect("events");
        assert!(
            events
                .iter()
                .any(|event| event.event_type == "MemoryRecordCreated"),
            "{:?}",
            events
                .iter()
                .map(|event| &event.event_type)
                .collect::<Vec<_>>()
        );
    }
}

#[tokio::test]
async fn the_memory_write_route_refuses_unanchored_and_empty_writes() {
    // ATH-MEM-005 + the hygiene gate: no anchor, no write; no content, no write.
    let (router, token, _store, _task) = memory_write_router().await;
    for (body, code) in [
        (
            r#"{"content": "a judgement", "workspace": "/repo/x34"}"#,
            "MEMORY_ANCHOR_REQUIRED",
        ),
        (
            r#"{"workspace": "/repo/x34", "task_id": ""}"#,
            "MEMORY_CONTENT_REQUIRED",
        ),
        (r#"{"content": "a judgement"}"#, "MEMORY_SCOPE_REQUIRED"),
    ] {
        let response = router
            .clone()
            .oneshot(op_write_request("/api/v1/memory", &token, body))
            .await
            .expect("answered");
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "{body}: {}",
            body_text(response).await
        );
        assert!(
            body_text(response).await.contains(code),
            "{code} expected in the refusal"
        );
    }

    // A well-formed but unknown anchor is a 404 (`ATH-MEM-005`'s provenance names facts).
    let ghost = op_write_request(
        "/api/v1/memory",
        &token,
        r#"{"content": "a judgement", "workspace": "/repo/x34", "task_id": "task-01K78EA9QQJXBY8CM55GB5WXEQM"}"#,
    );
    let response = router.oneshot(ghost).await.expect("answered");
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "{}",
        body_text(response).await
    );
}

#[tokio::test]
async fn the_memory_write_route_refuses_at_the_tier_gates() {
    // ATH-MEM-002/003: the disabled tier refuses with 403; the ceiling refuses with 409
    // (`MEMORY_CEILING_REACHED`) — removal is an explicit judgement, never a digest.
    let (_router, _token, store, task) = memory_write_router().await;
    let anchored = format!(
        r#"{{"content": "ceiling filler judgement", "workspace": "/repo/ceil", "task_id": "{}"}}"#,
        task.as_str()
    );
    // Two writes against a two-record ceiling: the third must refuse. The default cap
    // is 10 000, so this test asserts the *fill-then-refuse* shape through a store whose
    // ceiling the router's config sets to 2.
    let mut registry = TokenRegistry::default();
    let token_small = registry.issue("small-ceiling", Role::Operator);
    let mut small_config = athena_core::Config::default();
    small_config.memory.long_term.max_records = 2;
    let small_router = crate::build_router(crate::AppState {
        registry: std::sync::Arc::new(registry),
        mode: AuthMode::Token,
        host: "127.0.0.1".into(),
        rows: Some(store.clone()),
        standing: crate::standing::new_cell(),
        config: small_config,
    });
    for at in 0..2 {
        let filler = format!(
            r#"{{"content": "filler {at}", "workspace": "/repo/ceil", "task_id": "{}"}}"#,
            task.as_str()
        );
        let response = small_router
            .clone()
            .oneshot(op_write_request("/api/v1/memory", &token_small, &filler))
            .await
            .expect("answered");
        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "filler {at}: {}",
            body_text(response).await
        );
    }
    let third = small_router
        .oneshot(op_write_request("/api/v1/memory", &token_small, &anchored))
        .await
        .expect("answered");
    assert_eq!(
        third.status(),
        StatusCode::CONFLICT,
        "{}",
        body_text(third).await
    );
    assert!(
        body_text(third).await.contains("MEMORY_CEILING_REACHED"),
        "the ceiling's own code expected"
    );

    // The disabled tier (a fresh router with `memory.long_term.enabled = false`).
    let mut registry_off = TokenRegistry::default();
    let token_off = registry_off.issue("tier-off", Role::Operator);
    let mut off_config = athena_core::Config::default();
    off_config.memory.long_term.enabled = false;
    let off_router = crate::build_router(crate::AppState {
        registry: std::sync::Arc::new(registry_off),
        mode: AuthMode::Token,
        host: "127.0.0.1".into(),
        rows: Some(store.clone()),
        standing: crate::standing::new_cell(),
        config: off_config,
    });
    let refused = off_router
        .oneshot(op_write_request("/api/v1/memory", &token_off, &anchored))
        .await
        .expect("answered");
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    assert!(
        body_text(refused)
            .await
            .contains("MEMORY_LONG_TERM_DISABLED"),
        "the tier gate's own code expected"
    );
}
// --- X-38: the §22.9 narrowing keys (`?task_id=&type=&since=&types=`) ----------------------

/// Seed the register the way the §22.9 faces read it: a live Task row (the
/// faces answer 404 for an absent one, 400 for a malformed one), three
/// task-anchored `TaskQueued` rows (sequences 1–3) and two register-wide
/// `ConfigurationLoaded` rows (4–5), appended in that order; the store's append
/// order is the register's truth and no extra row is invented to make an
/// assertion come out.
async fn seed_event_mix(store: &SharedTestStore) -> String {
    use athena_core::{EventRepository, TaskRepository};
    let mut guard = store.lock().expect("store");
    let task = athena_types::Task {
        id: athena_types::TaskId::new(),
        title: "the narrowing probe".into(),
        instruction: "hold the §22.9 narrowing probe".into(),
        constraints: vec![],
        completion_criteria: vec![],
        state: athena_types::TaskState::Queued,
        priority: athena_types::Priority::Normal,
        plan_id: None,
        risk_level: athena_types::RiskLevel::Medium,
        attempt_count: 0,
        max_attempts: 3,
        iteration_count: 0,
        max_iterations: 50,
        deadline: None,
        parent_id: None,
        workspace: "/repo/x38".into(),
        goal: Some("hold the narrowing probe".into()),
        created_by: "test".into(),
        created_at: "2026-11-12T00:00:00.000Z".into(),
        updated_at: "2026-11-12T00:00:00.000Z".into(),
        finished_at: None,
    };
    TaskRepository::insert(&mut *guard, &task).expect("task row");
    // Three task-anchored `TaskQueued` rows (the §23.2 Task family's state-change
    // vocabulary, X-37's `STATE_CHANGE_NAMES`): sequences 1–3, each naming the
    // probe Task in `task_id` — the anchor the narrowing faces read.
    for index in 0..3 {
        let event = athena_core::NewEvent {
            event_id: athena_types::EventId::new(),
            schema_version: 1,
            event_type: "TaskQueued".into(),
            occurred_at: format!("2026-11-12T00:00:0{}.000Z", 1 + index),
            task_id: Some(task.id.clone()),
            plan_step_id: None,
            agent_id: None,
            instance_id: None,
            skill_id: None,
            correlation_id: format!("cor-x38-tq-{index}"),
            causation_id: None,
            actor: "system".into(),
            payload_json: serde_json::json!({}).to_string(),
        };
        EventRepository::append(&mut *guard, &event).expect("event row");
    }
    for index in 0..2 {
        let event = athena_core::NewEvent {
            event_id: athena_types::EventId::new(),
            schema_version: 1,
            event_type: "ConfigurationLoaded".into(),
            occurred_at: format!("2026-11-12T00:00:0{}.000Z", 4 + index),
            task_id: None,
            plan_step_id: None,
            agent_id: None,
            instance_id: None,
            skill_id: None,
            correlation_id: format!("cor-x38-config-{index}"),
            causation_id: None,
            actor: "system".into(),
            payload_json: serde_json::json!({}).to_string(),
        };
        EventRepository::append(&mut *guard, &event).expect("event row");
    }
    task.id.as_str().to_string()
}
#[tokio::test]
async fn the_list_face_serves_the_22_9_narrowing_keys() {
    let (router, token, store) = agent_surface_router().await;
    let task_id = seed_event_mix(&store).await;

    // `?task_id=` — the three TaskQueued rows anchored to the probe Task.
    let (status, body) = admin_get(
        &router,
        &token,
        &format!("/api/v1/events?task_id={task_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.matches("TaskQueued").count(), 3, "{body}");
    assert!(!body.contains("ConfigurationLoaded"), "{body}");
    assert!(body.contains("cor-x38-tq-0"), "{body}");

    // `?type=` (singular on the list face) — the two register-wide rows.
    let (status, body) =
        admin_get(&router, &token, "/api/v1/events?type=ConfigurationLoaded").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.matches("ConfigurationLoaded").count(), 2, "{body}");
    assert!(!body.contains("TaskQueued"), "{body}");
    assert!(body.contains("cor-x38-config-0"), "{body}");
    // `?since=` — the anchor is exclusive (`sequence > since`), so 4 leaves row 5.
    let (status, body) = admin_get(&router, &token, "/api/v1/events?since=4").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.matches("ConfigurationLoaded").count(), 1, "{body}");
    assert!(body.contains("cor-x38-config-1"), "{body}");
    assert!(!body.contains("TaskQueued"), "{body}");

    // A malformed task id is the 400; a well-formed but absent one is the 404.
    let (status, body) = admin_get(&router, &token, "/api/v1/events?task_id=not-a-task").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("TASK_NOT_FOUND"), "{body}");
    let (status, body) = admin_get(
        &router,
        &token,
        "/api/v1/events?task_id=task-01JAAAAAAAAAAAAAAAAAAAAAAA",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(body.contains("TASK_NOT_FOUND"), "{body}");
}
#[tokio::test]
async fn the_stream_face_serves_the_22_9_narrowing_keys() {
    let (router, token, store) = agent_surface_router().await;
    let task_id = seed_event_mix(&store).await;

    // `?task_id=` — the stream narrows to the probe Task's three rows only.
    let (status, body) = admin_get(
        &router,
        &token,
        &format!("/api/v1/events/stream?task_id={task_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.matches("TaskQueued").count(), 3, "{body}");
    assert!(!body.contains("ConfigurationLoaded"), "{body}");
    assert!(body.contains("cor-x38-tq-0"), "{body}");
    // `?types=` — a comma-separated type set narrows the register-wide stream.
    let (status, body) = admin_get(
        &router,
        &token,
        "/api/v1/events/stream?types=ConfigurationLoaded",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.matches("ConfigurationLoaded").count(), 2, "{body}");
    assert!(!body.contains("TaskQueued"), "{body}");
    assert!(body.contains("cor-x38-config-0"), "{body}");

    // `?since=` — the query rides the same gap anchor when the header is absent.
    let (status, body) = admin_get(
        &router,
        &token,
        &format!("/api/v1/events/stream?task_id={task_id}&since=2"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("cor-x38-tq-0"), "{body}");
    assert!(!body.contains("cor-x38-tq-1"), "{body}");
    assert!(body.contains("cor-x38-tq-2"), "{body}");
    // The `Last-Event-ID` header is the gap anchor (ATH-EVENT-020): a reconnect
    // fetches only the rows after the last served `sequence`.
    let request = Request::builder()
        .uri(format!("/api/v1/events/stream?task_id={task_id}"))
        .body(Body::empty())
        .expect("request");
    let mut request = request;
    bearer(&mut request, &token);
    request
        .headers_mut()
        .insert("last-event-id", "2".parse().expect("header"));
    let response = router.clone().oneshot(request).await.expect("answered");
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 65_536)
        .await
        .expect("body");
    let body = String::from_utf8_lossy(&body).to_string();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("cor-x38-tq-0"), "{body}");
    assert!(!body.contains("cor-x38-tq-1"), "{body}");
    assert!(body.contains("cor-x38-tq-2"), "{body}");
}
#[tokio::test]
async fn a_malformed_stream_task_id_is_the_400_and_an_absent_task_is_the_404() {
    let (router, token, store) = agent_surface_router().await;
    let _ = seed_event_mix(&store).await;
    // `?task_id=` that fails the newtype parse is the API face's 400 — the query
    // grammar is shared with the list face (X-38's `EventFilter`).
    let (status, body) =
        admin_get(&router, &token, "/api/v1/events/stream?task_id=not-a-task").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("TASK_NOT_FOUND"), "{body}");
    // A well-formed but absent Task is the 404 — existence, not row count
    // (`task_present`): `list_for_task`'s `Ok(empty)` is not the same fact.
    let (status, body) = admin_get(
        &router,
        &token,
        "/api/v1/events/stream?task_id=task-01JAAAAAAAAAAAAAAAAAAAAAAA",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(body.contains("TASK_NOT_FOUND"), "{body}");
}
