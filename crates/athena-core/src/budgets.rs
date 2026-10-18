//! Iteration and time budgets (P1-35; ATH-CORE-012, ATH-CORE-013, ATH-OBS-012).
//!
//! Three signals, all per Task, all *observations of the loop the Core Agent runs* —
//! never decisions:
//!
//! - **Iteration ceiling** (`core.max_iterations`, ATH-CORE-013): the loop escalation to a
//!   human when exhausted. Exceeding the ceiling is an escalation *event*, not a Core-side
//!   takeover — ATHENA stops and waits for a human.
//! - **Time budget** (`core.iteration_timeout`): the wall-clock ceiling per iteration.
//! - **No-progress** (ATH-CORE-012): every loop iteration must produce at least one
//!   observable change — a plan update, a delegation, a tool execution, an approval
//!   request, a completion. An iteration that produced no accepted, effective
//!   `CoreRequest` increments the no-progress counter whose persistence *feeds*
//!   [`crate::scheduler`]'s `TaskStalled` inputs (ATH-OBS-012: `stall_seconds` measured
//!   since the last state-change Event).
//!
//! The module is bookkeeping: the caller constructs the budgets from the Configuration
//! section, reports a completed iteration with (or without) an observable change, and
//! receives what §7.5.2's escalation path refuses to do — Core Host does not decide, it
//! reports the exceeded line and waits.

use athena_config::schema::CoreSection;
use athena_types::TaskId;

/// The budgets one Task's loop is running under.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Budgets {
    /// The iteration ceiling (ATH-CORE-013).
    pub max_iterations: u64,
    /// The per-iteration wall clock (ATH-CORE-013's time budget).
    pub iteration_timeout_ms: u64,
}

impl Budgets {
    /// From the configuration (ATH-SCHED-015-style: configurable, no hard-coded ceilings).
    #[must_use]
    pub fn from_config(section: &CoreSection) -> Self {
        Self {
            max_iterations: section.max_iterations,
            iteration_timeout_ms: u64::try_from(section.iteration_timeout.as_millis())
                .unwrap_or(u64::MAX),
        }
    }
}

/// What one completed iteration reported and what the budget concludes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IterationEffect {
    /// At least one accepted `CoreRequest` produced its effect (a plan update, a
    /// delegation, a tool execution, an approval request, a completion).
    Observable,
    /// Nothing took effect this iteration.
    None,
}

/// The budget's per-iteration signals, as the tasks the escalation path is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetSignal {
    /// The iteration count reached the ceiling (ATH-CORE-013): the loop escalates to a
    /// human.
    IterationLimitExceeded {
        /// The iteration count when it hit.
        count: u64,
    },
    /// The iteration's wall clock exceeded the time budget.
    IterationTimeoutExceeded {
        /// The measured wall clock.
        elapsed_ms: u64,
        /// The budget.
        budget_ms: u64,
    },
    /// `n` consecutive iterations produced no observable change; the caller reports this
    /// into the observability layer's `TaskStalled` materialization (ATH-OBS-012).
    NoProgress {
        /// Consecutive no-effect iterations.
        consecutive: u64,
    },
}

/// The per-Task bookkeeping.
#[derive(Debug, Clone, Default)]
pub struct BudgetLedger {
    ledger: std::collections::HashMap<TaskId, TaskLedger>,
}

/// One Task's running budget state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TaskLedger {
    /// Completed iterations so far.
    pub iterations: u64,
    /// Consecutive iterations with no observable change (ATH-CORE-012).
    pub consecutive_without_change: u64,
}

impl BudgetLedger {
    /// Record one iteration's effect, returning the signals it produced.
    ///
    /// A resting `no-progress` counter resets on any observable change: the loop is
    /// progress-bounded, not run-bounded.
    pub fn record_iteration(
        &mut self,
        task: &TaskId,
        effect: IterationEffect,
        budgets: &Budgets,
    ) -> Vec<BudgetSignal> {
        let ledger = self.ledger.entry(task.clone()).or_default();
        ledger.iterations += 1;
        match effect {
            IterationEffect::Observable => ledger.consecutive_without_change = 0,
            IterationEffect::None => ledger.consecutive_without_change += 1,
        }

        let mut signals = Vec::new();
        if ledger.iterations >= budgets.max_iterations {
            signals.push(BudgetSignal::IterationLimitExceeded {
                count: ledger.iterations,
            });
        }
        if ledger.consecutive_without_change > 0 {
            signals.push(BudgetSignal::NoProgress {
                consecutive: ledger.consecutive_without_change,
            });
        }
        signals
    }

    /// Record a measured iteration time, reporting the timeout signal when it passed.
    #[must_use]
    pub fn record_elapsed(
        &mut self,
        task: &TaskId,
        elapsed_ms: u64,
        budgets: &Budgets,
    ) -> Option<BudgetSignal> {
        let _ = task;
        if elapsed_ms > budgets.iteration_timeout_ms {
            Some(BudgetSignal::IterationTimeoutExceeded {
                elapsed_ms,
                budget_ms: budgets.iteration_timeout_ms,
            })
        } else {
            None
        }
    }

