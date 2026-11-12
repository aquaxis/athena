//! State Manager — Task / Plan / PlanStep transitions (spec §26.2, ATH-STATE-001).
//!
//! The only writer of Task / Plan / PlanStep state; agents, the Core Agent Process and the
//! Runtime may *request* transitions but never perform one. Each resolved transition is
//! applied together with its Event in a single transaction (ATH-PERS-003, D-1), which is
//! why cross-module effects route through here and the Event Manager rather than calling
//! sibling modules.
//!
//! Owned entities (`OWNED`): Task, Plan, PlanStep. Emitted Events: `Task*`, `Plan*`.

use crate::ownership::{Entity, ModuleRole};

/// The entities this module applies transitions to.
pub const OWNED: &[(Entity, ModuleRole)] = &[
    (Entity::Task, ModuleRole::StateManager),
    (Entity::Plan, ModuleRole::StateManager),
    (Entity::PlanStep, ModuleRole::StateManager),
];

// --- X-51: the Core Agent's `transition_task` acceptance path ------------------------------
//
// `CoreRequest::TransitionTask` was falling into the resident's generic "observed intention"
// arm, so a Task the Core Agent wanted moved never moved. The visible consequence was the one
// X-51 was filed for: a rejected approval leaves the Task in `WAITING_APPROVAL`, and nothing
// can bring it to `REPLANNING` — the §17.1 row for that edge names **Core Agent** as the
// requester (D-51), so the human's reject endpoint is the wrong actor for it and this is the
// right one.
//
// What makes the path narrow is the Event side. A transition writes the Event its table row
// names, and the catalog's mandatory keys must be *facts at that moment*: `TaskReplanning`
// needs `from_revision` / `to_revision`, which exist only once the superseding plan revision
// has been minted. The resolution is not to fabricate them and not to drop the edge, but to
// require the order the facts require — `submit_plan`, then `transition_task` — and to refuse
// with a message that says exactly that when the order is not yet satisfied.

pub mod transition {
    //! Accepting one `transition_task` request (`ATH-CORE-036`, `ATH-STATE-002`,
    //! `ATH-STATE-014`).

    use athena_persistence::repository::{PlanRepository, TaskRepository};
    use athena_persistence::{AthenaStore, NewEvent};
    use athena_state::{TaskTrigger, task_transition};
    use athena_types::{EventId, Plan, TaskId, TaskState};

