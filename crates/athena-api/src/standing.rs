//! The Core Agent's standing (X-29): the facts the supervision poll observes about
//! instance #0, published where `GET /api/v1/status` can read them.
//!
//! The facts-only discipline (`ATH-CORE-038`) governs: the cell carries only what the
//! supervision poll itself observed — the liveness verdict it already computed, the
//! restarts it performed, and the restart-window facts it owns. Nothing is inferred
//! elsewhere, and a standing the poll has not yet observed is `None`
//! (`CoreAgentStanding::UNOBSERVED` renders absence, never a fabricated zero).

use std::sync::{Arc, Mutex};

/// The Core Agent's standing, as the supervision poll last observed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreAgentStanding {
    /// Whether the last poll found instance #0 alive.
    pub alive: bool,
    /// How many restarts the Core Agent Session has performed (the window's counted
    /// attempts, ATH-CORE-045).
    pub restarts: u64,
    /// The restart budget inside the window (3 / h by default).
    pub restart_limit: u64,
    /// The `runtime_instance_id` of the resident instance (stability aid).
    pub runtime_instance_id: String,
    /// The last observation instant, RFC 3339.
    pub observed_at: String,
}

/// The published cell: `None` until the supervision poll's first observation
/// (`CoreAgentStanding::UNOBSERVED` is not a state a caller can read).
pub type StandingCell = Arc<Mutex<Option<CoreAgentStanding>>>;

/// The cell a boot installs before the supervision poll starts.
#[must_use]
pub fn new_cell() -> StandingCell {
    Arc::new(Mutex::new(None))
}

impl CoreAgentStanding {
    /// Publish one poll's observation into the cell (a lock failure is ignored: the
    /// cell is a single-writer publication, and a poisoned lock only means the last
    /// observation stands).
    pub fn publish(cell: &StandingCell, standing: Self) {
        if let Ok(mut guard) = cell.lock() {
            *guard = Some(standing);
        }
    }
}
