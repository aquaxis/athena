//! The Process Manager (P3-02…P3-04; spec §14.4, design §5.4).
//!
//! Responsibilities realized here: spawn and reap, pid/heartbeat/exit-code tracking,
//! startup and execution timeout enforcement (`ATH-RT-006`), graceful `stop` then the
//! graceful signal then force-kill after `timeout.stop` (`ATH-FAIL-015`), resource
//! release for terminal instances (`ATH-RT-010`), and reattach-or-stop reconciliation
//! after a Core restart (`ATH-RT-011`).
//!
//! The manager owns per-instance records; the Adapter stays a thin projection of the
//! Runtime Interface over it. Time comes from injected [`Clock`]/`heartbeat` facts, so
//! the whole surface stays deterministic in tests.

use crate::adapter::{ProcessDriver, command_for, command_for_core};
use athena_runtime::{RuntimeError, RuntimeInstanceRef, RuntimeInstanceStatus, StartContext};
use athena_types::{AgentDefinition, RuntimeInstanceId, StopReason};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

/// Time injection: the deterministic test surface calls these from a fixed `now`.
pub trait Clock: Send + Sync {
    /// The current instant.
    fn now(&self) -> SystemTime;
}

/// Wall clock.
#[derive(Debug, Default)]
pub struct WallClock;

impl Clock for WallClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// The manager's per-Instance record.
#[derive(Debug, Clone)]
pub struct Record {
    /// The OS pid (or driver-counter equivalent).
    pub pid: u32,
    /// The minted Runtime Instance id (`ri-…`).
    pub runtime_instance_id: RuntimeInstanceId,
    /// The Agent Instance the process materializes (ATH-RT-012, 1:1).
    pub instance_id: athena_types::InstanceId,
    /// The Workspace cwd.
    pub workspace: PathBuf,
    /// The definition the process runs.
    pub definition: AgentDefinition,
    /// When the process was spawned.
    pub started_at: SystemTime,
    /// The last observed activity (the heartbeat fact).
    pub last_beat: SystemTime,
    /// Whether this record is instance #0, the Core Agent (P3-15). The Core Agent's
    /// lifecycle is supervised by the Core Agent Session (§7.5), never by the
    /// subordinate flow — the role fact keeps its records addressable and segregated.
    pub is_core_agent: bool,
    /// Whether *this start* initialized (or repaired) the Workspace's `aiprj` write
    /// areas (P3-16, `ATH-AIPRJ-024`) — the fact Core Host reads to materialize the
    /// `WorkspaceInitialized` Event; a `Ready` start records `false`.
    pub workspace_initialized: bool,
    /// The Task the delegation binds, when the start was Task-bound (the result intake's
    /// ownership fact; instance #0 carries `None` — its result is never a Task result).
    pub task_id: Option<athena_types::TaskId>,
    /// The PlanStep the delegation executes, when the start carried one.
    pub plan_step_id: Option<athena_types::PlanStepId>,
    /// The peer agent-id captured from the spawn launcher's stdout (`agent-…`);
    /// duplicate display names make name-addressing ambiguous, so the transport
    /// addresses by this id (0.29.0 verified 2026-10-18).
    pub peer_id: Option<String>,
}

/// The reconciliation outcome (`ATH-RT-011`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Instances whose process was still alive after a Core restart: reattached.
    pub reattached: Vec<RuntimeInstanceId>,
    /// Instances whose process had died: released rather than resurrected.
    pub released: Vec<RuntimeInstanceId>,
}

/// The manager's tunables (`timeout.*` and the heartbeat budget).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budgets {
    /// Startup deadline (ATH-RT-006).
    pub startup: Duration,
    /// Execution deadline (ATH-RT-006).
    pub execution: Duration,
    /// Graceful-stop budget before the force-kill (ATH-FAIL-015).
    pub stop: Duration,
    /// Silence tolerated before the heartbeat fact is lost (ATH-RT-006 cohort).
    pub heartbeat: Duration,
}

impl Budgets {
    /// Budgets from the configuration's `timeout` values and the heartbeat budget.
    #[must_use]
    pub fn new(
        startup: Duration,
        execution: Duration,
        stop: Duration,
        heartbeat: Duration,
    ) -> Self {
        Self {
            startup,
            execution,
            stop,
            heartbeat,
        }
    }
}

