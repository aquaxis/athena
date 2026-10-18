//! Hierarchy Manager — composed hierarchies (spec §8, design §3.3).
//!
//! Owns the tree: unique parentage, acyclic (ATH-HIER-002 / 003), level ↔ work-unit
//! mapping (ATH-HIER-012), sibling ACL recording at composition (ATH-MSG-018). Composition
//! happens on a Core Agent decision and is recorded as `HierarchyComposed`
//! (ATH-HIER-009) — Core never invents the tree.
//!
//! Owned entities (`OWNED`): Hierarchy. Emitted Events: `HierarchyComposed`.

use crate::ownership::{Entity, ModuleRole};

/// The entities this module may mutate.
use athena_types::AgentId;

/// The Hierarchy Manager's owned-entity slice (§26.2).
pub const OWNED: &[(Entity, ModuleRole)] = &[(Entity::Hierarchy, ModuleRole::HierarchyManager)];

/// Admission control at `compose_hierarchy` (ATH-SCHED-011): the composition's peak
/// *simultaneously-working* instance count against the configured ceiling.
///
/// Counting rule (the same basis as ATH-SCHED-010): a superior waiting on subordinates is
/// not counted — the working set per superior is its **direct children** (the ones doing
/// the work while the superior waits). `deck` is `scheduler.max_instances_per_task`; a
/// composition whose peak exceeds it refuses as `PolicyError` (ATH-SCHED-011) before
/// anything is persisted — a deep hierarchy would otherwise be unconstructible at the
/// default (1 : N, ATH-HIER-013).
///
/// `children` carries each node's direct-children count (the composition model counts
/// them truthfully — no Scheduler judgement involved, ATH-SCHED-007).
#[must_use]
pub fn peak_working(children: &[(AgentId, u64)], ceiling: u64) -> Option<u64> {
    let peak = children.iter().map(|(_, count)| *count).max().unwrap_or(0);
    (peak > ceiling).then_some(peak)
}

#[cfg(test)]
#[path = "hierarchy_manager_ad_tests.rs"]
mod ad_tests;
