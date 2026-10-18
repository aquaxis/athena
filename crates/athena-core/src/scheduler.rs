//! The Scheduler's dispatch engine (spec §18, design §7).
//!
//! The Scheduler is the manager of execution order, never the decider (ATH-SCHED-007): it
//! holds **PlanStep execution requests** (ATH-SCHED-009), admits only dependency-satisfied
//! steps (ATH-SCHED-001), enforces the §18.3 limits as configuration (ATH-SCHED-003,
//! ATH-SCHED-015 — everything configurable, nothing hard-coded), keeps `WAITING` /
//! `SUSPENDED` instances out of every accounting so a waiting superior cannot hold a slot
//! its own children need (ATH-SCHED-010, hold-and-wait elimination), keeps the Core Agent's
//! slots outside the instance ceiling (ATH-SCHED-008), promotes waiting Tasks against
//! starvation (ATH-SCHED-006), and makes both blocked conditions observable *before* any
//! timeout: a dependency-satisfied step a limit refuses (ATH-SCHED-014), and a Task whose
//! whole non-terminal inventory is waiting while its queue is not empty (ATH-SCHED-012).
//!
//! Deterministic and pure over an inventory snapshot: the caller reads the world from the
//! repositories, the engine decides admissions, and the transitions / Events still go
//! through the State Manager / Event Manager pair (ATH-PERS-003, D-1). The queue length
//! ceiling (ATH-SCHED-004) is applied at *enqueue* time — this engine reports the surplus
//! it cannot hold, for the caller to answer with `TransientError`.
//!
//! Backoff / deadline re-arm (ATH-SCHED-013): the attempt *facts* — step, attempt number,
//! started-at — are persisted by the repository in `plan_step_attempts`; the gate is
//! `started_at + backoff(attempt)`, re-derived from configuration at every boot, so a Core
//! restart re-arms it without an extra store table. [`RetryBudget::ready_at`] is the only
//! derivation point.

use athena_config::schema::{BackoffSection, SchedulerSection};
use athena_types::{
    AgentInstanceState as InstanceState, PlanStepId, PlanStepState, Priority, TaskId,
};

/// The §18.3 limits, all configurable (ATH-SCHED-015: single-host *initial* values, to be
/// settled by a spec revision on Phase-1 measurement — never fixed in code).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchedulerLimits {
    /// Concurrent Tasks in `RUNNING` or later (ATH-SCHED-009).
    pub max_running_tasks: u64,
    /// Concurrent Agent Instances, `WAITING` / `SUSPENDED` excluded from the count
    /// (ATH-SCHED-010).
    pub max_instances: u64,
    /// Concurrent Instances under one Task (ATH-SCHED-009).
    pub max_instances_per_task: u64,
    /// Queue length ceiling; beyond it the *enqueuing* caller answers `TransientError`
    /// (ATH-SCHED-004).
    pub max_queue_length: u64,
    /// Core Agent slots held outside `max_instances` (ATH-SCHED-008); a terminating Core
    /// Agent and its successor overlap inside here.
    pub reserved_core_agent_slots: u64,
    /// Anti-starvation promotion interval (ATH-SCHED-006).
    pub promotion_interval_ms: u64,
}

impl SchedulerLimits {
    /// From the configuration section (every value reassigned there, ATH-SCHED-003).
    #[must_use]
    pub fn from_config(section: &SchedulerSection) -> Self {
        let promotion_interval_ms =
            u64::try_from(section.priority_promotion_interval.as_millis()).unwrap_or(u64::MAX);
        Self {
            max_running_tasks: section.max_concurrent_tasks,
            max_instances: section.max_concurrent_instances,
            max_instances_per_task: section.max_instances_per_task,
            max_queue_length: section.max_queue_length,
            reserved_core_agent_slots: section.reserved_core_agent_slots,
            promotion_interval_ms,
        }
    }
}

/// The retry budget per Task / PlanStep (ATH-FAIL-006; the §18.3 table is the scheduler's).
#[derive(Debug, Clone, PartialEq)]
pub struct RetryBudget {
    /// The configured backoff shape (`initial_ms`, `multiplier`, `max_ms`).
    pub backoff: BackoffSection,
}

