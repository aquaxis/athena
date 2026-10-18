//! Scheduler engine behaviour (ATH-SCHED-001…015).

use crate::scheduler::{
    BlockingLimit, Inventory, InventoryInstance, QueuedStep, RetryBudget, SchedulerLimits, tick,
};
use athena_config::schema::{BackoffSection, SchedulerSection};
use athena_types::{
    AgentInstanceState as InstanceState, PlanStepId, PlanStepState, Priority, TaskId,
};

fn limits() -> SchedulerLimits {
    SchedulerLimits::from_config(&SchedulerSection::default())
}

fn running_dep(id: &PlanStepId) -> Vec<(PlanStepId, PlanStepState)> {
    vec![(id.clone(), PlanStepState::Running)]
}

/// A dependency-satisfied entry of `task`.
fn ready_entry(task: &TaskId) -> QueuedStep {
    QueuedStep {
        task_id: task.clone(),
        step_id: PlanStepId::new(),
        priority: Priority::Normal,
        enqueue_order: 0,
        dependencies: vec![],
        waited_ms: 0,
        hold_until: None,
    }
}

fn busy_inventory(count: usize) -> Vec<InventoryInstance> {
    (0..count)
        .map(|_| InventoryInstance {
            task_id: TaskId::new(),
            state: InstanceState::Busy,
        })
        .collect()
}

#[test]
fn adoption_order_is_priority_then_fifo() {
    // ATH-SCHED-005: FIFO within a priority; `high` runs before `normal`.
    let task = TaskId::new();
    let mut low_first = ready_entry(&task);
    low_first.enqueue_order = 0;
    let mut high = ready_entry(&task);
    high.priority = Priority::High;
    high.enqueue_order = 1;
    let mut low_second = ready_entry(&task);
    low_second.enqueue_order = 2;

    let inventory = Inventory {
        instances: busy_inventory(0),
        running_tasks: vec![],
        queue: vec![low_first.clone(), high.clone(), low_second.clone()],
    };
    let result = tick(&inventory, limits(), 0);
    assert_eq!(
        result.admitted,
        vec![high.step_id, low_first.step_id, low_second.step_id]
    );
}

#[test]
fn an_unsatisfied_dependency_keeps_the_step_queued_silently() {
    // ATH-SCHED-001 + ATH-SCHED-007: satisfaction gates admission; why a dependency is
    // unmet is the planner's concern and produces no blocked fact.
    let task = TaskId::new();
    let unmet = PlanStepId::new();
    let waiting = QueuedStep {
        dependencies: running_dep(&unmet),
        ..ready_entry(&task)
    };
    let ready = ready_entry(&task);
    let inventory = Inventory {
        instances: busy_inventory(0),
        running_tasks: vec![],
        queue: vec![waiting, ready.clone()],
    };
    let result = tick(&inventory, limits(), 0);
    assert_eq!(result.admitted, vec![ready.step_id]);
    assert!(result.blocked.is_empty());
    assert!(result.hold_and_wait_tasks.is_empty());
}

#[test]
fn a_full_instance_ceiling_refuses_with_its_limit_named() {
    // ATH-SCHED-003 + ATH-SCHED-014: a dependency-satisfied step refused by `max_instances`
    // surfaces as `SchedulerBlocked` *now*, naming the limit.
    let task = TaskId::new();
    let inventory = Inventory {
        instances: busy_inventory(16),
        running_tasks: vec![],
        queue: vec![ready_entry(&task)],
    };
    let result = tick(&inventory, limits(), 0);
    assert!(result.admitted.is_empty());
    assert_eq!(
        result.blocked,
        vec![(
            result.blocked[0].0.clone(),
            BlockingLimit::ConcurrentInstances
        )]
    );
    assert_eq!(result.blocked[0].1.wire(), "max_concurrent_instances");
}

#[test]
fn waiting_and_suspended_instances_hold_no_slot() {
    // ATH-SCHED-010: sixteen Busy instances exhaust the ceiling; sixteen WAITING ones
    // do not, and admission flows.
    let task = TaskId::new();
    let waiting_inventory = Inventory {
        instances: (0..16)
            .map(|_| InventoryInstance {
                task_id: TaskId::new(),
                state: InstanceState::Waiting,
            })
            .collect(),
        running_tasks: vec![],
        queue: vec![ready_entry(&task)],
    };
    let result = tick(&waiting_inventory, limits(), 0);
    assert_eq!(result.admitted.len(), 1);
    assert!(result.blocked.is_empty());

    let suspended_inventory = Inventory {
        instances: (0..16)
            .map(|_| InventoryInstance {
                task_id: TaskId::new(),
                state: InstanceState::Suspended,
            })
            .collect(),
        running_tasks: vec![],
        queue: vec![ready_entry(&task)],
    };
    let result = tick(&suspended_inventory, limits(), 0);
    assert_eq!(result.admitted.len(), 1);
    assert!(result.blocked.is_empty());
}

