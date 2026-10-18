//! The Message Broker (spec §10, design §6) — Core Host's routes, never Agent's
//! (ATH-MSG-001).
//!
//! Module `broker` carries the delivery semantics: the Core-issued `thread_id`
//! (ATH-MSG-013), persistence-before-ack (ATH-MSG-012), dedup on
//! `(to_instance_id, message_id)` (ATH-MSG-014), hierarchy + sibling-ACL routing
//! (ATH-MSG-018), control-message jump-the-queue (ATH-MSG-017), and the
//! never-dropped guarantee for report/complete/error/control (ATH-MSG-012).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod aggregation;
pub mod backpressure;
pub mod broker;
pub mod deadline;
pub mod envelope;
pub mod route;

pub use aggregation::{
    AggregateReport, AggregationBreach, ReportDigest, VerbatimRelay, verbatim_relays,
};
pub use backpressure::{BackPressure, QueueFull, decide, is_back_pressurable};
pub use broker::{Broker, Delivery, DeliveryRefusal, is_undroppable_of, route_check};
pub use deadline::{
    DeadlineFault, TimeoutPayload, WaitRecord, carries_deadline, expired_at, receiver_is_core_agent,
};
pub use envelope::{MessageUnderway, SendRefusal};
pub use route::{RouteNode, RouteRefusal, RouteWorld, by_agent, route_denial};

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[cfg(test)]
#[path = "phase2_tests.rs"]
mod phase2_tests;

#[cfg(test)]
#[path = "route_tests.rs"]
mod route_tests;