impl RetryBudget {
    /// The instant an attempt in backoff becomes re-eligible, derived from the persisted
    /// attempt row (ATH-SCHED-013): the fact is `plan_step_attempts.started_at`, the delay
    /// is configuration, so the Core restart re-arm is this one function.
    #[must_use]
    pub fn ready_at(&self, started_at_ms: u64, attempt: u64) -> u64 {
        let multiplier = if self.backoff.multiplier < 1.0 {
            1.0
        } else {
            self.backoff.multiplier
        };
        let mut delay = self.backoff.initial_ms;
        for _ in 1..attempt {
            let grown = (delay as f64 * multiplier).min(self.backoff.max_ms as f64) as u64;
            delay = grown.max(delay);
        }
        started_at_ms.saturating_add(delay)
    }
}

/// One live instance the caller counted from the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryInstance {
    /// Its Task.
    pub task_id: TaskId,
    /// Its state; `WAITING` / `SUSPENDED` are excluded from every ceiling (ATH-SCHED-010).
    pub state: InstanceState,
}

/// One PlanStep execution request as queued (ATH-SCHED-009).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedStep {
    /// The step's owning Task.
    pub task_id: TaskId,
    /// The step.
    pub step_id: PlanStepId,
    /// The Task's priority at enqueue time.
    pub priority: Priority,
    /// Enqueue order inside its priority — the FIFO tie-breaker (ATH-SCHED-005).
    pub enqueue_order: u64,
    /// Unmet `depends_on` steps with their observed states; satisfaction is *all* `Success`
    /// and nothing else. Failure of a dependency is not a scheduler decision: it stays
    /// queued until Core Agent re-plans (ATH-SCHED-001, ATH-SCHED-007).
    pub dependencies: Vec<(PlanStepId, PlanStepState)>,
    /// Wait time so far, for promotion (ATH-SCHED-006).
    pub waited_ms: u64,
    /// A boot-time gate, derived from the persisted attempt row and the retry budget when
    /// this step is a backoff-waiting attempt (ATH-SCHED-013).
    pub hold_until: Option<u64>,
}

/// The world the engine decides over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inventory {
    /// Every live instance, one row each (ATH-RT-012).
    pub instances: Vec<InventoryInstance>,
    /// Tasks currently `RUNNING` or later — the running-task numerator (ATH-SCHED-009).
    pub running_tasks: Vec<TaskId>,
    /// The queue, enqueued (or previously held) PlanStep execution requests.
    pub queue: Vec<QueuedStep>,
}

/// Which limit refused admission — the `SchedulerBlocked` payload's `blocking_limit`
/// wire value (ATH-SCHED-014).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockingLimit {
    /// `scheduler.max_concurrent_tasks`.
    RunningTasks,
    /// `scheduler.max_concurrent_instances`.
    ConcurrentInstances,
    /// `scheduler.max_instances_per_task`.
    InstancesPerTask,
    /// `scheduler.max_waiting_approval_ratio` (ATH-HITL-016, realised in Phase 4).
    ApprovalWaitCap,
}

impl BlockingLimit {
    /// The wire tag the `SchedulerBlocked` Event payload carries (spec §23.2).
    #[must_use]
    pub const fn wire(&self) -> &'static str {
        match self {
            Self::RunningTasks => "max_concurrent_tasks",
            Self::ConcurrentInstances => "max_concurrent_instances",
            Self::InstancesPerTask => "max_instances_per_task",
            Self::ApprovalWaitCap => "max_waiting_approval_ratio",
        }
    }
}

