//! Policy Manager — risk classification and permission checks (spec §29).
//!
//! First-match evaluation over `policy.rules`, unmatched ⇒ `high` (ATH-CFG-009). Risk
//! classification never reads an agent's `reason` / `impact` prose (ATH-CORE-038); the
//! canonical descriptor it produces is what the human actually approves (ATH-HITL-010).
//!
//! Emits `PrivilegedRequestDenied`; owns no business entity of its own — it is the
//! enforcement point every privileged request passes through (ATH-CORE-026).

use crate::ownership::ModuleRole;

/// The module's role marker; it owns no business entity.
pub const ROLE: ModuleRole = ModuleRole::PolicyManager;
