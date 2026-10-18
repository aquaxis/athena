//! Event Manager — `events` append and SSE fan-out (design §3.3).
//!
//! It *is* the emitter: every other module's Event reaches the store and the stream through
//! here, always after the persisting transaction commits (ATH-EVENT-020) and never dropped
//! for lack of a subscriber. Owned entities: none — the Event Manager is defined by being
//! the sole writer of the `events` table (append-only, ATH-PERS-008).
//!
//! Owned entities (`OWNED`): none. Emits: everything, on behalf of its callers.

use crate::ownership::ModuleRole;

/// The module's role marker; it owns no business entity.
pub const ROLE: ModuleRole = ModuleRole::EventManager;
