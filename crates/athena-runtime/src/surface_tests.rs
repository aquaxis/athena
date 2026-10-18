//! Tests for the Runtime layer's declarative faces: the isolation-boundary ordering,
//! the live-status routing rule, the capability declaration (round trip), and the
//! registry's duplicate/absence refusals (ATH-RT-005, ATH-RT-007, ATH-RT-008).

use crate::registry::RuntimeRegistry;
use crate::status::{IsolationBoundary, RuntimeCapabilities, RuntimeInstanceStatus};
use crate::{Runtime, StubConfig, StubScript, stub::StubRuntime};
use athena_types::{RuntimeType, ToolName};
use std::sync::Arc;

#[test]
fn the_isolation_boundary_ordering_is_lattice_shaped() {
    // Process ⊂ Container ⊂ Remote: a boundary is at least as strong as itself and any
    // weaker one — precisely the upgrade-reasoning face R-SEC-11 names.
    for boundary in [
        IsolationBoundary::Process,
        IsolationBoundary::Container,
        IsolationBoundary::Remote,
    ] {
        assert!(boundary.at_least_as_strong_as(boundary));
    }
    assert!(IsolationBoundary::Container.at_least_as_strong_as(IsolationBoundary::Process));
    assert!(IsolationBoundary::Remote.at_least_as_strong_as(IsolationBoundary::Container));
    assert!(!IsolationBoundary::Process.at_least_as_strong_as(IsolationBoundary::Remote));
    assert!(!IsolationBoundary::Container.at_least_as_strong_as(IsolationBoundary::Remote));
}

#[test]
fn the_live_status_face_admits_exactly_the_message_routable_states() {
    for status in [
        RuntimeInstanceStatus::Starting,
        RuntimeInstanceStatus::Running,
        RuntimeInstanceStatus::Waiting,
        RuntimeInstanceStatus::Suspended,
    ] {
        assert!(status.is_alive(), "{status:?}");
    }
    assert!(!RuntimeInstanceStatus::Stopped { exit_code: Some(0) }.is_alive());
    assert!(
        !RuntimeInstanceStatus::Failed {
            detail: "supervision contract broken".into(),
        }
        .is_alive()
    );
}

#[test]
fn the_capability_declaration_round_trips_through_the_wire() {
    let capabilities = RuntimeCapabilities {
        runtime_type: RuntimeType::AgentCli,
        isolation: IsolationBoundary::Process,
        supported_tools: vec![ToolName::FsRead, ToolName::FsWrite],
        max_concurrent_instances: 16,
    };
    let wire = serde_json::to_string(&capabilities).expect("serialized");
    let back: RuntimeCapabilities = serde_json::from_str(&wire).expect("decoded");
    assert_eq!(back, capabilities);
}

#[test]
fn the_registry_refuses_a_shadowing_kind_and_names_an_absence() {
    let mut registry = RuntimeRegistry::new();
    registry
        .register(Arc::new(StubRuntime::new(
            StubConfig::default(),
            StubScript::default(),
        )) as Arc<dyn Runtime>)
        .expect("first registration of the kind");
    // A second Adapter for one kind would make the registry silently choose — refused.
    #[allow(clippy::err_expect, reason = "expect_err requires a Debug Ok type")]
    let refusal = registry
        .register(Arc::new(StubRuntime::new(
            StubConfig::default(),
            StubScript::default(),
        )) as Arc<dyn Runtime>)
        .err()
        .expect("duplicate kind refuses, not an Ok registration");
    assert!(
        refusal.to_string().contains("already registered"),
        "{refusal}"
    );

    // An absent kind is a `Unsupported` fact the caller learns from — never a fallback.
    let absent = registry
        .require(&RuntimeType::Docker)
        .err()
        .expect("nothing registered for Docker");
    assert!(
        absent.to_string().contains("no runtime registered"),
        "{absent}"
    );
    assert!(
        registry.get(&RuntimeType::Docker).is_none(),
        "an unsupported kind invents nothing"
    );
}
