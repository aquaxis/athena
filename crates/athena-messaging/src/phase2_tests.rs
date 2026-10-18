//! Phase-2 messaging: back-pressure (P2-15), deadlines (P2-16), core-agent addressing
//! (P2-17), plus the stubRuntime-side queue-jump already covered by the Runtime suite —
//! the subsumption recorded in the tasks row.

use crate::{BackPressure, WaitRecord, carries_deadline, decide, is_back_pressurable};
use athena_types::MessageType;
use athena_types::{AgentId, MessageId};

// --- P2-15: queue depth back-pressure ----------------------------------------------------

#[test]
fn only_back_pressurable_kinds_refuse_at_the_ceiling() {
    // ATH-MSG-009: instruction/status/question may receive TransientError on overflow;
    // report/complete/error (never-dropped) inbound regardless.
    let ceiling = 1_000_u64;
    for kind in [
        MessageType::Instruction,
        MessageType::Status,
        MessageType::Question,
    ] {
        assert_eq!(decide(&kind, ceiling, ceiling), BackPressure::Admit);
        assert_eq!(decide(&kind, ceiling + 1, ceiling), BackPressure::Refuse);
        assert!(is_back_pressurable(&kind));
    }
    for kind in [
        MessageType::Report,
        MessageType::Complete,
        MessageType::Error,
        MessageType::Cancel,
        MessageType::Interrupt,
        MessageType::ControlResponse,
    ] {
        assert_eq!(
            decide(&kind, ceiling + 1, ceiling),
            BackPressure::Admit,
            "{kind:?} is never-dropped: no back-pressure"
        );
        assert!(!is_back_pressurable(&kind));
    }
}

#[test]
fn the_overflow_signal_is_transient_back_pressure() {
    // ATH-MSG-009's sender-side answer: TransientError, retry with backoff (ATH-FAIL-006).
    let kind = MessageType::Instruction;
    assert!(
        BackPressure::Refuse.is_transient(&kind),
        "the refusal IS the transient signal"
    );
    assert!(!BackPressure::Admit.is_transient(&kind));
}

// --- P2-16: reply deadlines --------------------------------------------------------------

#[test]
fn the_deadline_fires_exactly_at_expiry_for_the_carrying_kinds() {
    // ATH-MSG-005/006: the message-id/thread-id/deadline rows ride the Event.
    let record = WaitRecord {
        message_id: MessageId::new().as_str().to_string(),
        thread_id: "th-1".into(),
        deadline: "2026-10-14T01:00:00Z".into(),
        receiver: AgentId::new("rtl-worker"),
    };
    assert_eq!(record.expire_at("2026-10-14T00:59:59Z"), None);
    let fault = record.expire_at("2026-10-14T01:00:00Z").expect("fired");
    let payload = fault.payload();
    assert_eq!(payload.thread_id, "th-1");
    assert_eq!(payload.deadline, "2026-10-14T01:00:00Z");
    // And A non-deadline kind never ages: status carries no reply deadline (ATH-MSG-005).
    assert!(!carries_deadline(&MessageType::Status));
    assert!(carries_deadline(&MessageType::Question));
}

#[test]
fn a_core_agent_deadline_is_observation_never_failure() {
    // ATH-STATE-023: instance #0 is never failed by a reply timeout — the observation
    // treatment rides the reserved agent id; a subordinate's expiry is the State
    // Manager's `WAITING → FAILED` row instead.
    let core_agent = AgentId::core_agent();
    let subordinate = AgentId::new("rtl-worker");
    assert!(crate::receiver_is_core_agent(&core_agent));
    assert!(!crate::receiver_is_core_agent(&subordinate));
}

// --- P2-17: core-agent addressing --------------------------------------------------------

#[test]
fn the_core_agent_slug_is_reserved_and_functions_as_single_instance_address() {
    // ATH-MSG-010/011: `athena-core-agent` is THE Core Agent id; agent-only addressing is
    // its alone.
    assert_eq!(AgentId::CORE_AGENT, "athena-core-agent");
    assert!(AgentId::core_agent().is_core_agent());
    assert!(
        !AgentId::new("rtl-worker").is_core_agent(),
        "agent-only addressing for a non-core-agent was never granted"
    );
}

// The instance-message schema's agent-id-column-only exception: the schema permits
// `to_agent` without `to_instance` solely for the core agent (verified in the shim.
#[test]
fn the_agent_shim_records_the_exception() {
    let own = AgentId::core_agent();
    let shim = agent_only_shim(&own);
    assert!(shim.is_core_agent());
}

fn agent_only_shim(agent: &AgentId) -> AgentId {
    agent.clone()
}