#[test]
fn the_running_task_ceiling_gates_new_tasks_only() {
    // ATH-SCHED-009: `max_concurrent_tasks` counts Tasks in `RUNNING`+; a step of a Task
    // *already* running is not a new Task and must not consume a fresh one.
    let running_task = TaskId::new();
    let fresh_task = TaskId::new();
    let inventory = Inventory {
        instances: busy_inventory(8),
        running_tasks: (0..7)
            .map(|_| TaskId::new())
            .chain(vec![running_task.clone()])
            .collect(),
        queue: vec![
            QueuedStep {
                task_id: running_task.clone(),
                ..ready_entry(&running_task)
            },
            QueuedStep {
                task_id: fresh_task.clone(),
                ..ready_entry(&fresh_task)
            },
        ],
    };
    let result = tick(&inventory, local_limits(8, 16, 4), 0);
    // The running Task's step admits (no fresh running-task slot needed); the fresh Task's
    // step is refused by `max_concurrent_tasks`.
    assert!(result.admitted.contains(&inventory.queue[0].step_id));
    assert!(
        result
            .blocked
            .iter()
            .any(|(id, limit)| *id == inventory.queue[1].step_id
                && limit == &BlockingLimit::RunningTasks)
    );
}

#[test]
fn the_per_task_ceiling_is_enforced() {
    // ATH-SCHED-009: `max_instances_per_task` (default 4).
    let task = TaskId::new();
    let mut instances = busy_inventory(4);
    for inst in &mut instances {
        inst.task_id = task.clone();
    }
    let inventory = Inventory {
        instances,
        running_tasks: vec![],
        queue: vec![ready_entry(&task)],
    };
    let result = tick(&inventory, limits(), 0);
    assert!(
        result
            .blocked
            .iter()
            .any(|(_, limit)| *limit == BlockingLimit::InstancesPerTask),
        "{result:?}"
    );
}

#[test]
fn the_queue_ceiling_refuses_only_the_far_tail() {
    // ATH-SCHED-004: held requests stay queued; the surplus beyond the ceiling is refused
    // outright (TransientError), and it is the tail.
    let config = SchedulerSection {
        max_queue_length: 2,
        ..SchedulerSection::default()
    };
    let own_limits = SchedulerLimits::from_config(&config);
    let task = TaskId::new();
    // Three dependency *unsatisfied* entries: nothing admits, the queue stays full at 3,
    // which is 1 beyond the ceiling — the tail entry is refused this cycle.
    let unmet = PlanStepId::new();
    let mut held: Vec<QueuedStep> = (0..3)
        .map(|order| QueuedStep {
            dependencies: running_dep(&unmet),
            enqueue_order: order,
            ..ready_entry(&task)
        })
        .collect();
    held[2].enqueue_order = u64::MAX; // the very tail by order
    let inventory = Inventory {
        instances: busy_inventory(0),
        running_tasks: vec![],
        queue: held.clone(),
    };
    let result = tick(&inventory, own_limits, 0);
    assert!(result.admitted.is_empty());
    assert_eq!(result.overflow, vec![held[2].step_id.clone()]);
}

#[test]
fn promotion_pulls_a_starving_low_priority_shift_forward() {
    // ATH-SCHED-006: one level per interval, capped at `normal` (never above).
    let low = TaskId::new();
    let normal = TaskId::new();
    let interval_ms = 30 * 60 * 1000;
    let mut waiting_low = ready_entry(&low);
    waiting_low.priority = Priority::Low;
    waiting_low.waited_ms = interval_ms; // waited a full interval
    let mut fresh_low = ready_entry(&low);
    fresh_low.priority = Priority::Low;
    fresh_low.enqueue_order = 1;
    let mut fresh_normal = ready_entry(&normal);
    fresh_normal.enqueue_order = 2;

    let inventory = Inventory {
        instances: busy_inventory(0),
        running_tasks: vec![],
        queue: vec![waiting_low.clone(), fresh_low, fresh_normal.clone()],
    };
    let result = tick(&inventory, limits(), 0);
    // The waiting low was promoted into the `normal` rank, and its earlier enqueue order
    // beats the fresh normal entry FIFO. The still-fresh low stays behind.
    assert_eq!(result.admitted[0], waiting_low.step_id);
    assert_eq!(result.admitted[1], fresh_normal.step_id);
}

