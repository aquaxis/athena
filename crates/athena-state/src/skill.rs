//! Skill lifecycle state machine (spec section 17.3) — the table lives in
//! `athena-types` (`skill_state`), because the state-per-`(id, version)` Registry
//! (`athena-skills`, §32.2's `athena-skills → athena-types` edge) must consult the same
//! single implementation point. This module is the `athena-state` re-export the Core
//! modules use; the table itself is tests-covered in `athena-types`.

pub use athena_types::{SkillTransition, SkillTrigger, skill_transition};
