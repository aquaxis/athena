//! Runtime Manager — `AgentInstance` / `RuntimeInstance` (spec §26.3, design §5).
//!
//! Operates instances only through the Runtime Interface (ATH-RT-002); maps Runtime-level
//! observations onto the §17.2 instance state machine; holds no `match` on the Runtime kind
//! (ATH-RT-007) — the registry answers "which Runtime", not this module.
//!
//! Owned entities (`OWNED`): AgentInstance, RuntimeInstance. Emitted Events: `Agent*`,
//! `RuntimeInstance*`.

use athena_events::events::{AgentFailed, EventContext, TypedEvent, WorkspaceInitialized};
use athena_runtime::RuntimeInstanceStatus;
use athena_state::InstanceTrigger;
use athena_types::{Actor, AgentInstance, AgentInstanceState, EventId, SideEffects};

use crate::ownership::{Entity, ModuleRole};
use crate::protocol::ErrorClass;

/// The entities this module may mutate.
pub const OWNED: &[(Entity, ModuleRole)] = &[
    (Entity::AgentInstance, ModuleRole::RuntimeManager),
    (Entity::RuntimeInstance, ModuleRole::RuntimeManager),
];

/// A Runtime-level failure resolved against the §17.2 Agent Instance machine (P3-14: the
/// report from the Adapter's abnormal-exit observation into Core Host).
///
/// The resolution names the trigger, the state it lands on, and the Event it requires —
/// the caller (a supervision step of the Core Host) then persists the transition and the
/// typed Event through the store's atomic transition+Event primitive (`ATH-PERS-003`),
/// reusing the same deterministic recovery semantics as the boot reconciliation
/// (`ATH-CORE-048`: side effects unknown until Verification).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeFailureReport {
    /// The §17.2 trigger, routed per the instance's own state exactly as the boot
    /// reconciliation routes a lost process (`StartFailed` / `ProcessCrashed` /
    /// `ExecutionFailed`).
    pub trigger: InstanceTrigger,
    /// The state the §17.2 table resolves to (`Failed` for every route here).
    pub resolved_to: AgentInstanceState,
    /// The type name of the Event the transition requires (`AgentFailed`).
    pub event_type: &'static str,
    /// The nine-class taxonomy value.
    pub error_class: String,
    /// The observation detail, exactly as the Runtime reported it (no rewriting).
    pub detail: String,
    /// Whether the side effects are known — `Unknown`: a crashed process leaves this
    /// undetermined, so the retry is gated behind Verification (ATH-STATE-029).
    pub side_effects: SideEffects,
}

/// What one status observation means when interpreted against an instance.
///
/// Runtimeoise statuses (`Running`, `Waiting`, `Starting`, live `Suspended`) and a graceful
/// zero-exit `Stopped` are not failures — work reports its own outcomes. A `Failed`
/// observation or a nonzero-exit `Stopped` (ATH-ACLI-005: abnormal `agent-cli` termination)
/// is routed by the instance's own state; a failure in a state whose §17.2 book has no
/// process-fact route (`CREATED`, `WAITING`, `SUSPENDED`, …) is [an
/// `Unroutable`]
/// refusal rather than an invented transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// The observation finalizes the instance as `FAILED` with this report.
    Failed(RuntimeFailureReport),
    /// A live or gracefully-ended observation: nothing to record.
    NoFailure,
}

/// The asymmetry the §17.2 machine enforces: some instance states have no route out on a
/// process fact, and Core must not invent one (the wait paths own their own failures).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RejectedObservation {
    /// `Failed` observed in a state with no process-fact route; nothing transitions.
    Unroutable(AgentInstanceState),
}

impl core::fmt::Display for RejectedObservation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unroutable(state) => write!(
                f,
                "the failed observation has no §17.2 route from {state}; the state stays"
            ),
        }
    }
}

