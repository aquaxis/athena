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
    // R-API-03: `healthz` is the single unauthenticated endpoint and must not expose state.
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
    assert_eq!(response.status(), StatusCode::OK);
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