    /// Why a requested transition was refused. Every variant is a refusal the agent can
    /// act on — none of them is Core Host deciding something on the agent's behalf.
    #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
    pub enum TransitionFault {
        /// No such Task.
        #[error("task {0} is absent")]
        TaskAbsent(String),
        /// `ATH-STATE-014` / `ATH-CORE-036`: the optimistic check failed — the agent's
        /// belief about the current state is stale, which is exactly what the key exists
        /// to catch.
        #[error("task {task} is in {actual}, not the expected {expected} (ATH-STATE-014)")]
        StateMismatch {
            /// The Task.
            task: String,
            /// What the store holds.
            actual: &'static str,
            /// What the request believed.
            expected: &'static str,
        },
        /// `ATH-STATE-002`: the transition table defines no edge from the current state to
        /// the requested one, so there is nothing to walk.
        #[error("no transition from {from} to {to} exists in the table (ATH-STATE-002)")]
        NoSuchEdge {
            /// The current state.
            from: &'static str,
            /// The requested state.
            to: &'static str,
        },
        /// The edge exists, but another path owns it: that path holds facts this one does
        /// not, and walking it from here would either bypass a gate or write an Event with
        /// invented keys. The message names the owner.
        #[error("the edge {from} → {to} is walked by {owner}, not by transition_task")]
        EdgeOwnedElsewhere {
            /// The current state.
            from: &'static str,
            /// The requested state.
            to: &'static str,
            /// What does own it, named so the agent can use it.
            owner: &'static str,
        },
        /// `TaskReplanning` names the revision that supersedes the current one, and no
        /// such revision exists yet: the superseding plan must be submitted first.
        #[error(
            "replanning records the revision that supersedes the current one; task {task} \
             holds revision {current}, so submit the superseding plan (submit_plan with \
             base_revision {current}) before requesting REPLANNING"
        )]
        SupersedingRevisionAbsent {
            /// The Task.
            task: String,
            /// The revision the store currently holds (0 when the Task has no plan).
            current: u64,
        },
        /// The store refused.
        #[error("store fault: {0}")]
        Store(String),
        /// The Event refused against the catalog.
        #[error("event construction: {0}")]
        Construction(String),
    }

    impl TransitionFault {
        /// The §20.1 class.
        #[must_use]
        pub const fn class(&self) -> crate::protocol::ErrorClass {
            match self {
                Self::TaskAbsent(_)
                | Self::StateMismatch { .. }
                | Self::NoSuchEdge { .. }
                | Self::SupersedingRevisionAbsent { .. } => {
                    crate::protocol::ErrorClass::ValidationError
                }
                // The request is well formed and the state machine allows the edge; what
                // refuses is the division of labour, which is a policy answer.
                Self::EdgeOwnedElsewhere { .. } => crate::protocol::ErrorClass::PolicyError,
                Self::Store(_) | Self::Construction(_) => {
                    crate::protocol::ErrorClass::InternalError
                }
            }
        }

        /// The stable machine code.
        #[must_use]
        pub const fn code(&self) -> &'static str {
            match self {
                Self::TaskAbsent(_) => "TASK_NOT_FOUND",
                Self::StateMismatch { .. } => "TASK_STATE_MISMATCH",
                Self::NoSuchEdge { .. } => "TASK_TRANSITION_UNDEFINED",
                Self::EdgeOwnedElsewhere { .. } => "TASK_TRANSITION_NOT_OWNED",
                Self::SupersedingRevisionAbsent { .. } => "PLAN_REVISION_ABSENT",
                Self::Store(_) | Self::Construction(_) => "TASK_TRANSITION_FAILED",
            }
        }
    }

    /// What one accepted transition recorded.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Transitioned {
        /// The Task.
        pub task_id: TaskId,
        /// The state it left.
        pub from: TaskState,
        /// The state it entered.
        pub to: TaskState,
        /// The Event the table's row names.
        pub event_type: &'static str,
        /// That Event's identity, as stored.
        pub event_id: EventId,
    }

    /// The triggers `transition_task` may fire, and for the rest, who owns them.
    ///
    /// The accepted pair is the replanning cycle, which is what the request exists for and
    /// the only pair whose Events are fully derivable from store facts:
    ///
    /// * `… → REPLANNING` from the three states whose table rows name Core Agent as the
    ///   requester — `WAITING_APPROVAL` (an approval rejected, `ATH-STATE-015`),
    ///   `WAITING_INPUT` (the human's answer requires a different plan) and `EVALUATING`
    ///   (criteria unmet with budget left) — writing `TaskReplanning`, whose revisions come
    ///   from the Task's own plan history;
    /// * `REPLANNING → RUNNING` (`NewPlanSettled`) — `TaskStarted`, whose `plan_id` and
    ///   `revision` are the latest plan's.
    ///
    /// Everything else the table offers out of these states is refused *by name*:
    /// `SUCCESS` and `FAILED` are the settlement's (X-48's gate decides acceptance, and a
    /// second path to the same edge would be a hole in it), `WAITING_INPUT` is
    /// `open_clarification`'s, `WAITING_APPROVAL` is the approval request's, `CANCELLING`
    /// is the human's. A refusal that names the owner is actionable; a silent acceptance
    /// that invents payload keys is not.
    fn trigger_for(from: TaskState, to: TaskState) -> Result<TaskTrigger, TransitionFault> {
        match (from, to) {
            (TaskState::WaitingApproval, TaskState::Replanning) => {
                Ok(TaskTrigger::ApprovalRejected)
            }
            (TaskState::Evaluating, TaskState::Replanning) => {
                Ok(TaskTrigger::CriteriaUnmetWithBudget)
            }
            (TaskState::WaitingInput, TaskState::Replanning) => {
                Ok(TaskTrigger::AnswerRequiresReplan)
            }
            (TaskState::Replanning, TaskState::Running) => Ok(TaskTrigger::NewPlanSettled),
            (from, to @ (TaskState::Success | TaskState::Failed)) => {
                Err(TransitionFault::EdgeOwnedElsewhere {
                    from: from.as_str(),
                    to: to.as_str(),
                    owner: "the Result settlement (settle_task), which reads the recorded \
                            Evaluations (ATH-EVAL-002)",
                })
            }
            (from, to @ TaskState::WaitingInput) => Err(TransitionFault::EdgeOwnedElsewhere {
                from: from.as_str(),
                to: to.as_str(),
                owner: "the clarification path (ask_human), which opens the row the answer \
                        endpoint resumes",
            }),
            (from, to @ TaskState::WaitingApproval) => Err(TransitionFault::EdgeOwnedElsewhere {
                from: from.as_str(),
                to: to.as_str(),
                owner: "the approval request (request_approval), which records the operation \
                        the human answers",
            }),
            (from, to @ (TaskState::Cancelling | TaskState::Cancelled)) => {
                Err(TransitionFault::EdgeOwnedElsewhere {
                    from: from.as_str(),
                    to: to.as_str(),
                    owner: "the human's cancel endpoint (ATH-STATE-016)",
                })
            }
            (from, to) => Err(TransitionFault::NoSuchEdge {
                from: from.as_str(),
                to: to.as_str(),
            }),
        }
    }

    /// Accept one `transition_task` request from the Core Agent.
    ///
    /// The order of checks is the order the rules are stated in: the Task exists, the
    /// optimistic key matches (`ATH-CORE-036` / `ATH-STATE-014`), the edge is one this path
    /// owns, the table confirms it (and names the Event and the requester), the Event's
    /// mandatory keys are assembled **from store facts**, and only then does the store walk
    /// the row and append the Event in one transaction (`ATH-PERS-003`).
    ///
    /// # Errors
    /// [`TransitionFault`], whole; the caller answers the agent with it (`ATH-CORE-034`).
    pub fn accept_transition(
        store: &mut AthenaStore,
        task_id: &TaskId,
        to: TaskState,
        expected_state: TaskState,
        now: &str,
        correlation_id: &str,
    ) -> Result<Transitioned, TransitionFault> {
        let task = TaskRepository::get(store, task_id)
            .map_err(store_fault)?
            .ok_or_else(|| TransitionFault::TaskAbsent(task_id.as_str().to_string()))?;
        if task.state != expected_state {
            return Err(TransitionFault::StateMismatch {
                task: task_id.as_str().to_string(),
                actual: task.state.as_str(),
                expected: expected_state.as_str(),
            });
        }
        let trigger = trigger_for(task.state, to)?;
        // The table is the authority on both the destination and the Event name; a
        // mismatch here would mean this module and the table disagree, which is a refusal,
        // never a silent preference for one of them.
        let resolved = task_transition(task.state, trigger).ok_or(TransitionFault::NoSuchEdge {
            from: task.state.as_str(),
            to: to.as_str(),
        })?;
        if resolved.to != to {
            return Err(TransitionFault::NoSuchEdge {
                from: task.state.as_str(),
                to: to.as_str(),
            });
        }

        let latest = PlanRepository::latest_for_task(store, task_id).map_err(store_fault)?;
        let payload = payload_for(resolved.event, task_id, task.state, to, latest.as_ref())?;
        let event = NewEvent {
            event_id: EventId::new(),
            schema_version: 1,
            event_type: resolved.event.to_string(),
            occurred_at: now.to_string(),
            task_id: Some(task_id.clone()),
            plan_step_id: None,
            agent_id: Some(athena_types::AgentId::core_agent()),
            instance_id: None,
            skill_id: None,
            correlation_id: correlation_id.to_string(),
            causation_id: None,
            // The table's own requester, never a guess (§17.1 names Core Agent for these
            // rows, which is the whole reason this path exists rather than the API's).
            actor: resolved.requester.as_str().to_string(),
            payload_json: payload.to_string(),
        };
        let stored = store
            .transition_task(task_id, expected_state, to, &event)
            .map_err(store_fault)?;
        Ok(Transitioned {
            task_id: task_id.clone(),
            from: task.state,
            to,
            event_type: resolved.event,
            event_id: stored.event_id,
        })
    }

    /// The Event payload for one accepted edge, assembled from facts.
    ///
    /// `TaskReplanning`'s revisions are the Task's own plan history: the superseding
    /// revision is the latest one the store holds, and the one it replaces is the revision
    /// before it. A Task whose latest revision is the first (or which has no plan at all)
    /// has nothing that supersedes anything, and the refusal says so rather than writing
    /// `to_revision: 1, from_revision: 0` — numbers that would describe a replacement that
    /// never happened.
    ///
    /// The `reason` is Core Host's own description of the circumstance (the catalog's doc
    /// is explicit that this key is *not* agent input): the state the Task is leaving and
    /// the revision pair it is moving between. `ATH-CORE-016` wants the record readable
    /// afterwards, which this is; the agent's own prose belongs in its evaluation and plan
    /// rationale, where it is recorded as the agent's.
    fn payload_for(
        event_type: &str,
        task_id: &TaskId,
        from: TaskState,
        to: TaskState,
        latest: Option<&Plan>,
    ) -> Result<serde_json::Value, TransitionFault> {
        match event_type {
            "TaskReplanning" => {
                let current = latest.map_or(0, |plan| plan.revision);
                if current < 2 {
                    return Err(TransitionFault::SupersedingRevisionAbsent {
                        task: task_id.as_str().to_string(),
                        current,
                    });
                }
                Ok(serde_json::json!({
                    "from_revision": current - 1,
                    "to_revision": current,
                    "reason": format!(
                        "the Core Agent requested replanning from {}; revision {} supersedes {}",
                        from.as_str(),
                        current,
                        current - 1
                    ),
                }))
            }
            "TaskStarted" => {
                let plan = latest.ok_or(TransitionFault::SupersedingRevisionAbsent {
                    task: task_id.as_str().to_string(),
                    current: 0,
                })?;
                Ok(serde_json::json!({
                    "plan_id": plan.id.as_str(),
                    "revision": plan.revision,
                }))
            }
            // Unreachable while `trigger_for` admits exactly the two edges above; a future
            // addition that forgets its payload lands here rather than writing an Event
            // with missing keys.
            other => Err(TransitionFault::Construction(format!(
                "no payload rule for {other} ({} → {})",
                from.as_str(),
                to.as_str()
            ))),
        }
    }

    fn store_fault(fault: athena_persistence::error::PersistenceError) -> TransitionFault {
        TransitionFault::Store(fault.to_string())
    }

    /// The agent-facing answer for an accepted transition (`ATH-CORE-034`).
    #[must_use]
    pub fn acceptance_response(
        request_id: &str,
        transitioned: &Transitioned,
    ) -> crate::protocol::CoreResponse {
        crate::protocol::CoreResponse::Ok {
            request_id: request_id.to_string(),
            data: serde_json::json!({
                "task_id": transitioned.task_id.as_str(),
                "from": transitioned.from.as_str(),
                "to": transitioned.to.as_str(),
                "event_type": transitioned.event_type,
                "event_id": transitioned.event_id.as_str(),
            }),
        }
    }

    /// The agent-facing answer for a refusal; the class and code are the fault's own.
    #[must_use]
    pub fn refusal_response(
        request_id: &str,
        correlation_id: &str,
        fault: &TransitionFault,
    ) -> crate::protocol::CoreResponse {
        // The one structured fact a refusal can act on: the state the store actually holds
        // (so the agent can re-issue with the right `expected_state`), or the revision it
        // must supersede first.
        let details = match fault {
            TransitionFault::StateMismatch { actual, .. } => {
                serde_json::json!({ "actual_state": actual })
            }
            TransitionFault::SupersedingRevisionAbsent { current, .. } => {
                serde_json::json!({ "current_revision": current })
            }
            _ => serde_json::Value::Null,
        };
        crate::protocol::CoreResponse::Err {
            request_id: request_id.to_string(),
            error: crate::protocol::ErrorEnvelope {
                class: fault.class(),
                code: fault.code().to_string(),
                message: fault.to_string(),
                details,
                correlation_id: correlation_id.to_string(),
            },
        }
    }
}
