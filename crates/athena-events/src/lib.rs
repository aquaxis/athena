//! Typed Events (spec section 23).
//!
//! Every Event is an envelope (§23.1, ATH-EVENT-001) carrying a payload whose mandatory
//! items are fixed per Event type (§23.2, ATH-EVENT-017). **[design decision D-3]** Each
//! Event type is a distinct struct whose required payload fields are non-`Option` members,
//! so an Event that would lack a mandatory item cannot be built at all — the §23.2 table is
//! enforced by the type system, with [`TypedEvent::new`] verifying the serialized keys as a
//! runtime backstop.
//!
//! This crate owns definition only. It never appends: Events are persisted by
//! `athena-persistence::EventRepository` inside the transaction that also performs the
//! state transition (ATH-PERS-003), and the store assigns `sequence` (ATH-EVENT-003), so
//! no edge to `athena-persistence` exists or is permitted (spec section 32.2).
//!
//! ## Payload policy (ATH-EVENT-007, R-SEC-10, R-POL-13)
//!
//! Payloads carry no secrets, no credentials, and no full agent input; bulk content is
//! referenced (Result ID, AI_LOG reference, digest). Construction goes through
//! [`TypedEvent::new`], which refuses payloads carrying banned keys (the keys that would
//! embed instruction / prompt / body text verbatim) and masks known secret shapes in every
//! string value before serialization (module `redact`).
//!
//! Event types with an empty payload carry nothing beyond the envelope's references: the
//! §23.2 table fixes a *minimum* for the listed types (ATH-EVENT-017), and adding a payload
//! item to a catalog entry is a specification revision first (ATH-EVENT-009). Where a field
//! here goes beyond or falls short of the minimum, the reason is documented on the struct.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod envelope;
pub mod events;
pub mod redact;

pub use envelope::{EventEnvelope, EventKind, SCHEMA_VERSION, SEQUENCE_UNASSIGNED};
pub use events::{
    EventContext, HierarchyNodeRef, Payload, Result as EventResult, SkillRef, TypedEvent, WaitOn,
};
pub use redact::{EventError, Result as PayloadResult, digest};