/// Interpret one Runtime status observation for one Agent Instance (P3-14).
///
/// The routes mirror the boot reconciliation's (`ATH-CORE-048`): `Starting → StartFailed`,
/// `Idle → ProcessCrashed`, `Busy → ExecutionFailed`, each landing in `FAILED` with the
/// `AgentFailed` Event.
///
/// # Errors
/// [`RejectedObservation::Unroutable`] when a failed observation lands in a state the
/// §17.2 machine does not route out of on a process fact.
pub fn resolve_runtime_failure(
    instance_state: AgentInstanceState,
    observed: &RuntimeInstanceStatus,
) -> Result<Resolution, RejectedObservation> {
    let detail = match observed {
        RuntimeInstanceStatus::Failed { detail } => Some(detail.clone()),
        RuntimeInstanceStatus::Stopped {
            exit_code: Some(code),
        } if *code != 0 => Some(format!("exited with a nonzero exit code ({code})")),
        _ => None,
    };
    let Some(detail) = detail else {
        // Graceful completion reports through messages; live statuses are not failures.
        return Ok(Resolution::NoFailure);
    };

    let trigger = match instance_state {
        AgentInstanceState::Starting => InstanceTrigger::StartFailed,
        AgentInstanceState::Idle => InstanceTrigger::ProcessCrashed,
        AgentInstanceState::Busy => InstanceTrigger::ExecutionFailed,
        // The wait states own their failures through their own triggers (reply deadline,
        // approval denial); §17.2 offers no process-fact route from here.
        _ => return Err(RejectedObservation::Unroutable(instance_state)),
    };
    // Verified through the same table the State Manager uses — the row must exist, end in
    // `FAILED`, and carry the `AgentFailed` Event for every route above.
    let row = athena_state::instance_transition(instance_state, trigger)
        .filter(|row| row.to == AgentInstanceState::Failed)
        .ok_or(RejectedObservation::Unroutable(instance_state))?;

    Ok(Resolution::Failed(RuntimeFailureReport {
        trigger,
        resolved_to: row.to,
        event_type: row.event,
        // The §22.2 taxonomy names its wire form `snake_case` (ATH-API-007's `class` field).
        error_class: serde_json::to_value(ErrorClass::RuntimeError)
            .map(|wire| wire.to_string().replace('"', ""))
            .unwrap_or_else(|_| "runtime_error".into()),
        detail,
        side_effects: SideEffects::Unknown,
    }))
}

/// Build the `AgentFailed` Event the report carries into the Event Store (P3-14): the
/// §23.2 payload row (`error_class`, `attempt`, `side_effects`) hygiene-checked against the
/// §23.1 envelope.
///
/// # Errors
/// Event construction failures (missing field, forbidden content, invalid envelope).
pub fn failure_event(
    report: &RuntimeFailureReport,
    instance: &AgentInstance,
    occurred_at: String,
    correlation_id: String,
) -> Result<TypedEvent<AgentFailed>, athena_events::EventError> {
    let payload = AgentFailed {
        error_class: report.error_class.clone(),
        attempt: instance.attempt,
        side_effects: report.side_effects,
    };
    TypedEvent::new(
        payload,
        EventContext {
            event_id: EventId::new(),
            occurred_at,
            actor: Actor::System,
            task_id: instance.task_id.clone(),
            plan_step_id: instance.plan_step_id.clone(),
            agent_id: Some(instance.agent_id.clone()),
            instance_id: Some(instance.id.clone()),
            skill_id: None,
            correlation_id,
            causation_id: None,
        },
    )
}

/// The `WorkspaceInitialized` Event builder (P3-16, `ATH-AIPRJ-024`).
///
/// The Runtime's start records the fact on the record; Core Host materializes the Event
/// through the Event Manager (`ATH-PERS-003`-style: emission is a cross-module effect,
/// D-1) with the resolved Workspace and the vendored `aiprj` revision.
///
/// # Errors
/// Event construction failures (missing field, forbidden content, invalid envelope).
pub fn workspace_initialized_event(
    agent_id: athena_types::AgentId,
    workspace: impl Into<String>,
    occurred_at: String,
    correlation_id: String,
) -> Result<TypedEvent<WorkspaceInitialized>, athena_events::EventError> {
    TypedEvent::new(
        WorkspaceInitialized {
            workspace: workspace.into(),
            aiprj_version: athena_runtime::AIPRJ_VERSION.to_string(),
        },
        EventContext {
            event_id: EventId::new(),
            occurred_at,
            actor: Actor::System,
            task_id: None,
            plan_step_id: None,
            agent_id: Some(agent_id),
            instance_id: None,
            skill_id: None,
            correlation_id,
            causation_id: None,
        },
    )
}
// --- P4-16: the SUSPENDED lifecycle --------------------------------------------------------

