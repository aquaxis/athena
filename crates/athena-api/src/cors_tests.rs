use crate::auth::{AuthMode, TokenRegistry};
use crate::roles::Role;
use crate::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;

fn state() -> AppState {
    let mut registry = TokenRegistry::default();
    let _admin = registry.issue("cors-test", Role::Admin);
    AppState {
        registry: std::sync::Arc::new(registry),
        mode: AuthMode::Token,
        host: "127.0.0.1".into(),
        rows: None,
        standing: crate::standing::new_cell(),
        config: athena_core::Config::default(),
    }
}

/// The Studio's origin is answered with the allow headers (X-30).
#[tokio::test]
async fn the_studio_origin_is_answered_with_the_cors_allow_headers() {
    let router = build_router(state());
    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/v1/healthz")
                .header("origin", "http://127.0.0.1:5173")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("answered");
    assert_eq!(response.status(), StatusCode::OK);
    let origin = response
        .headers()
        .get("access-control-allow-origin")
        .expect("allow-origin")
        .to_str()
        .expect("header text");
    assert_eq!(origin, "http://127.0.0.1:5173", "{origin}");
    let methods = response
        .headers()
        .get("access-control-allow-methods")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert!(methods.contains("GET"), "{methods}");
    assert!(methods.contains("POST"), "{methods}");
}

/// A non-Studio origin gets no permission: the headers are simply absent (the
/// browser's own refusal does the blocking).
#[tokio::test]
async fn a_foreign_origin_earns_no_cors_permission() {
    let router = build_router(state());
    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/v1/healthz")
                .header("origin", "http://evil.example:5173")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("answered");
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .get("access-control-allow-origin")
            .is_none(),
        "a foreign origin must not be answered with permission"
    );
}

/// A request without `Origin` (the CLI, the token verb) is untouched.
#[tokio::test]
async fn a_non_browser_caller_ride_without_cors_headers() {
    let router = build_router(state());
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
    assert!(
        response
            .headers()
            .get("access-control-allow-origin")
            .is_none()
    );
}

/// The preflight (`OPTIONS` + `Origin`) is answered by the layer itself: 204 with the
/// allow headers and no handler run (a preflight requires no credential).
#[tokio::test]
async fn a_preflight_is_answered_204_with_the_allow_headers() {
    use axum::http::Method;
    let router = build_router(state());
    let response = router
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/api/v1/tasks")
                .header("origin", "http://127.0.0.1:5173")
                .header("access-control-request-method", "POST")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("answered");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .and_then(|value| value.to_str().ok()),
        Some("http://127.0.0.1:5173")
    );
}
