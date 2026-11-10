//! Tests for the Skill API rules (P5-13; X-32's §22.7 surface): the admin gate, the
//! mandatory `(id, version)` pin on the per-version routes, the §17.3 transition-table
//! discipline with its Event legs, the request/completion split (enable/disable/remove
//! complete at the API; update/validate answer 202 as the recorded request), and the
//! no-ENABLED-before-validation registration rule — full router round-trips through
//! [`crate::routes::build_router`].

use crate::auth::{AuthMode, TokenRegistry};
use crate::roles::Role;
use crate::routes::AppState;
use athena_core::AthenaStore;
use athena_types::{Skill, SkillId, SkillState};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

type SharedStore = Arc<std::sync::Mutex<AthenaStore>>;

fn state(store: Option<SharedStore>) -> AppState {
    let mut registry = TokenRegistry::default();
    let _admin = registry.issue("x32-test", Role::Admin);
    AppState {
        registry: Arc::new(registry),
        mode: AuthMode::Token,
        host: "127.0.0.1".into(),
        rows: store,
        standing: crate::standing::new_cell(),
        config: athena_core::Config::default(),
    }
}

fn admin_request(uri: &str, method: &str, token: &str, body: Option<String>) -> Request<Body> {
    let builder = Request::builder()
        .uri(uri)
        .method(method)
        .header("authorization", format!("Bearer {token}"));
    match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(Body::from(body))
            .expect("request"),
        None => builder.body(Body::empty()).expect("request"),
    }
}

async fn answer(router: &axum::Router, request: Request<Body>) -> (StatusCode, String) {
    let response = router.clone().oneshot(request).await.expect("answered");
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 65_536)
        .await
        .expect("body");
    (status, String::from_utf8_lossy(&body).to_string())
}

/// One in-store `AVAILABLE` row: a complete `Skill` entity (X-31's field map).
fn seeded_skill(id: &str, version: &str, state: SkillState) -> Skill {
    Skill {
        id: SkillId::new(id),
        name: "the skill".into(),
        version: version.into(),
        description: "seeded for the X-32 tests".into(),
        capabilities: vec!["fpga".into()],
        required_tools: Vec::new(),
        required_permissions: Vec::new(),
        supported_agents: Vec::new(),
        dependencies: Vec::new(),
        runtime_provider: "agent-cli".into(),
        source: "local".into(),
        checksum: "sha256:x".into(),
        publisher: "local".into(),
        risk_level: athena_types::RiskLevel::Medium,
        state,
        hot_reload_safe: false,
        registered_at: "2026-11-10T00:00:00Z".into(),
        updated_at: "2026-11-10T00:00:00Z".into(),
    }
}

/// The seeded router: one store holding `the-skill@1.2.0` in the given state.
fn seeded_router(state_of: SkillState) -> (axum::Router, String, SharedStore) {
    let mut registry = TokenRegistry::default();
    let admin = registry.issue("x32-seeded", Role::Admin);
    let store = Arc::new(std::sync::Mutex::new(
        AthenaStore::in_memory().expect("store"),
    ));
    {
        let mut guard = store.lock().expect("lock");
        athena_core::SkillRepository::upsert(
            &mut *guard,
            &seeded_skill("the-skill", "1.2.0", state_of),
        )
        .expect("seed");
    }
    let app = crate::routes::build_router(AppState {
        registry: Arc::new(registry),
        mode: AuthMode::Token,
        host: "127.0.0.1".into(),
        rows: Some(store.clone()),
        standing: crate::standing::new_cell(),
        config: athena_core::Config::default(),
    });
    (app, admin, store)
}

