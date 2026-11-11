//! `GET /api/v1/metrics` (ATH-OBS-007): the §30.3 catalog rendered in Prometheus text.
//!
//! The catalog (names, kinds, label columns) comes from `athena-core::observability` — the
//! §30.3 table is what keeps a name drift silent-out. The values ride two fact lanes (X-40):
//!
//! * the **store snapshot** — one read pass over the Task, instance, message and Event rows
//!   observes every family those rows answer (`ATH-OBS-006`);
//! * the **API tally** — the role layer records every answered `(route, status)` request and
//!   its served millis into the state's shared tally, and the render folds the two
//!   `athena_api_*` families (the unauthenticated health face rides no role layer and stays
//!   untallied).
//!
//! All 23 families of the §30.3 table ride one of the two lanes; what a family cannot answer
//! from recorded facts it does not answer — a row whose payload key or timestamp is
//! unreadable is skipped, never guessed. Histogram families render the recorded cumulative
//! sum: this registry's observation unit is one value per `(family, label set)`.
//!
//! **The zero floor is per family, never per label value** (design D-44): a family no
//! observation reached renders one `<name> 0` row so a scraper's charts never toggle on
//! series presence, while a family holding observations renders exactly the rows observed.
//! The label domains (`error_class`, `skill_id`, `route`, `runtime`, …) are open, so a value
//! the store never answered is *absent*, not zero — enumerating it would be invention.

use athena_core::observability::{MetricFamily, all_families};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// One value in the registry: a family with label values and its current reading.
type LabelSet = BTreeMap<String, String>;

/// The API request tally the role layer shares: `(route, status) -> (count, millis sum)`.
pub type SharedTally = Arc<Mutex<BTreeMap<(String, u16), (u64, u64)>>>;

/// Record one answered request (the role layer owns the request IO; this face owns the
/// tally discipline). A poisoned lock skips the record — a lost tally row never breaks
/// the request that rode it.
pub fn tally_request(tally: &SharedTally, route: &str, status: u16, millis: u64) {
    if let Ok(mut guard) = tally.lock() {
        let entry = guard.entry((route.to_string(), status)).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += millis;
    }
}

/// The metrics registry the caller observes into.
#[derive(Debug, Default)]
pub struct Metrics {
    values: BTreeMap<(MetricFamily, LabelSet), f64>,
}

impl Metrics {
    /// A fresh registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one current value for a family's label set.
    pub fn observe(&mut self, family: MetricFamily, labels: LabelSet, value: f64) {
        self.values.insert((family, labels), value);
    }
    /// The Prometheus text export (`text/plain; version=0.0.4`).
    #[must_use]
    pub fn render(&self) -> String {
        let mut text = String::new();
        for family in all_families() {
            let rows: Vec<(&LabelSet, f64)> = self
                .values
                .iter()
                .filter(|(key, _)| key.0 == family)
                .map(|(key, value)| (&key.1, *value))
                .collect();
            text.push_str(&format!("# TYPE {} {}\n", family.name(), family.kind()));
            if rows.is_empty() {
                // A family with no observed rows renders its type line at zero, so the
                // wire format is the whole §30.3 surface (a scraper's charts never
                // toggle on series presence, ATH-OBS-007).
                text.push_str(&format!("{} 0\n", family.name()));
                continue;
            }
            for (labels, value) in rows {
                let rendered = render_labels(&family, labels);
                text.push_str(&format!("{}{} {}\n", family.name(), rendered, value));
            }
        }
        text
    }
}
/// Label rendering: `{key="value",…}` or empty when no labels.
fn render_labels(family: &MetricFamily, labels: &LabelSet) -> String {
    let mut parts = Vec::new();
    for label in family.labels() {
        if let Some(value) = labels.get(*label) {
            parts.push(format!("{label}=\"{value}\""));
        }
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("{{{}}}", parts.join(","))
    }
}

/// Observe one count-per-label family from a state -> count map.
fn observe_labelled(registry: &mut Metrics, family: MetricFamily, counts: &BTreeMap<String, u64>) {
    for (label, count) in counts {
        registry.observe(
            family,
            [("state".to_string(), label.clone())].into_iter().collect(),
            f64::from(u32::try_from(*count).unwrap_or(u32::MAX)),
        );
    }
}

/// Observe one count-per-label family with an explicit label key (the catalog's label
/// names differ per family: error_class, type, outcome, skill_id, runtime, request_kind).
fn observe_labelled_keyed(
    registry: &mut Metrics,
    family: MetricFamily,
    key: &str,
    counts: &BTreeMap<String, u64>,
) {
    for (label, count) in counts {
        registry.observe(
            family,
            [(key.to_string(), label.clone())].into_iter().collect(),
            f64::from(u32::try_from(*count).unwrap_or(u32::MAX)),
        );
    }
}