    /// The Task's running counters.
    #[must_use]
    pub fn ledger_of(&self, task: &TaskId) -> TaskLedger {
        *self.ledger.get(task).unwrap_or(&TaskLedger::default())
    }

    /// A completed Task's ledger is released: the next Task starts fresh.
    pub fn release(&mut self, task: &TaskId) {
        self.ledger.remove(task);
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    fn budgets() -> Budgets {
        Budgets::from_config(&CoreSection::default())
    }

    #[test]
    fn the_iteration_ceiling_signals_escalation_at_its_value() {
        // ATH-CORE-013 / R-CORE-06: reaching the ceiling escalates to a human (the default
        // 50 is the config's stated value, not a hard-coded fork).
        let owned_budgets = budgets();
        let task = TaskId::new();
        let mut ledger = BudgetLedger::default();
        let mut escalated = Option::<BudgetSignal>::None;
        for iteration in 1..=50_u64 {
            let signals =
                ledger.record_iteration(&task, IterationEffect::Observable, &owned_budgets);
            if let Some(signal) = signals
                .iter()
                .find(|signal| matches!(signal, BudgetSignal::IterationLimitExceeded { .. }))
            {
                assert_eq!(iteration, owned_budgets.max_iterations);
                escalated = Some(*signal);
            }
        }
        assert_eq!(
            escalated,
            Some(BudgetSignal::IterationLimitExceeded { count: 50 })
        );
        // The ledger exceeds further only by recording again (the loop should have
        // escalated); the signal keeps repeating, and the escalation is the caller's.
        let again = ledger.record_iteration(&task, IterationEffect::Observable, &owned_budgets);
        assert!(
            again
                .iter()
                .any(|signal| matches!(signal, BudgetSignal::IterationLimitExceeded { count: 51 }))
        );
    }

    #[test]
    fn a_configured_ceiling_is_honoured() {
        // ATH-SCHED-015-alike: every ceiling is configuration; a Task with a 3-iteration
        // budget escalates at 3.
        let owned = Budgets {
            max_iterations: 3,
            iteration_timeout_ms: 600_000,
        };
        let task = TaskId::new();
        let mut ledger = BudgetLedger::default();
        for _ in 1..3 {
            ledger.record_iteration(&task, IterationEffect::Observable, &owned);
        }
        let signals = ledger.record_iteration(&task, IterationEffect::Observable, &owned);
        assert!(
            signals
                .iter()
                .any(|signal| matches!(signal, BudgetSignal::IterationLimitExceeded { count: 3 }))
        );
    }

    #[test]
    fn no_progress_keeps_counting_until_an_effect_arrives() {
        // ATH-CORE-012: an iteration with no accepted, effective CoreRequest increments
        // the counter only, and any observable change resets it. The counter feeds the
        // `TaskStalled` materialization (ATH-OBS-012), never a decision by Core Host.
        let owned_budgets = budgets();
        let task = TaskId::new();
        let mut ledger = BudgetLedger::default();
        let mut counts = Vec::new();
        for _ in 0..4 {
            let signals = ledger.record_iteration(&task, IterationEffect::None, &owned_budgets);
            counts.push(signals.iter().any(
                |signal| matches!(signal, BudgetSignal::NoProgress { consecutive: 4 } if false),
            ));
            let last = ledger.ledger_of(&task).consecutive_without_change;
            assert_eq!(last, counts.len() as u64);
        }
        assert_eq!(ledger.ledger_of(&task).consecutive_without_change, 4);
        let signals = ledger.record_iteration(&task, IterationEffect::Observable, &owned_budgets);
        assert_eq!(
            ledger.ledger_of(&task).consecutive_without_change,
            0,
            "any observable change resets the no-progress counter"
        );
        assert!(
            !signals
                .iter()
                .any(|signal| matches!(signal, BudgetSignal::NoProgress { .. }))
        );
        let _ = counts;
    }

    #[test]
    fn the_time_budget_reports_the_passed_wall_clock() {
        let owned_budgets = Budgets {
            max_iterations: 50,
            iteration_timeout_ms: 30 * 60 * 1000,
        };
        let task = TaskId::new();
        let mut ledger = BudgetLedger::default();
        assert_eq!(
            ledger.record_elapsed(&task, 30 * 60 * 1000, &owned_budgets),
            None,
            "the budget line itself passes"
        );
        assert_eq!(
            ledger.record_elapsed(&task, 30 * 60 * 1000 + 1, &owned_budgets),
            Some(BudgetSignal::IterationTimeoutExceeded {
                elapsed_ms: 30 * 60 * 1000 + 1,
                budget_ms: 30 * 60 * 1000,
            })
        );
    }

    #[test]
    fn a_released_task_leds_recovers_the_budget_state() {
        // Terminal Task bookkeeping: the ledger is dropped, the next Task starts fresh.
        let owned_budgets = budgets();
        let task = TaskId::new();
        let mut ledger = BudgetLedger::default();
        ledger.record_iteration(&task, IterationEffect::None, &owned_budgets);
        ledger.release(&task);
        let fresh = ledger.ledger_of(&task);
        // Dropped rows read back as the default ledger (fresh Task, fresh counters).
        assert_eq!(fresh, TaskLedger::default());
    }
}
