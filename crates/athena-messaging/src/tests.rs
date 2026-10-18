//! The pure Broker semantics (spec §10; ATH-MSG-010/013/018's message-level rules).

use crate::{Broker, DeliveryRefusal, SendRefusal};
use athena_types::{AgentId, InstanceId, MessageType, Priority};

fn underway(
    thread: &str,
    from_instance: InstanceId,
    to_instance: Option<InstanceId>,
    body: &str,
) -> crate::MessageUnderway {
    crate::MessageUnderway {
        message_id: athena_types::MessageId::new(),
        from: AgentId::core_agent(),
        from_instance,
        to: AgentId::new("rtl-worker"),
        to_instance,
        thread_id: thread.into(),
        priority: Priority::Normal,
        body: body.into(),
        deadline: None,
    }
}

#[test]
fn an_empty_thread_id_is_refused_as_a_shape_violation() {
    // ATH-MSG-013: the thread id is Core Host's issuance; a blank id cannot route.
    let from = InstanceId::new();
    let message = underway("  ", from, Some(InstanceId::new()), "hi");
    let refusal = Broker
        .deliver_check(&message, |_from, _to| true)
        .expect_err("refused");
    match refusal {
        DeliveryRefusal::Envelope(SendRefusal::MissingThread) => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(refusal.class(), "ValidationError");
    assert_eq!(refusal.code(), "ROUTE_MISSING_THREAD");
}

#[test]
fn an_unaddressed_or_self_addressed_message_is_refused() {
    // ATH-MSG-010: addressing is by `to_instance_id`; agent-only addressing is Core
    // Agent's alone, and a self-addressed pair is a route to nowhere.
    let own = InstanceId::new();
    let mut unaddressed = underway("th-1", own.clone(), None, "hi");
    unaddressed.thread_id = "th-1".into();
    let refusal = Broker
        .deliver_check(&unaddressed, |_from, _to| true)
        .expect_err("refused");
    assert!(
        matches!(
            refusal,
            DeliveryRefusal::Envelope(SendRefusal::MissingInstance)
        ),
        "{refusal:?}"
    );

    let selfy = underway("th-1", own.clone(), Some(own.clone()), "hi");
    let refusal = Broker
        .deliver_check(&selfy, |_from, _to| true)
        .expect_err("refused");
    assert!(
        matches!(
            refusal,
            DeliveryRefusal::Envelope(SendRefusal::SelfAddressed)
        ),
        "{refusal:?}"
    );
}

#[test]
fn a_denied_route_carries_the_permission_class() {
    // ATH-MSG-018: sibling pairs outside the ACL refuse at the route check
    // (PermissionError), before the store is touched.
    let from = InstanceId::new();
    let to = InstanceId::new();
    let message = underway("th-1", from, Some(to), "route through?");
    let refusal = Broker
        .deliver_check(&message, |_from, _to| false)
        .expect_err("refused");
    assert_eq!(refusal.class(), "PermissionError");
    assert_eq!(refusal.code(), "ROUTE_SIBLING_DENIED");
}

#[test]
fn the_undroppable_kinds_carry_the_message_contract() {
    // ATH-MSG-012: report/complete/error/control are the never-dropped kinds; a Broker's
    // back-pressure does not apply to them. Instruction/question/status ARE
    // back-pressurable (ATH-MSG-009).
    let droppable = [
        MessageType::Instruction,
        MessageType::Question,
        MessageType::Status,
    ];
    for kind in droppable {
        assert!(
            !crate::broker::is_undroppable_of(&kind),
            "{kind:?} is back-pressurable"
        );
    }
    let kept = [
        MessageType::Report,
        MessageType::Complete,
        MessageType::Error,
        MessageType::Cancel,
        MessageType::Interrupt,
        MessageType::ControlResponse,
        MessageType::ApprovalRequest,
        MessageType::ControlRequest,
        MessageType::Answer,
    ];
    for kind in kept {
        // ATH-MSG-012's list names report/complete/error/control exactly.
        match kind {
            MessageType::Report
            | MessageType::Complete
            | MessageType::Error
            | MessageType::Cancel
            | MessageType::Interrupt
            | MessageType::ControlResponse => {
                assert!(
                    crate::broker::is_undroppable_of(&kind),
                    "{kind:?} must never drop"
                );
            }
            _ => {
                assert!(
                    !crate::broker::is_undroppable_of(&kind),
                    "{kind:?} is NOT in ATH-MSG-012's never-drop set"
                );
            }
        }
    }
}