/// One dispatch cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tick {
    /// Steps admitted in decision order; the caller starts an Instance for each.
    pub admitted: Vec<PlanStepId>,
    /// Dependency-satisfied steps a limit refused **this cycle**: they stay queued and the
    /// caller emits `SchedulerBlocked` (ATH-SCHED-014), never waiting for a timeout.
    pub blocked: Vec<(PlanStepId, BlockingLimit)>,
    /// Queue overflow: the tail entries once held requests exceed the ceiling even after
    /// admissions (ATH-SCHED-004) — the caller answers those with `TransientError`,
    /// and the sender retries with backoff (ATH-FAIL-006).
    pub overflow: Vec<PlanStepId>,
    /// Tasks whose entire non-terminal inventory is `WAITING` / `SUSPENDED` while their
    /// queue is not empty (ATH-SCHED-012): the caller emits `SchedulerBlocked` too —
    /// hold-and-wait is observable without a timeout.
    pub hold_and_wait_tasks: Vec<TaskId>,
}

/// Dispatch the queue against the inventory and limits.
///
/// Order: effective priority (after any promotion) then enqueue order, FIFO inside a
/// priority (ATH-SCHED-005). Steps still inside their backoff window are passed over
/// without any event: their gate is time, not a limit (ATH-SCHED-013).
#[must_use]
pub fn tick(inventory: &Inventory, limits: SchedulerLimits, now_ms: u64) -> Tick {
    let mut result = Tick {
        admitted: Vec::new(),
        blocked: Vec::new(),
        overflow: Vec::new(),
        hold_and_wait_tasks: Vec::new(),
    };

    // Inventory counters, with the ATH-SCHED-010 exclusion applied everywhere.
    let mut running_tasks = inventory.running_tasks.clone();
    let mut user_instances = inventory
        .instances
        .iter()
        .filter(|inst| !excluded(&inst.state))
        .count();
    let mut per_task: std::collections::HashMap<TaskId, u64> = std::collections::HashMap::new();
    for inst in &inventory.instances {
        if excluded(&inst.state) {
            continue;
        }
        *per_task.entry(inst.task_id.clone()).or_insert(0) += 1;
    }

    // Eligible entries, in (effective priority, FIFO) order.
    let mut ordered: Vec<&QueuedStep> = inventory
        .queue
        .iter()
        .filter(|entry| entry.hold_until.is_none_or(|gate| gate <= now_ms))
        .collect();
    ordered.sort_by_key(|entry| {
        (
            effective_rank(entry, limits.promotion_interval_ms),
            entry.enqueue_order,
        )
    });

    let mut admitted_len = result.admitted.len();
    for entry in &ordered {
        // ATH-SCHED-001: genuinely unsatisfied dependencies keep the step queued. That is
        // not a blocked fact: the planner owns the fix (ATH-SCHED-007).
        let satisfied = entry
            .dependencies
            .iter()
            .all(|(_, state)| *state == PlanStepState::Success);
        if !satisfied {
            continue;
        }

        // The Task of this step is already occupying a running-task slot, or needs one.
        let task_running = running_tasks.iter().any(|task| task == &entry.task_id);
        if !task_running && running_tasks.len() as u64 >= limits.max_running_tasks {
            result
                .blocked
                .push((entry.step_id.clone(), BlockingLimit::RunningTasks));
            continue;
        }
        if user_instances as u64 >= limits.max_instances {
            result
                .blocked
                .push((entry.step_id.clone(), BlockingLimit::ConcurrentInstances));
            continue;
        }
        let task_load = *per_task.get(&entry.task_id).unwrap_or(&0);
        // The admission itself takes one instance for this Task: refuse at the boundary.
        if task_load + 1 > limits.max_instances_per_task {
            result
                .blocked
                .push((entry.step_id.clone(), BlockingLimit::InstancesPerTask));
            continue;
        }

        // Consume the resources the start claims.
        user_instances += 1;
        *per_task.entry(entry.task_id.clone()).or_insert(0) += 1;
        if !task_running {
            running_tasks.push(entry.task_id.clone());
        }
        result.admitted.push(entry.step_id.clone());
        admitted_len += 1;
    }

    // ATH-SCHED-004: the queue is held, not rejected — up to the ceiling. Requests beyond
    // it are refused outright; they are the tail, lowest priority last enqueued (the same
    // displacement priority ordering shows).
    let post_admission_len = inventory.queue.len().saturating_sub(admitted_len);
    if post_admission_len as u64 > limits.max_queue_length {
        let surplus = (post_admission_len as u64) - limits.max_queue_length;
        let mut ordered_all: Vec<&QueuedStep> = inventory.queue.iter().collect();
        ordered_all.sort_by_key(|entry| (rank(entry.priority), entry.enqueue_order));
        result.overflow = ordered_all
            .iter()
            .rev()
            .take(surplus as usize)
            .map(|entry| entry.step_id.clone())
            .collect();
        result.overflow.reverse();
    }

    // ATH-SCHED-012: for a Task with a non-empty queue, every non-terminal instance
    // WAITING / SUSPENDED is hold-and-wait. Observable now, not at timeout.
    let mut task_represented: Vec<TaskId> = inventory
        .queue
        .iter()
        .map(|entry| entry.task_id.clone())
        .collect();
    task_represented.sort();
    task_represented.dedup();
    for task in task_represented {
        let non_terminal: Vec<&InventoryInstance> = inventory
            .instances
            .iter()
            .filter(|inst| inst.task_id == task && !terminal(&inst.state))
            .collect();
        if !non_terminal.is_empty() && non_terminal.iter().all(|inst| excluded(&inst.state)) {
            result.hold_and_wait_tasks.push(task);
        }
    }

    result
}