/// A histogram family renders one row per label set; this registry's observation unit is
/// one recorded aggregate, so the family's value is the cumulative `sum` counterpart.
fn observe_histogram(registry: &mut Metrics, family: MetricFamily, total_ms: u64) {
    registry.observe(
        family,
        BTreeMap::new(),
        (total_ms / 1000) as f64 + f64::from((total_ms % 1000) as u32) / 1000.0,
    );
}

/// Render every family of one store snapshot plus the shared API tally.
///
/// # Errors
/// Store read failures surface whole; the caller answers 500.
pub fn render_from(
    store: &athena_core::AthenaStore,
    tally: &SharedTally,
    now_ms: u64,
) -> Result<String, athena_core::PersistenceError> {
    let mut registry = Metrics::new();
    let tasks = athena_core::TaskRepository::list_all(store)?;
    let mut per_state: BTreeMap<String, u64> = BTreeMap::new();
    let mut running: u64 = 0;
    let mut oldest_nonterminal_ms: Option<u64> = None;
    for task in &tasks {
        *per_state.entry(task.state.to_string()).or_default() += 1;
        if task.state == athena_types::TaskState::Running {
            running += 1;
        }
        if !task.state.is_terminal()
            && let Some(born) = athena_types::clock::parse_rfc3339_millis(&task.created_at)
        {
            let age = now_ms.saturating_sub(born);
            oldest_nonterminal_ms = Some(oldest_nonterminal_ms.map_or(age, |seen| seen.max(age)));
        }
    }
    observe_labelled(&mut registry, MetricFamily::TasksTotal, &per_state);
    registry.observe(
        MetricFamily::TasksRunning,
        BTreeMap::new(),
        f64::from(u32::try_from(running).unwrap_or(u32::MAX)),
    );
    if let Some(age_ms) = oldest_nonterminal_ms {
        observe_histogram(
            &mut registry,
            MetricFamily::OldestNonterminalTaskAgeSeconds,
            age_ms,
        );
    }
    // One walk over the Event rows folds eleven families; the instances, approvals and
    // messages rows ride their own reads below. Lines whose timestamp is unreadable are
    // skipped (never guessed — never-invent).
    let events = athena_core::EventRepository::list_since(store, 0, u64::MAX)?;
    let mut events_written: u64 = 0;
    let mut failures: BTreeMap<String, u64> = BTreeMap::new();
    let mut messages: BTreeMap<String, u64> = BTreeMap::new();
    let mut replans: u64 = 0;
    let mut retries: BTreeMap<String, u64> = BTreeMap::new();
    let mut approvals: BTreeMap<String, u64> = BTreeMap::new();
    let mut approval_waits_ms: Vec<u64> = Vec::new();
    let mut skill_loads: BTreeMap<String, u64> = BTreeMap::new();
    let mut skill_validation_failures: u64 = 0;
    let mut scheduler_blocked: u64 = 0;
    let mut privileged_denied: BTreeMap<String, u64> = BTreeMap::new();
    let mut restarts: u64 = 0;
    let mut last_event_ms: Option<u64> = None;
    let mut task_durations_ms: BTreeMap<String, u64> = BTreeMap::new();
    let mut started: BTreeMap<String, u64> = BTreeMap::new();
    let mut pending_request_ms: BTreeMap<String, u64> = BTreeMap::new();
    let mut instance_durations_ms: Vec<u64> = Vec::new();
    for event in &events {
        let stamp_ms = athena_types::clock::parse_rfc3339_millis(&event.occurred_at);
        if let Some(stamp) = stamp_ms {
            last_event_ms = Some(last_event_ms.map_or(stamp, |seen| seen.max(stamp)));
        }
        events_written += 1;
        let payload: serde_json::Value =
            serde_json::from_str(&event.payload_json).unwrap_or(serde_json::Value::Null);
        let class = |key: &str| -> Option<String> {
            payload
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        };
        match event.event_type.as_str() {
            "AgentFailed" => {
                if let Some(class) = class("error_class") {
                    *failures.entry(class).or_default() += 1;
                }
            }
            "AgentMessageSent" => {
                if let Some(kind) = class("message_type") {
                    *messages.entry(kind).or_default() += 1;
                }
            }
            "TaskReplanning" => replans += 1,
            "TaskRetrying" => {
                if let Some(class) = class("error_class") {
                    *retries.entry(class).or_default() += 1;
                }
            }
            "SkillLoaded" => {
                if let Some(id) = class("skill_id") {
                    *skill_loads.entry(id).or_default() += 1;
                }
            }
            "SkillValidationFailed" => skill_validation_failures += 1,
            "SchedulerBlocked" => scheduler_blocked += 1,
            "PrivilegedRequestDenied" => {
                if let Some(kind) = class("request_kind") {
                    *privileged_denied.entry(kind).or_default() += 1;
                }
            }
            "CoreAgentRestarted" => restarts += 1,
            "TaskStarted" => {
                if let (Some(task), Some(started_ms)) = (&event.task_id, stamp_ms) {
                    started.insert(task.as_str().to_string(), started_ms);
                }
            }
            "TaskCompleted" | "TaskFailed" => {
                if let (Some(task), Some(end_ms)) = (&event.task_id, stamp_ms)
                    && let Some(born) = started.remove(task.as_str())
                {
                    task_durations_ms
                        .insert(task.as_str().to_string(), end_ms.saturating_sub(born));
                }
            }
            "ApprovalRequested" => {
                if let (Some(id), Some(stamp)) = (class("approval_id"), stamp_ms) {
                    pending_request_ms.insert(id, stamp);
                }
            }
            _ => {}
        }
    }
    // Fold the walk's accumulators into the registry (one row per family and label set).
    registry.observe(
        MetricFamily::EventsWrittenTotal,
        BTreeMap::new(),
        f64::from(u32::try_from(events_written).unwrap_or(u32::MAX)),
    );
    observe_labelled_keyed(
        &mut registry,
        MetricFamily::AgentFailuresTotal,
        "error_class",
        &failures,
    );
    observe_labelled_keyed(
        &mut registry,
        MetricFamily::MessagesTotal,
        "type",
        &messages,
    );
    // The Broker dwell gauge reads the message rows themselves, not the Event chain: an
    // addressed row without an accepted delivery record is dwelling, and an unaddressed row
    // (ATH-MSG-010) answers no delivery key and is excluded by the repository's own
    // never-invent rule rather than assumed.
    let dwelling = athena_core::MessageRepository::count_dwelling(store)?;
    registry.observe(
        MetricFamily::MessageQueueDepth,
        BTreeMap::new(),
        f64::from(u32::try_from(dwelling).unwrap_or(u32::MAX)),
    );
    registry.observe(
        MetricFamily::ReplansTotal,
        BTreeMap::new(),
        f64::from(u32::try_from(replans).unwrap_or(u32::MAX)),
    );
    observe_labelled_keyed(
        &mut registry,
        MetricFamily::RetriesTotal,
        "error_class",
        &retries,
    );
    observe_labelled_keyed(
        &mut registry,
        MetricFamily::SkillLoadsTotal,
        "skill_id",
        &skill_loads,
    );
    registry.observe(
        MetricFamily::SkillValidationFailuresTotal,
        BTreeMap::new(),
        f64::from(u32::try_from(skill_validation_failures).unwrap_or(u32::MAX)),
    );
    registry.observe(
        MetricFamily::SchedulerBlockedTotal,
        BTreeMap::new(),
        f64::from(u32::try_from(scheduler_blocked).unwrap_or(u32::MAX)),
    );
    observe_labelled_keyed(
        &mut registry,
        MetricFamily::PrivilegedRequestsDeniedTotal,
        "request_kind",
        &privileged_denied,
    );

    // The remaining walk-side families: restarts (the catalog's restart counter) and
    // the Core Agent's last-Event age in seconds (the freshness gauge, now-anchored).
    registry.observe(
        MetricFamily::CoreAgentRestartsTotal,
        BTreeMap::new(),
        f64::from(u32::try_from(restarts).unwrap_or(u32::MAX)),
    );
    if let Some(stamp) = last_event_ms {
        let age_ms = now_ms.saturating_sub(stamp);
        registry.observe(
            MetricFamily::CoreAgentLastEventAgeSeconds,
            BTreeMap::new(),
            (age_ms / 1000) as f64,
        );
    }
    // The Task duration histogram: each terminal Task's observed span (the walk
    // paired `TaskStarted` -> `TaskCompleted`/`TaskFailed` inside it), summed per
    // the registry's one-value-per-family observation unit (ATH-OBS-006).
    let total_task_duration_ms: u64 = task_durations_ms.values().sum();
    observe_histogram(
        &mut registry,
        MetricFamily::TaskDurationSeconds,
        total_task_duration_ms,
    );

    // The approval lane: Approval rows are the outcome truth (spec section 17.4);
    // the row itself carries no request time, so the wait pairs each responded row
    // with the request stamp the walk recorded by approval id. The outcome label is
    // the state's wire form.
    for (wire, state) in [
        ("REQUESTED", athena_types::ApprovalState::Requested),
        ("GRANTED", athena_types::ApprovalState::Granted),
        ("REJECTED", athena_types::ApprovalState::Rejected),
        ("EXPIRED", athena_types::ApprovalState::Expired),
        ("CONSUMED", athena_types::ApprovalState::Consumed),
    ] {
        for row in &athena_core::ApprovalRepository::list_by_state(store, state)? {
            *approvals.entry(wire.to_string()).or_default() += 1;
            if let Some(responded) = row.responded_at.as_deref()
                && let Some(responded_ms) = athena_types::clock::parse_rfc3339_millis(responded)
                && let Some(requested_ms) = pending_request_ms.get(row.id.as_str())
            {
                approval_waits_ms.push(responded_ms.saturating_sub(*requested_ms));
            }
        }
    }

    // Fold the approval accumulators the rows lane filled: outcomes per wire state and
    // the recorded waits as the wait histogram's cumulative counterpart.
    observe_labelled_keyed(
        &mut registry,
        MetricFamily::ApprovalsTotal,
        "outcome",
        &approvals,
    );
    let total_approval_wait_ms: u64 = approval_waits_ms.iter().copied().sum();
    observe_histogram(
        &mut registry,
        MetricFamily::ApprovalWaitSeconds,
        total_approval_wait_ms,
    );
    // The instance lane: `AgentInstancesRunning{runtime}` counts non-terminal rows by
    // their definition's runtime kind (a row whose definition is unknown is skipped —
    // never-invent); `AgentInstanceDurationSeconds` sums finished spans (started+finished
    // both readable only). `InstancesWaiting{state}` counts WAITING/SUSPENDED rows
    // (the ATH-SCHED-010 / ATH-STATE-020 exclusion set).
    let definitions = athena_core::AgentRepository::list_definitions(store)?;
    let runtime_of: BTreeMap<String, String> = definitions
        .iter()
        .map(|definition| {
            (
                definition.id.as_str().to_string(),
                definition.runtime.as_str().to_string(),
            )
        })
        .collect();
    let instances = athena_core::AgentRepository::list_all_instances(store)?;
    let mut running_by_runtime: BTreeMap<String, u64> = BTreeMap::new();
    let mut waiting_by_state: BTreeMap<String, u64> = BTreeMap::new();
    for instance in &instances {
        if !instance.state.is_terminal()
            && let Some(runtime) = runtime_of.get(instance.agent_id.as_str())
        {
            *running_by_runtime.entry(runtime.clone()).or_default() += 1;
        }
        if instance.state.is_excluded_from_limits() {
            *waiting_by_state
                .entry(instance.state.as_str().to_string())
                .or_default() += 1;
        }
        if let (Some(started), Some(finished)) = (&instance.started_at, &instance.finished_at)
            && let (Some(s_ms), Some(f_ms)) = (
                athena_types::clock::parse_rfc3339_millis(started),
                athena_types::clock::parse_rfc3339_millis(finished),
            )
        {
            instance_durations_ms.push(f_ms.saturating_sub(s_ms));
        }
    }
    observe_labelled_keyed(
        &mut registry,
        MetricFamily::AgentInstancesRunning,
        "runtime",
        &running_by_runtime,
    );
    observe_labelled_keyed(
        &mut registry,
        MetricFamily::InstancesWaiting,
        "state",
        &waiting_by_state,
    );
    let total_instance_duration_ms: u64 = instance_durations_ms.iter().copied().sum();
    observe_histogram(
        &mut registry,
        MetricFamily::AgentInstanceDurationSeconds,
        total_instance_duration_ms,
    );
    // The API tally lane folds last: `(route, status) -> (count, millis sum)` —
    // `athena_api_requests_total` per pair plus the duration histogram as the recorded
    // cumulative counterpart (ATH-OBS-007's fact lanes; the unauthenticated healthz
    // rides no role layer and stays untallied).
    if let Ok(guard) = tally.lock() {
        let mut tally_millis: u64 = 0;
        for ((route, status), (count, millis)) in guard.iter() {
            registry.observe(
                MetricFamily::ApiRequestsTotal,
                [
                    ("route".to_string(), route.clone()),
                    ("status".to_string(), status.to_string()),
                ]
                .into_iter()
                .collect(),
                f64::from(u32::try_from(*count).unwrap_or(u32::MAX)),
            );
            tally_millis = tally_millis.saturating_add(*millis);
        }
        // The duration histogram is one recorded aggregate across the tally rows —
        // observing it inside the loop would let the last row clobber the single
        // (family, no-label) value instead of rendering the cumulative sum.
        observe_histogram(
            &mut registry,
            MetricFamily::ApiRequestDurationSeconds,
            tally_millis,
        );
    }
    let text = registry.render();
    Ok(text)
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod tests_spec;