pub mod suspension {
    //! The suspension for approval (`ATH-STATE-019/020/021`, `R-ST-10`): a waiting human
    //! does not hold a process. The PlanStep/Instance route rides the section 17 tables
    //! (`SuspendForApproval` / `ApprovalGranted`, athena-state); this module freezes the
    //! facts the resume needs - the **frozen Skill Set** (same ids and versions on
    //! resume, `ATH-STATE-021`) and the short-term-memory summary reference - keyed by
    //! `instance_id` in Working Memory.

    use crate::ownership::{Entity, ModuleRole};
    use athena_types::{AgentInstance, SkillRef};

    /// Silent ownership re-statement (drift-checked like every Core Host module).
    #[must_use]
    pub const fn owned() -> &'static [(Entity, ModuleRole)] {
        &[
            (Entity::AgentInstance, ModuleRole::RuntimeManager),
            (Entity::MemoryRecord, ModuleRole::RuntimeManager),
        ]
    }

    /// The frozen snapshot the suspension writes (keyed by `instance_id`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct FrozenSnapshot {
        /// The suspended instance.
        pub instance_id: String,
        /// The Skill Set frozen at start (`ATH-MEM-007` last row): same ids + versions
        /// only on resume (`ATH-STATE-021`).
        pub skill_set: Vec<SkillRef>,
        /// The short-term summary record reference (Working Memory row), when the
        /// summarizer produced one; a `None` means the snapshot holds only the
        /// skill-set fact (the reconciling context rebuild tolerates it, `ATH-MEM-008`).
        pub short_term_summary_ref: Option<String>,
    }

    /// Freeze one instance: the Skill Set from the instance record itself, the summary
    /// reference from the caller (the snapshot writer).
    #[must_use]
    pub fn freeze(instance: &AgentInstance, summary_ref: Option<String>) -> FrozenSnapshot {
        FrozenSnapshot {
            instance_id: instance.id.as_str().to_string(),
            skill_set: instance.skill_set.clone(),
            short_term_summary_ref: summary_ref,
        }
    }

    /// The resume gate (`ATH-STATE-021`): the restarted instance must run the
    /// **identical** Skill Set - same ids and versions, same order. Image-based
    /// substitution is forbidden by refusal, not by trust.
    ///
    /// # Errors
    /// [`SkillSetDrift`] naming the first mismatch.
    pub fn resume_with(
        frozen: &FrozenSnapshot,
        requested_skill_set: &[SkillRef],
    ) -> Result<Vec<SkillRef>, SkillSetDrift> {
        if requested_skill_set.len() != frozen.skill_set.len() {
            return Err(SkillSetDrift {
                detail: format!(
                    "size drift: frozen {}, requested {}",
                    frozen.skill_set.len(),
                    requested_skill_set.len(),
                ),
            });
        }
        for (frozen_ref, requested_ref) in frozen.skill_set.iter().zip(requested_skill_set) {
            if frozen_ref.id != requested_ref.id || frozen_ref.version != requested_ref.version {
                return Err(SkillSetDrift {
                    detail: format!(
                        "frozen {}@{} does not match requested {}@{}",
                        frozen_ref.id.as_str(),
                        frozen_ref.version,
                        requested_ref.id.as_str(),
                        requested_ref.version,
                    ),
                });
            }
        }
        Ok(requested_skill_set.to_vec())
    }

    /// Why the resume refused.
    #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
    #[error(
        "the suspended instance cannot resume with a drifted Skill Set: {detail} (ATH-STATE-021)"
    )]
    pub struct SkillSetDrift {
        detail: String,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use athena_runtime::RuntimeInstanceStatus;
    use athena_types::{AgentInstanceState, InstanceId};

    fn instance(state: AgentInstanceState) -> AgentInstance {
        // The §26.3 fixture only needs the fields the report reads; the other entity rows
        // are inert for this mapping.
        AgentInstance {
            id: InstanceId::new(),
            agent_id: athena_types::AgentId::new("rk-worker"),
            task_id: Some(athena_types::TaskId::new()),
            plan_step_id: Some(athena_types::PlanStepId::new()),
            superior_instance_id: None,
            delegation_depth: athena_types::AgentLevel::Worker.depth(),
            state,
            runtime_instance_id: None,
            workspace: "/tmp/rk".into(),
            skill_set: vec![],
            aiprj_log_ref: None,
            attempt: 1,
            started_at: None,
            finished_at: None,
        }
    }

    #[test]
    fn the_suspension_freezes_the_skill_set_and_the_resume_demands_it_back() {
        use crate::runtime_manager::suspension::{self, SkillSetDrift};
        use athena_types::SkillRef;

        let mut instance = instance(AgentInstanceState::Busy);
        instance.skill_set = vec![
            athena_types::SkillRef {
                id: athena_types::SkillId::new("skill-report"),
                version: "1.0.0".into(),
                checksum: None,
            },
            athena_types::SkillRef {
                id: athena_types::SkillId::new("skill-lint"),
                version: "2.1.0".into(),
                checksum: None,
            },
        ];
        let mut suspended = instance.clone();
        suspended.state = AgentInstanceState::Suspended;
        let frozen = suspension::freeze(&suspended, Some("mem-1".into()));
        assert_eq!(frozen.instance_id, suspended.id.as_str());
        assert_eq!(frozen.skill_set.len(), suspended.skill_set.len());

        // The resume demands the identical Skill Set (same ids, versions, order).
        let ok = suspension::resume_with(&frozen, &frozen.skill_set);
        assert!(ok.is_ok(), "{ok:?}");

        // A drifted set refuses: different version.
        let mut drifted = frozen.skill_set.clone();
        if let Some(first) = drifted.first_mut() {
            first.version = "9.9.9".into();
        }
        let refused = suspension::resume_with(&frozen, &drifted)
            .expect_err("the drift is refused (ATH-STATE-021)");
        assert!(matches!(refused, SkillSetDrift { .. }), "{refused}");
        assert!(refused.to_string().contains("drifted Skill Set"));
        void(&refused);

        // A shrunken set refuses too.
        let shrunken: Vec<SkillRef> = frozen.skill_set.iter().skip(1).cloned().collect();
        assert!(suspension::resume_with(&frozen, &shrunken).is_err());
    }
    fn void<T>(_: &T) {}

    #[test]
    fn the_workspace_initialized_event_carries_the_resolved_facts() {
        let event = workspace_initialized_event(
            athena_types::AgentId::core_agent(),
            "/athena/var/instances/ai-1",
            "2026-10-17T00:00:00Z".into(),
            "cor-p3-16".into(),
        )
        .expect("built");
        assert_eq!(event.event_type, "WorkspaceInitialized");
        let wire: serde_json::Value =
            serde_json::from_str(&event.payload_json).expect("the payload serializes");
        assert_eq!(
            wire.get("workspace").and_then(|v| v.as_str()),
            Some("/athena/var/instances/ai-1")
        );
        assert_eq!(
            wire.get("aiprj_version").and_then(|v| v.as_str()),
            Some(athena_runtime::AIPRJ_VERSION),
            "the §23.2 payload row reports the vendored revision"
        );
        assert_eq!(
            event.correlation_id, "cor-p3-16",
            "the Event joins the correlation chain (ATH-OBS-004)"
        );
    }

    #[test]
    fn each_failed_observation_routes_per_the_172_state() {
        // The three §17.2 routes the boot reconciliation established, at supervision time.
        for (state, trigger) in [
            (AgentInstanceState::Starting, InstanceTrigger::StartFailed),
            (AgentInstanceState::Idle, InstanceTrigger::ProcessCrashed),
            (AgentInstanceState::Busy, InstanceTrigger::ExecutionFailed),
        ] {
            let resolution = resolve_runtime_failure(
                state,
                &RuntimeInstanceStatus::Failed {
                    detail: "exited unexpectedly".into(),
                },
            )
            .expect("a failed observation resolves");
            let Resolution::Failed(report) = resolution else {
                panic!("expected a failure report for {state}");
            };
            assert_eq!(report.trigger, trigger);
            assert_eq!(report.resolved_to, AgentInstanceState::Failed);
            assert_eq!(report.event_type, "AgentFailed");
            assert_eq!(report.error_class, "runtime_error", "the §20.1 class");
            assert_eq!(report.side_effects, SideEffects::Unknown, "ATH-STATE-029");
            assert_eq!(report.detail, "exited unexpectedly", "no rewriting");
        }
    }

    #[test]
    fn live_and_graceful_observations_record_nothing() {
        for observed in [
            RuntimeInstanceStatus::Running,
            RuntimeInstanceStatus::Waiting,
            RuntimeInstanceStatus::Starting,
            RuntimeInstanceStatus::Suspended,
            RuntimeInstanceStatus::Stopped { exit_code: Some(0) },
            RuntimeInstanceStatus::Stopped { exit_code: None },
        ] {
            let resolution =
                resolve_runtime_failure(AgentInstanceState::Busy, &observed).expect("no rejection");
            assert_eq!(resolution, Resolution::NoFailure);
        }
    }

    #[test]
    fn a_nonzero_exit_is_abnormal_and_routes_like_a_crash() {
        // ATH-ACLI-005: an abnormal `agent-cli` termination is the reportable fact.
        let resolution = resolve_runtime_failure(
            AgentInstanceState::Busy,
            &RuntimeInstanceStatus::Stopped {
                exit_code: Some(-1),
            },
        )
        .expect("routes");
        let Resolution::Failed(report) = resolution else {
            panic!("nonzero exit is a failure");
        };
        assert_eq!(report.trigger, InstanceTrigger::ExecutionFailed);
        assert!(report.detail.contains("-1"), "{}", report.detail);
    }

    #[test]
    fn a_failed_observation_in_an_unroutable_state_is_refused_not_invented() {
        // CREATED / WAITING / SUSPENDED / STOPPED: the §17.2 machine has no process-fact
        // route; Core must not invent one — the wait paths own their failures.
        for state in [
            AgentInstanceState::Created,
            AgentInstanceState::Waiting,
            AgentInstanceState::Suspended,
            AgentInstanceState::Stopped,
            AgentInstanceState::Failed,
        ] {
            let error = resolve_runtime_failure(
                state,
                &RuntimeInstanceStatus::Failed {
                    detail: "gone".into(),
                },
            )
            .expect_err("refused");
            assert_eq!(error, RejectedObservation::Unroutable(state));
        }
    }

    #[test]
    fn the_report_materializes_the_failure_event() {
        let instance = instance(AgentInstanceState::Busy);
        let report = {
            let Resolution::Failed(report) = resolve_runtime_failure(
                AgentInstanceState::Busy,
                &RuntimeInstanceStatus::Failed {
                    detail: "crashed".into(),
                },
            )
            .expect("resolves") else {
                unreachable!()
            };
            report
        };
        let event = failure_event(
            &report,
            &instance,
            "2026-10-17T00:00:00Z".into(),
            "cor-task".into(),
        )
        .expect("the typed event builds");
        assert_eq!(event.event_type, "AgentFailed");
        assert!(
            event.payload_json.contains("\"runtime_error\""),
            "{event:?}",
        );
        assert!(event.payload_json.contains("\"unknown\""), "{event:?}",);
        assert!(
            !event.payload_json.contains("cor-task"),
            "the envelope carries the correlation id, the payload carries none"
        );
    }
}