/// Anti-starvation promotion (ATH-SCHED-006): one priority level per interval, capped at
/// `normal`. The queue's stored priority is the Task's; the promoted level is what this
/// dispatch cycle decides by.
fn effective_rank(entry: &QueuedStep, interval_ms: u64) -> u8 {
    let promoted =
        interval_ms > 0 && entry.waited_ms >= interval_ms && entry.priority == Priority::Low;
    if promoted {
        rank(Priority::Normal)
    } else {
        rank(entry.priority)
    }
}

/// Dispatch-time priority rank (`high` first).
const fn rank(priority: Priority) -> u8 {
    match priority {
        Priority::High => 0,
        Priority::Normal => 1,
        Priority::Low => 2,
    }
}

/// `WAITING` / `SUSPENDED` stand outside every ceiling (ATH-SCHED-010).
#[must_use]
pub const fn excluded(state: &InstanceState) -> bool {
    matches!(state, InstanceState::Waiting | InstanceState::Suspended)
}

// --- P4-17: the approval-waiting cap (ATH-HITL-016) -------------------------------------------

/// The approval-waiting cap (`ATH-HITL-016`): the share of
/// `max_concurrent_instances` that may be awaiting approval at once. At or above the
/// cap, delegations that would create **new** approval requests are held (they do not
/// spawn a `WAITING_APPROVAL` / `SUSPENDED` pair) — the hold is observable and released
/// the moment a waiting instance leaves the set (granted, rejected, or expired). This
/// prevents half the system standing still in queues waiting for a human: a bounded
/// wait keeps the working set working.
///
/// The ratio is the configured `max_waiting_approval_ratio` (default 0.25, verified in
/// the §28.2 cross-constraints as positive).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ApprovalWaitCap {
    ratio: f64,
}

impl ApprovalWaitCap {
    /// The configured ratio.
    #[must_use]
    pub fn new(ratio: f64) -> Self {
        Self { ratio }
    }

    /// The ceiling in instance counts (floor at one so a non-disabled cap admits at
    /// least one waiting instance; zero disables the gate).
    #[must_use]
    pub fn ceiling(&self, max_concurrent_instances: u64) -> u64 {
        if self.ratio <= 0.0 {
            return 0;
        }
        ((self.ratio * max_concurrent_instances as f64) as u64).max(1)
    }

    /// Whether a new approval-requesting delegation must be held right now: the waiting
    /// count is already at (or over) the ceiling.
    #[must_use]
    pub fn holds(&self, waiting_for_approval: u64, max_concurrent_instances: u64) -> bool {
        let ceiling = self.ceiling(max_concurrent_instances);
        if ceiling == 0 {
            return false;
        }
        waiting_for_approval >= ceiling
    }
}

/// Terminal instance states (spec §17.2).
const fn terminal(state: &InstanceState) -> bool {
    matches!(state, InstanceState::Stopped | InstanceState::Failed)
}
