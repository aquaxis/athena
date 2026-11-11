//! The §22.9 read-key grammar (X-38), one place for both list faces.
//!
//! The spec's Event rows name their wire keys:
//! `GET /api/v1/events` takes `?task_id=&type=&since=&limit=&cursor=` and
//! `GET /api/v1/events/stream` takes `?task_id=&types=`. Before this face the
//! handlers read only `Last-Event-ID` (stream) and `?task_id=` (list) — the
//! narrowing keys went unread (the session-030 filing: X-31's unfinished half).
//!
//! The parsed filter rides the D-35 lineage (the module owns the rule, the
//! caller owns its own clock/IO): parsing answers a value, matching answers
//! the predicate, and neither touches the store.

use athena_persistence::StoredEvent;
use athena_types::TaskId;

/// What the caller asked the Events rows to be narrowed by (§22.9's query keys).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EventFilter {
    /// `?task_id=` — one Task's replay only.
    pub task: Option<TaskId>,
    /// `?types=` / `?type=` — the wire type names to keep (empty = any type).
    pub types: Vec<String>,
    /// `?since=` — the sequence anchor (`ATH-EVENT-020`'s gap face): rows with
    /// `sequence > since` only.
    pub since: Option<u64>,
}

impl EventFilter {
    /// Parse the §22.9 keys out of the wire pairs (an absent key is an absent
    /// taste — no value is invented; a `task_id` that fails the newtype parse
    /// stays `None` and the API face owns its 400).
    #[must_use]
    pub fn parse(pairs: &[(String, String)]) -> Self {
        let mut filter = Self::default();
        for (key, value) in pairs {
            match key.as_str() {
                "task_id" => filter.task = TaskId::parse(value),
                "type" | "types" => {
                    for name in value.split(',').map(str::trim).filter(|n| !n.is_empty()) {
                        filter.types.push(name.to_string());
                    }
                }
                "since" => filter.since = value.parse::<u64>().ok(),
                _ => {}
            }
        }
        filter
    }

    /// Wire-truth check: does this Event row answer this filter?
    #[must_use]
    pub fn matches(&self, event: &StoredEvent) -> bool {
        if let Some(task) = &self.task
            && event.task_id.as_ref() != Some(task)
        {
            return false;
        }
        if !self.types.is_empty() && !self.types.contains(&event.event_type) {
            return false;
        }
        match self.since {
            Some(since) => event.sequence > since,
            None => true,
        }
    }
}
