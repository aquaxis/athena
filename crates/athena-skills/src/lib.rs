//! `athena-skills` — the Skill Manager subsystem (spec §12, design §9).
//!
//! Phase 5 starts here: the Skill **Package** model — the two files every package
//! (`ATH-SKILL-003`) must carry, `SKILL.md` (the material an Agent reads) and
//! `manifest.yaml` (the machine-managed metadata) — and the **Catalog** record built
//! from a manifest, carrying all fifteen §12.3 fields exactly (`ATH-SKILL-005`).
//!
//! Everything downstream (Validation P5-02, Registry / lifecycle P5-03/04, Resolver
//! P5-06, Loader P5-07) sits on top of these two shapes.

pub mod delivery;
pub mod freeze;
pub mod hot_reload;
pub mod loader;
#[cfg(test)]
pub(crate) mod loader_tests;
pub mod manifest;
pub mod package;
pub mod precedence;
pub mod registry;
pub mod resolver;
pub mod selection;
pub mod skill_use;
pub mod validator;