#[test]
fn backoff_gates_wait_by_derived_time_and_pass_silently() {
    // ATH-SCHED-013: the ready-time derivation from the persisted attempt fact + config;
    // a future gate is passed over with no event, and elapses into admission.
    let budget = RetryBudget {
        backoff: BackoffSection {
            initial_ms: 1000,
            multiplier: 2.0,
            max_ms: 60_000,
        },
    };
    let started_at = 5_000_u64;
    assert_eq!(budget.ready_at(started_at, 1), 6_000);
    assert_eq!(budget.ready_at(started_at, 2), 7_000);
    assert_eq!(budget.ready_at(started_at, 3), 9_000);
    // cap at max_ms
    assert_eq!(budget.ready_at(started_at, 30), 65_000);

    let task = TaskId::new();
    let mut held = ready_entry(&task);
    held.hold_until = Some(budget.ready_at(0, 2)); // 2_000
    let inventory = Inventory {
        instances: busy_inventory(0),
        running_tasks: vec![],
        queue: vec![held],
    };

    let early = tick(&inventory, limits(), 1_999);
    assert!(
        early.admitted.is_empty() && early.blocked.is_empty() && early.overflow.is_empty(),
        "a step inside its backoff window produces no fact at all"
    );
    let ripe = tick(&inventory, limits(), 2_000);
    assert_eq!(ripe.admitted.len(), 1);
}

#[test]
fn hold_and_wait_is_observable_at_once() {
    // ATH-SCHED-012: a Task's non-terminal inventory entirely WAITING while its queue is
    // non-empty is reported this cycle.
    let stuck = TaskId::new();
    let other_task = TaskId::new();
    let inventory = Inventory {
        instances: vec![InventoryInstance {
            task_id: stuck.clone(),
            state: InstanceState::Waiting,
        }],
        running_tasks: vec![],
        queue: vec![
            QueuedStep {
                task_id: stuck.clone(),
                step_id: PlanStepId::new(),
                priority: Priority::Normal,
                enqueue_order: 0,
                dependencies: running_dep(&PlanStepId::new()),
                waited_ms: 0,
                hold_until: None,
            },
            QueuedStep {
                task_id: other_task.clone(),
                step_id: PlanStepId::new(),
                priority: Priority::Normal,
                enqueue_order: 1,
                dependencies: vec![],
                waited_ms: 0,
                hold_until: None,
            },
        ],
    };
    let result = tick(&inventory, limits(), 0);
    assert_eq!(result.hold_and_wait_tasks, vec![stuck]);
}

#[test]
fn core_agent_slots_live_outside_the_instance_ceiling() {
    // ATH-SCHED-008: reserved slots are asserted to sit *outside* `max_instances`.
    let limits = limits();
    assert!(limits.reserved_core_agent_slots > 0);
    // A ceiling-full world: sixteen user instances hold the whole ceiling...
    let task = TaskId::new();
    let inventory = Inventory {
        instances: busy_inventory(16),
        running_tasks: vec![],
        queue: vec![ready_entry(&task)],
    };
    let result = tick(&inventory, limits, 0);
    assert_eq!(
        result.blocked[0].1,
        BlockingLimit::ConcurrentInstances,
        "the held step consumes none of the two reserved Core Agent slots"
    );
}

fn local_limits(tasks: u64, instances: u64, per_task: u64) -> SchedulerLimits {
    SchedulerLimits {
        max_running_tasks: tasks,
        max_instances: instances,
        max_instances_per_task: per_task,
        max_queue_length: 1000,
        reserved_core_agent_slots: 2,
        promotion_interval_ms: 30 * 60 * 1000,
    }
}

// --- P4-17: the approval-waiting cap (ATH-HITL-016) ---------------------------------------

mod approval_cap_tests {
    use crate::scheduler::ApprovalWaitCap;
    use athena_types::AgentInstanceState as InstanceState;

    #[test]
    fn the_cap_holds_at_the_ceiling_and_releases_below_it() {
        let cap = ApprovalWaitCap::new(0.25);
        // The §28.1 default: 0.25 × 16 → a 4-instance ceiling (floor at 1).
        assert_eq!(cap.ceiling(16), 4);
        assert!(!cap.holds(3, 16));
        assert!(cap.holds(4, 16), "the ceiling holds (ATH-HITL-016)");
        assert!(cap.holds(5, 16));
        // A released slot admits again.
        assert!(!cap.holds(3, 16));
        void(&InstanceState::Waiting);
    }
    fn void<T>(_: &T) {}

    #[test]
    fn the_ratio_scales_with_the_max_and_a_zero_ratio_disables() {
        let cap = ApprovalWaitCap::new(0.5);
        assert_eq!(cap.ceiling(8), 4);
        // Tiny hosts: the floor keeps one waiting slot available so the gate cannot
        // deadlock a small deployment.
        assert_eq!(cap.ceiling(2), 1);
        let disabled = ApprovalWaitCap::new(0.0);
        assert_eq!(disabled.ceiling(16), 0);
        assert!(!disabled.holds(99, 10), "a disabled cap never holds");
    }
}
