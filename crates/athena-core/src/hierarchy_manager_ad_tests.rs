//! The admission-control surface (ATH-SCHED-011, P2-09).

use super::peak_working;
use athena_types::AgentId;

#[test]
fn a_peak_over_the_ceiling_refuses_as_policy() {
    // 1:N hierarchy (ATH-HIER-013): one Lead with five Workers — counted only when the
    // peak exceeds `max_instances_per_task` (default 4).
    let lead = AgentId::new("lead");
    let counts = vec![(lead, 5_u64)];
    let peak = peak_working(&counts, 4).expect("over the ceiling");
    assert_eq!(peak, 5);
}

#[test]
fn the_peak_at_the_ceiling_is_admissible() {
    let lead = AgentId::new("lead");
    let counts = vec![(lead, 4_u64)];
    assert_eq!(peak_working(&counts, 4), None, "a 4-worker Lead constructs");
    // And a composition whose every level holds fewer constructs silently.
    let counts = vec![
        (AgentId::new("lead-a"), 3_u64),
        (AgentId::new("lead-b"), 2_u64),
    ];
    assert_eq!(peak_working(&counts, 4), None);
}
