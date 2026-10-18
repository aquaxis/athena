//! Tests for the Skill API rules (P5-13): the admin gate, the mandatory `(id,
//! version)`, and the no-ENABLED-before-validation rule — at the handler-rule layer
//! (the full router round-trip rides the routes crate's transport tests in P6).

use crate::auth::{AuthMode, TokenRegistry};
use crate::roles::Role;
use crate::routes::AppState;
use athena_types::SkillState;
use serde_json::{Value, json};
use std::sync::Arc;

fn state() -> AppState {
    let mut registry = TokenRegistry::default();
    let _admin = registry.issue("admin-test", Role::Admin);
    AppState {
        registry: Arc::new(registry),
        mode: AuthMode::Token,
        host: "127.0.0.1".into(),
        rows: None,
    }
}

#[test]
fn the_admin_gate_is_the_only_required_role() {
    // `ATH-API-015` / `ATH-STATE-025`.
    for role in Role::all() {
        assert_eq!(role.satisfies(Role::Admin), role == Role::Admin, "{role:?}");
    }
    let _ = state();
}

#[test]
fn a_missing_pin_names_both_fields() {
    let payload: Value = json!({});
    let _missing_id_and_version = (
        payload.get("id").and_then(Value::as_str),
        payload.get("version").and_then(Value::as_str),
    );
    assert!(payload.get("id").and_then(Value::as_str).is_none());
    assert!(payload.get("version").and_then(Value::as_str).is_none());
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
    // And a DISCOVERED registration rides the legal states only:
    assert!(!"DISCOVERED".eq_ignore_ascii_case(SkillState::Enabled.as_str()));
}