#[derive(Default)]
struct Live {
    records: HashMap<String, Record>,
}

/// The Process Manager (design §5.4).
pub struct ProcessManager<D: ProcessDriver> {
    driver: D,
    live: Arc<Mutex<Live>>,
    budgets: Budgets,
    clock: Arc<dyn Clock>,
}

impl<D: ProcessDriver + 'static> ProcessManager<D> {
    /// The manager over its driver, budgets, and clock.
    #[must_use]
    pub fn new(driver: D, budgets: Budgets, clock: Arc<dyn Clock>) -> Self {
        Self {
            driver,
            live: Arc::new(Mutex::new(Live::default())),
            budgets,
            clock,
        }
    }

    /// Spawn the definition's process in its Workspace and record it (P3-02).
    ///
    /// # Errors
    /// Spawn failures surface whole.
    pub fn spawn(
        &self,
        definition: &AgentDefinition,
        ctx: &StartContext,
    ) -> Result<RuntimeInstanceRef, RuntimeError> {
        let workspace = PathBuf::from(ctx.workspace.as_str());
        let spawned = self.driver.spawn(&workspace, &command_for(definition))?;
        let now = self.clock.now();
        let record = Record {
            pid: spawned.pid,
            runtime_instance_id: cloned_id(&spawned),
            instance_id: ctx.instance_id.clone(),
            workspace: workspace.clone(),
            definition: definition.clone(),
            started_at: now,
            last_beat: now,
            is_core_agent: false,
            workspace_initialized: false,
            task_id: ctx.task_id.clone(),
            plan_step_id: ctx.plan_step_id.clone(),
            peer_id: spawned.peer_id.clone(),
        };
        self.live
            .lock()
            .expect("manager live")
            .records
            .insert(spawned.runtime_instance_id.as_str().to_string(), record);
        Ok(RuntimeInstanceRef {
            runtime_type: athena_types::RuntimeType::AgentCli,
            runtime_instance_id: cloned_id(&spawned),
            instance_id: ctx.instance_id.clone(),
        })
    }

    /// Spawn the Core Agent, instance #0, with its `spawn`-mode argv (P3-15;
    /// `ATH-ACLI-008`, Phase 0: `spawn` re-execs `serve` detached and self-registers as a
    /// peer, which is what makes the resident, IPC-driven session real).
    ///
    /// Instance #0 is unique (`ATH-RT-012`, 1 : 1): a second core-class start while one
    /// is resident is refused; a *stale* record whose process has died is dropped so the
    /// Core Host's restart decision (§7.5.3, its own window) can bring instance #0 back.
    /// The Core Agent record is segregated: subordinate supervision never reads it, and
    /// its workspace is the Core Agent's own (`<ATHENA_HOME>/var/core-agent/`), never
    /// shared with subordinates (`ATH-OBS` cohort aside, spec §7.5.1).
    ///
    /// # Errors
    /// A Task-bound context (`ATH-CORE-020`), a non-`athena-core-agent` definition id
    /// (`ATH-MSG-011`), an already-resident instance #0, or driver spawn failures.
    pub fn spawn_core_agent(
        &self,
        definition: &AgentDefinition,
        ctx: &StartContext,
    ) -> Result<RuntimeInstanceRef, RuntimeError> {
        if ctx.task_id.is_some() {
            return Err(RuntimeError::Validation(
                "instance #0 is session-scoped; no Task may bind to it (ATH-CORE-020)".into(),
            ));
        }
        if !definition.id.is_core_agent() {
            return Err(RuntimeError::Validation(format!(
                "the Core Agent's AgentId is {} (ATH-MSG-011); got {}",
                athena_types::AgentId::CORE_AGENT,
                definition.id.as_str()
            )));
        }
        if let Some(stale) = self.core_stale() {
            self.live
                .lock()
                .expect("manager live")
                .records
                .remove(stale.runtime_instance_id.as_str());
        } else if self.core_live_record().is_some() {
            return Err(RuntimeError::Failure(
                "instance #0 is already resident; Agent Instance : Runtime Instance = 1:1
                 (ATH-RT-012)"
                    .into(),
            ));
        }
        let workspace = PathBuf::from(ctx.workspace.as_str());
        let spawned = self
            .driver
            .spawn(&workspace, &command_for_core(definition))?;
        let now = self.clock.now();
        let record = Record {
            pid: spawned.pid,
            runtime_instance_id: cloned_id(&spawned),
            instance_id: ctx.instance_id.clone(),
            workspace: workspace.clone(),
            definition: definition.clone(),
            started_at: now,
            last_beat: now,
            is_core_agent: true,
            workspace_initialized: false,
            task_id: ctx.task_id.clone(),
            plan_step_id: ctx.plan_step_id.clone(),
            peer_id: spawned.peer_id.clone(),
        };
        self.live
            .lock()
            .expect("manager live")
            .records
            .insert(spawned.runtime_instance_id.as_str().to_string(), record);
        Ok(RuntimeInstanceRef {
            runtime_type: athena_types::RuntimeType::AgentCli,
            runtime_instance_id: cloned_id(&spawned),
            instance_id: ctx.instance_id.clone(),
        })
    }

    /// The live Core Agent record, when instance #0 is tracked.
    #[must_use]
    pub fn core_live_record(&self) -> Option<Record> {
        self.live
            .lock()
            .expect("manager live")
            .records
            .values()
            .find(|record| record.is_core_agent)
            .cloned()
    }

    /// The stale Core Agent record: role-tagged, but its process has died.
    #[must_use]
    fn core_stale(&self) -> Option<Record> {
        self.live
            .lock()
            .expect("manager live")
            .records
            .values()
            .find(|record| record.is_core_agent && !self.driver.is_alive(record.pid))
            .cloned()
    }

    /// Every subordinate record — the Core Agent's (`is_core_agent`) is excluded, which
    /// is the supervision isolation instance #0 depends on (P3-15): the subordinate flow
    /// reads this list, the Core Agent Session reads [`Self::core_live_record`].
    #[must_use]
    pub fn subordinate_records(&self) -> Vec<Record> {
        self.live
            .lock()
            .expect("manager live")
            .records
            .values()
            .filter(|record| !record.is_core_agent)
            .cloned()
            .collect()
    }

    /// Stamp the record's workspace fact (P3-16): whether *this start* initialized the
    /// `aiprj` areas — the value the record carries for Core Host's
    /// `WorkspaceInitialized` materialization.
    pub fn set_workspace_initialized(&self, runtime_instance_id: &str, initialized: bool) {
        if let Some(record) = self
            .live
            .lock()
            .expect("manager live")
            .records
            .get_mut(runtime_instance_id)
        {
            record.workspace_initialized = initialized;
        }
    }

    /// Record activity: the heartbeat fact (P3-02) — refreshed on outbound and inbound
    /// traffic the Adapter observes.
    pub fn beat(&self, runtime_instance_id: &str) {
        let now = self.clock.now();
        if let Some(record) = self
            .live
            .lock()
            .expect("manager live")
            .records
            .get_mut(runtime_instance_id)
        {
            record.last_beat = now;
        }
    }

    /// The record's read-only view, for the Adapter's bookkeeping.
    #[must_use]
    pub fn record(&self, runtime_instance_id: &str) -> Option<Record> {
        self.live
            .lock()
            .expect("manager live")
            .records
            .get(runtime_instance_id)
            .cloned()
    }

    /// The live record count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.live.lock().expect("manager live").records.len()
    }

    /// Whether the manager tracks nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether one recorded process is still running — the liveness fact the Adapter's
    /// send path consults (`UnknownInstance` semantics live on the caller side).
    ///
    /// # Errors
    /// `UnknownInstance` when nothing is recorded.
    pub fn is_running(&self, runtime_instance_id: &str) -> Result<bool, RuntimeError> {
        let record = self
            .record(runtime_instance_id)
            .ok_or_else(|| RuntimeError::UnknownInstance(runtime_instance_id.to_string()))?;
        Ok(self.driver.is_alive(record.pid))
    }

    /// The deadline-aware status (`ATH-RT-006`; P3-14's classification).
    ///
    /// - process gone with code 0 → `Stopped`; nonzero/absent → `Failed` (ATH-ACLI-005);
    /// - startup window not yet elapsed → `Starting`;
    /// - execution deadline passed → `Failed` with the deadline detail;
    /// - heartbeat silence past the budget → `Failed` (the supervision contract broke).
    ///
    /// # Errors
    /// `UnknownInstance` when nothing is recorded.
    pub async fn status(
        &self,
        runtime_instance_id: &str,
    ) -> Result<RuntimeInstanceStatus, RuntimeError> {
        let record = self
            .record(runtime_instance_id)
            .ok_or_else(|| RuntimeError::UnknownInstance(runtime_instance_id.to_string()))?;
        if !self.driver.is_alive(record.pid) {
            return Ok(match self.driver.exit_code(record.pid) {
                Some(0) => RuntimeInstanceStatus::Stopped { exit_code: Some(0) },
                Some(code) => RuntimeInstanceStatus::Failed {
                    detail: format!("exit code {code}"),
                },
                None => RuntimeInstanceStatus::Failed {
                    detail: "the process vanished without an exit code (ATH-ACLI-005)".into(),
                },
            });
        }
        let now = self.clock.now();
        let elapsed = now
            .duration_since(record.started_at)
            .unwrap_or(Duration::ZERO);
        if elapsed < self.budgets.startup {
            return Ok(RuntimeInstanceStatus::Starting);
        }
        if elapsed >= self.budgets.execution {
            return Ok(RuntimeInstanceStatus::Failed {
                detail: format!("execution deadline passed (ATH-RT-006, {elapsed:?})"),
            });
        }
        let silence = now
            .duration_since(record.last_beat)
            .unwrap_or(Duration::ZERO);
        if silence >= self.budgets.heartbeat {
            return Ok(RuntimeInstanceStatus::Failed {
                detail: format!("heartbeat lost for {silence:?}"),
            });
        }
        Ok(RuntimeInstanceStatus::Running)
    }

    /// Graceful stop: the graceful signal, a poll until `timeout.stop`, then the
    /// force-kill (`ATH-FAIL-015`); the record releases at terminal state (`ATH-RT-010`)
    /// so later probes report rather than fabricate.
    ///
    /// # Errors
    /// `UnknownInstance`, or driver kill failures.
    pub async fn stop(
        &self,
        runtime_instance_id: &str,
        _reason: StopReason,
    ) -> Result<(), RuntimeError> {
        let record = self
            .record(runtime_instance_id)
            .ok_or_else(|| RuntimeError::UnknownInstance(runtime_instance_id.to_string()))?;
        self.driver.terminate(record.pid)?;
        let deadline = self.clock.now() + self.budgets.stop;
        while self.driver.is_alive(record.pid) {
            if self.clock.now() >= deadline {
                self.driver.force_kill(record.pid)?;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.live
            .lock()
            .expect("manager live")
            .records
            .remove(runtime_instance_id);
        Ok(())
    }

    /// Reconciliation after a Core restart (`ATH-RT-011`): the records carried forward
    /// (from the persisted Agent Instance state) are reattached when the process is
    /// alive and released when it is not — a dead process is never resurrected.
    ///
    /// # Errors
    /// Surface from records that name processes the driver cannot treat.
    pub fn reconcile(&self, carried: Vec<Record>) -> Result<ReconcileReport, RuntimeError> {
        let mut report = ReconcileReport {
            reattached: vec![],
            released: vec![],
        };
        for mut record in carried {
            if self.driver.is_alive(record.pid) {
                record.last_beat = self.clock.now();
                report.reattached.push(record.runtime_instance_id.clone());
                self.live
                    .lock()
                    .expect("manager live")
                    .records
                    .insert(record.runtime_instance_id.as_str().to_string(), record);
            } else {
                self.driver.force_kill(record.pid)?;
                report.released.push(record.runtime_instance_id.clone());
            }
        }
        Ok(report)
    }
}

fn cloned_id(spawned: &crate::adapter::SpawnedProcess) -> RuntimeInstanceId {
    spawned.runtime_instance_id.clone()
}
