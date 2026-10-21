//! `GET /api/v1/metrics` (ATH-OBS-007): the §30.3 catalog rendered in Prometheus text.
//!
//! The values come from *observations the caller pushes* (`Metrics::observe`), the
//! families and labels from `athena-core::observability` (the §30.3 catalog is what keeps
//! a name drift silent-out). The registry holds the values the orchestrator's pollers
//! write; the endpoint only renders.

use athena_core::observability::{MetricFamily, all_families};
use std::collections::BTreeMap;

/// One value in the registry: a family with label values and its current reading.
type LabelSet = BTreeMap<String, String>;

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
                // A family with no observed rows renders its registry zero, as the
                // Prometheus convention wants a type line to stand beside a value.
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
    let requested = family.labels();
    let mut parts = Vec::new();
    for label in requested {
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

/// Render every family of one store snapshot.
///
/// # Errors
/// Store read failures surface whole; the caller answers 500.
pub fn render_from(
    store: &athena_core::AthenaStore,
) -> Result<String, athena_core::PersistenceError> {
    let mut registry = Metrics::new();
    let tasks = athena_core::TaskRepository::list_all(store)?;
    let mut per_state: BTreeMap<String, u64> = BTreeMap::new();
    for task in &tasks {
        *per_state.entry(task.state.to_string()).or_default() += 1;
    }
    for (state, count) in per_state {
        registry.observe(
            MetricFamily::TasksTotal,
            [("state".into(), state)].into_iter().collect(),
            count as f64,
        );
    }
    let running = tasks
        .iter()
        .filter(|task| {
            task.state == athena_types::TaskState::Queued
                || task.state == athena_types::TaskState::Running
        })
        .count();
    registry.observe(MetricFamily::TasksRunning, BTreeMap::new(), running as f64);

    let mut labels = BTreeMap::new();
    labels.insert("state".to_string(), "running".to_string());
    registry.observe(MetricFamily::InstancesWaiting, labels.clone(), {
        tasks
            .iter()
            .filter(|task| task.state == athena_types::TaskState::Running)
            .count() as f64
    });

    // Every declared family must be present, at zero when unobserved.
    let text = registry.render();
    if !text.is_empty() {
        // `render` already emits `# TYPE` lines for unobserved families at 0, so the
        // wire format is the whole §30.3 surface, not just the observed slice.
    }
    Ok(text)
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod tests_spec;