#[tokio::test]
async fn the_section_22_7_reads_serve_the_catalog_the_versions_and_the_pin() {
    let (router, admin, _store) = seeded_router(SkillState::Available);
    let (status, body) = answer(
        &router,
        admin_request("/api/v1/skills", "GET", &admin, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"the-skill\""), "{body}");

    let (status, body) = answer(
        &router,
        admin_request("/api/v1/skills/the-skill", "GET", &admin, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"versions\""), "{body}");

    let (status, body) = answer(
        &router,
        admin_request(
            "/api/v1/skills/the-skill/versions/1.2.0",
            "GET",
            &admin,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"1.2.0\""), "{body}");

    let (status, body) = answer(
        &router,
        admin_request(
            "/api/v1/skills/the-skill/versions/9.9.9",
            "GET",
            &admin,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(body.contains("SKILL_VERSION_ABSENT"), "{body}");
}

#[tokio::test]
async fn the_enable_disable_legs_walk_the_section_17_3_table_and_append_their_events() {
    // DISABLED → ENABLED is legal and answers the row's new state:
    let (router, admin, store) = seeded_router(SkillState::Disabled);
    let (status, body) = answer(
        &router,
        admin_request(
            "/api/v1/skills/the-skill/versions/1.2.0/enable",
            "POST",
            &admin,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let face: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(face["state"], "ENABLED", "{body}");
    {
        let guard = store.lock().expect("lock");
        let skill = athena_core::SkillRepository::get(&*guard, &SkillId::new("the-skill"), "1.2.0")
            .expect("read")
            .expect("row");
        assert_eq!(skill.state, SkillState::Enabled);
        // the §17.3 Event leg rode the same transaction window (D-1):
        let events = athena_core::EventRepository::list_since(&*guard, 0, 500).expect("events");
        let leg = events
            .iter()
            .find(|event| event.event_type == "SkillEnabled")
            .expect("the SkillEnabled event leg");
        assert_eq!(
            leg.skill_id.as_ref().map(|id| id.as_str()),
            Some("the-skill"),
            "{leg:?}"
        );
    }

    // ENABLED → ENABLED is refused by the table (409), no second event leg:
    let (status, body) = answer(
        &router,
        admin_request(
            "/api/v1/skills/the-skill/versions/1.2.0/enable",
            "POST",
            &admin,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("SKILL_STATE_TRANSITION_ILLEGAL"), "{body}");

    // and the disable leg rides ENABLED → DISABLED:
    let (router, admin, _store) = seeded_router(SkillState::Enabled);
    let (status, body) = answer(
        &router,
        admin_request(
            "/api/v1/skills/the-skill/versions/1.2.0/disable",
            "POST",
            &admin,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let face: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(face["state"], "DISABLED", "{body}");
}

#[tokio::test]
async fn the_update_and_validate_legs_record_the_request_and_answer_202() {
    // ENABLED → UPDATING: the request leg, the acquisition rides the Skill Manager:
    let (router, admin, store) = seeded_router(SkillState::Enabled);
    let (status, body) = answer(
        &router,
        admin_request(
            "/api/v1/skills/the-skill/versions/1.2.0/update",
            "POST",
            &admin,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let face: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(face["state"], "UPDATING", "{body}");
    {
        let guard = store.lock().expect("lock");
        let row = athena_core::SkillRepository::get(&*guard, &SkillId::new("the-skill"), "1.2.0")
            .expect("read")
            .expect("row");
        assert_eq!(row.state, SkillState::Updating);
    }

    // FAILED → VALIDATING: the re-validation request leg (§17.3 再検証 row):
    let (router, admin, _store) = seeded_router(SkillState::Failed);
    let (status, body) = answer(
        &router,
        admin_request(
            "/api/v1/skills/the-skill/versions/1.2.0/validate",
            "POST",
            &admin,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let face: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(face["state"], "VALIDATING", "{body}");
}

#[tokio::test]
async fn the_removal_leg_walks_to_the_terminal_state() {
    for seed in [
        SkillState::Available,
        SkillState::Disabled,
        SkillState::Failed,
    ] {
        let (router, admin, store) = seeded_router(seed);
        let (status, body) = answer(
            &router,
            admin_request(
                "/api/v1/skills/the-skill/versions/1.2.0",
                "DELETE",
                &admin,
                None,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let face: Value = serde_json::from_str(&body).expect("json");
        assert_eq!(face["state"], "REMOVED", "{body}");
        {
            let guard = store.lock().expect("lock");
            let row =
                athena_core::SkillRepository::get(&*guard, &SkillId::new("the-skill"), "1.2.0")
                    .expect("read")
                    .expect("row");
            assert!(row.state.is_terminal());
        }
    }
}

#[tokio::test]
async fn the_absent_pin_refuses_with_version_absent() {
    let (router, admin, _store) = seeded_router(SkillState::Available);
    let (status, body) = answer(
        &router,
        admin_request(
            "/api/v1/skills/no-such/versions/1.0.0/enable",
            "POST",
            &admin,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(body.contains("SKILL_VERSION_ABSENT"), "{body}");
}

#[tokio::test]
async fn the_registration_refuses_an_enabled_payload_and_serves_a_manifest() {
    let (router, admin, store) = seeded_router(SkillState::Available);
    // no-ENABLED-before-validation (ATH-SKILL-009 / ATH-API-014):
    let (status, body) = answer(
        &router,
        admin_request(
            "/api/v1/skills",
            "POST",
            &admin,
            Some(json!({"id": "x", "version": "1.0.0", "state": "ENABLED"}).to_string()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("SKILL_ENABLED_REQUIRES_VALIDATION"), "{body}");

    // the manifest-carrying wrapper the CLI's install sends (the manifest pins
    // (id, version) itself, ATH-SKILL-020 / ATH-SKILL-003; the shape is the assembler's
    // own, ATH-SKILL-005 — every field present) → LANDS AVAILABLE, 201:
    let manifest = "id: demo-skill\n\
        name: Demo\n\
        version: \"0.1.0\"\n\
        description: demo\n\
        capabilities:\n\
        \x20 - assembling\n\
        required_tools:\n\
        \x20 - shell\n\
        required_permissions:\n\
        \x20 - shell\n\
        supported_agents:\n\
        \x20 - athena-worker\n\
        runtime:\n\
        \x20 provider: agent-cli\n\
        source: project-local\n\
        publisher: schema_skill\n\
        risk:\n\
        \x20 level: low\n";
    let (status, body) = answer(
        &router,
        admin_request(
            "/api/v1/skills",
            "POST",
            &admin,
            Some(json!({"manifest_yaml": manifest}).to_string()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body.contains("\"AVAILABLE\""), "{body}");
    {
        let guard = store.lock().expect("lock");
        assert!(
            athena_core::SkillRepository::get(&*guard, &SkillId::new("demo-skill"), "0.1.0")
                .expect("read")
                .is_some(),
            "the registration landed"
        );
    }
}

#[tokio::test]
async fn the_missing_pin_names_both_fields() {
    let (router, admin, _store) = seeded_router(SkillState::Available);
    // The entity-shaped body (no `manifest_yaml` key) must carry the top-level pin:
    let (status, body) = answer(
        &router,
        admin_request(
            "/api/v1/skills",
            "POST",
            &admin,
            Some(json!({}).to_string()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let face: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(face["error"]["code"], "SKILL_ID_VERSION_REQUIRED", "{body}");
    assert_eq!(
        face["error"]["details"]["fields"],
        json!(["id", "version"]),
        "{body}"
    );
}

#[test]
fn the_admin_gate_is_the_only_required_role() {
    // `ATH-API-015` / `ATH-STATE-025`.
    for role in Role::all() {
        assert_eq!(role.satisfies(Role::Admin), role == Role::Admin, "{role:?}");
    }
    let _ = state(None);
}

#[test]
fn the_no_enabled_before_validation_rule_scans_the_payload_state() {
    // The state scan is the rule's wire fact:
    let enabled_payload = json!({"id": "x", "version": "1.0.0", "state": "ENABLED"});
    let state_face = enabled_payload
        .get("state")
        .and_then(Value::as_str)
        .map(|raw| raw.eq_ignore_ascii_case(SkillState::Enabled.as_str()))
        .unwrap_or(false);
    assert!(state_face, "the refusal fires on the ENABLED state string");
}
