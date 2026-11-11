//! The delegation intake (X-44): a Core Agent's `delegate` becomes an `AgentInstance` row
//! for one PlanStep attempt, or a refusal that names its ground.
//!
//! What this module owns is the *admission decision and the row* — the rules a delegation
//! must pass before any process is started:
//!
//! * the idempotency claim (`ATH-CORE-035`, `ATH-NFR-017`): a key is executed once, so a
//!   Core Agent restart that re-sends a delegation replays the claim instead of starting a
//!   second Instance;
//! * the single-flight rule (`ATH-DATA-010`): a PlanStep with a non-terminal Instance may
//!   not be delegated again;
//! * the parent record (`ATH-AGENT-002`): an Instance never outlives a definition absent
//!   from the store — and with no definition, nothing can be started;
//! * the delegation ceiling (`ATH-HIER-005`, `ATH-DATA-005`): the definition's own
//!   `max_delegation_depth` bounds the depth the new Instance would sit at;
//! * the frozen Skill Set (`ATH-DATA-006`, `ATH-SKILL-010`): the set rides the request and
//!   is written once, never edited afterwards.
//!
//! The row lands in `CREATED` — the §17.2 state machine's entry — because starting it is
//! the Runtime Manager's act on the `CREATED → STARTING` edge (`AgentStarting`), which this
//! module does not perform. The same cadence/semantics split the core keeps (D-35): the
//! rule and the row here, the process and the clock in the resident.
//!
//! **Not in scope, and why.** No process is spawned here: the `CREATED → STARTING` edge is
//! the Runtime Manager's, and the resident drives it. The caller names the instances tree
//! (`<ATHENA_HOME>/var/instances`) because the layout's root belongs to the deployment, not
//! to this rule.

use athena_persistence::repository::{
    AgentRepository, EventRepository, IdempotencyRepository, PlanRepository, TaskRepository,
};
use athena_persistence::{AthenaStore, NewEvent};
use athena_types::{
    AgentId, AgentInstance, AgentInstanceState, EventId, InstanceId, PlanStepId, SkillRef, TaskId,
};

/// One delegation request, as the protocol states it (`CoreRequest::Delegate`).
#[derive(Debug, Clone, PartialEq)]
pub struct DelegationRequest {
    /// The Task the step belongs to.
    pub task_id: TaskId,
    /// The step to run.
    pub plan_step_id: PlanStepId,
    /// The agent the Core Agent selected.
    pub agent_id: AgentId,
    /// The resolved Skill Set, frozen onto the Instance (`ATH-SKILL-010`).
    pub skill_set: Vec<SkillRef>,
    /// `plan_step_id + ":" + attempt` (`ATH-CORE-035`); the key Core Host executes once.
    pub idempotency_key: String,
}

/// What an accepted delegation produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegationAccepted {
    /// The Instance row's identifier (Core-minted, `ATH-CORE-041`).
    pub instance_id: InstanceId,
    /// The Instance's cwd: `<instances_root>/<instance id>` (`ATH-OPS-009`), the layout
    /// `init_workspace` creates and the area sweep's fence recognizes (X-41).
    pub workspace: String,
    /// The attempt this Instance is (`ATH-FAIL-007`: each attempt is its own Instance).
    pub attempt: u64,
    /// The depth the Instance sits at (`ATH-HIER-005`).
    pub delegation_depth: u64,
    /// The `AgentDelegated` record's identifier.
    pub event_id: EventId,
    /// `true` when this call replayed an already-claimed key rather than creating a row —
    /// the restart-safety face (`ATH-NFR-017`). The identifiers then name the Instance the
    /// first execution created.
    pub replayed: bool,
}

/// Why a delegation was not admitted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DelegationFault {
    /// The Task the request names is absent.
    #[error("task {0} is absent")]
    TaskAbsent(String),
    /// The PlanStep the request names is absent.
    #[error("plan step {0} is absent")]
    StepAbsent(String),
    /// The step already holds a non-terminal Instance (`ATH-DATA-010`): delegating again
    /// would run one step twice.
    #[error("plan step {step} already holds {count} non-terminal instance(s) (ATH-DATA-010)")]
    StepAlreadyRunning {
        /// The step.
        step: String,
        /// How many non-terminal Instances it holds.
        count: u64,
    },
    /// No definition for the selected agent (`ATH-AGENT-002`): an Instance may not outlive
    /// a definition the store does not hold, so nothing can be started.
    #[error("agent definition {0} is absent (ATH-AGENT-002)")]
    DefinitionAbsent(String),
    /// The new Instance would sit below the definition's delegation ceiling
    /// (`ATH-HIER-005`, `ATH-DATA-005`).
    #[error("delegation depth {depth} exceeds {agent}'s ceiling {ceiling} (ATH-HIER-005)")]
    DepthExceeded {
        /// The agent whose ceiling was consulted.
        agent: String,
        /// The depth the Instance would sit at.
        depth: u64,
        /// The ceiling.
        ceiling: u64,
    },
    /// The idempotency key is malformed: `ATH-CORE-035` fixes its shape as
    /// `plan_step_id:attempt`, and the attempt is a fact the row carries — it is read from
    /// the key rather than invented.
    #[error("idempotency key {0:?} is not plan_step_id:attempt (ATH-CORE-035)")]
    MalformedKey(String),
    /// The store refused a read or a write; surfaced whole.
    #[error("store fault: {0}")]
    Store(String),
    /// The `AgentDelegated` record could not be constructed against the catalog.
    #[error("event construction: {0}")]
    Construction(String),
}

impl DelegationFault {
    /// The §20.1 class of this fault.
    #[must_use]
    pub fn class(&self) -> crate::protocol::ErrorClass {
        match self {
            Self::TaskAbsent(_)
            | Self::StepAbsent(_)
            | Self::DefinitionAbsent(_)
            | Self::MalformedKey(_) => crate::protocol::ErrorClass::ValidationError,
            // A single-flight refusal and a ceiling refusal are policy answers: the request
            // is well-formed and the configuration/plan state refuses it.
            Self::StepAlreadyRunning { .. } | Self::DepthExceeded { .. } => {
                crate::protocol::ErrorClass::PolicyError
            }
            Self::Store(_) | Self::Construction(_) => crate::protocol::ErrorClass::InternalError,
        }
    }

    /// The stable machine code within the class.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::TaskAbsent(_) => "TASK_NOT_FOUND",
            Self::StepAbsent(_) => "PLAN_STEP_NOT_FOUND",
            Self::StepAlreadyRunning { .. } => "PLAN_STEP_ALREADY_RUNNING",
            Self::DefinitionAbsent(_) => "AGENT_DEFINITION_NOT_FOUND",
            Self::DepthExceeded { .. } => "DELEGATION_DEPTH_EXCEEDED",
            Self::MalformedKey(_) => "IDEMPOTENCY_KEY_MALFORMED",
            Self::Store(_) => "DELEGATION_FAILED",
            Self::Construction(_) => "DELEGATION_FAILED",
        }
    }
}

/// The attempt an idempotency key names (`ATH-CORE-035`'s `plan_step_id:attempt` shape).
///
/// The attempt is read from the key rather than counted here: the key is the agent's own
/// statement of which attempt this is, and the store's claim ledger is keyed by it.
fn attempt_of(key: &str, step: &PlanStepId) -> Result<u64, DelegationFault> {
    let expected_prefix = format!("{}:", step.as_str());
    let suffix = key
        .strip_prefix(&expected_prefix)
        .ok_or_else(|| DelegationFault::MalformedKey(key.to_string()))?;
    suffix
        .parse::<u64>()
        .map_err(|_| DelegationFault::MalformedKey(key.to_string()))
}

/// Admit one delegation (`ATH-CORE-035`, `ATH-DATA-010`, `ATH-AGENT-002`, `ATH-HIER-005`).
///
/// `instances_root` is `<ATHENA_HOME>/var/instances` — the caller names the tree
/// (`ATH-OPS-009`), and the Instance's cwd is `<instances_root>/<instance id>`, which is
/// exactly the layout `init_workspace` creates and the retention sweep's recognition fence
/// recognizes (X-41's condition 1). Composing it here rather than in the caller is what
/// keeps those two faces in step.
///
/// The order is deliberate: every read-only refusal is answered *before* the idempotency
/// key is claimed, so a malformed or inadmissible request does not burn its key — a claim
/// is a record that the work ran, and claiming one for a refusal would make the retry
/// impossible. The claim is therefore the last gate before the row is written.
///
/// # Errors
/// [`DelegationFault`] naming the ground.
pub fn accept_delegation(
    store: &mut AthenaStore,
    request: &DelegationRequest,
    instances_root: &std::path::Path,
    now: &str,
) -> Result<DelegationAccepted, DelegationFault> {
    let attempt = attempt_of(&request.idempotency_key, &request.plan_step_id)?;

    // The Task and the step must exist: the row references both, and an Instance for a
    // step no plan holds would be an orphan at birth.
    if TaskRepository::get(store, &request.task_id)
        .map_err(store_fault)?
        .is_none()
    {
        return Err(DelegationFault::TaskAbsent(
            request.task_id.as_str().to_string(),
        ));
    }
    if PlanRepository::get_step(store, &request.plan_step_id)
        .map_err(store_fault)?
        .is_none()
    {
        return Err(DelegationFault::StepAbsent(
            request.plan_step_id.as_str().to_string(),
        ));
    }

    // ATH-DATA-010: one step runs once at a time. The count is the store's own answer, not
    // an inference from Event order.
    let in_flight = AgentRepository::count_non_terminal_for_step(store, &request.plan_step_id)
        .map_err(store_fault)?;
    if in_flight > 0 {
        return Err(DelegationFault::StepAlreadyRunning {
            step: request.plan_step_id.as_str().to_string(),
            count: in_flight,
        });
    }

    // ATH-AGENT-002: the definition is the Instance's parent record. Its level fixes the
    // depth the Instance sits at, and its own ceiling bounds that depth (ATH-HIER-005).
    let definition = AgentRepository::get_definition(store, &request.agent_id)
        .map_err(store_fault)?
        .ok_or_else(|| DelegationFault::DefinitionAbsent(request.agent_id.as_str().to_string()))?;
    let delegation_depth = definition.level.depth();
    if delegation_depth > definition.max_delegation_depth {
        return Err(DelegationFault::DepthExceeded {
            agent: request.agent_id.as_str().to_string(),
            depth: delegation_depth,
            ceiling: definition.max_delegation_depth,
        });
    }

    // The claim is last (see the doc above): a refusal must not burn the key. A key already
    // claimed is a replay — the first execution's Instance is the answer, found by the step
    // and attempt the key names.
    if !IdempotencyRepository::claim(store, &request.idempotency_key).map_err(store_fault)? {
        return replay_of(store, request, attempt, delegation_depth);
    }

    let instance_id = InstanceId::new();
    // The cwd the layout prescribes: `<instances_root>/<instance id>` (ATH-OPS-009), the
    // one shape the area sweep's fence recognizes as ATHENA's own (X-41).
    let workspace = instances_root
        .join(instance_id.as_str())
        .display()
        .to_string();
    let instance = AgentInstance {
        id: instance_id.clone(),
        agent_id: request.agent_id.clone(),
        task_id: Some(request.task_id.clone()),
        plan_step_id: Some(request.plan_step_id.clone()),
        // The superior is the delegating Core Agent, whose own Instance row carries no Task
        // (ATH-STATE-012) and is not a hierarchy parent of this row; the hierarchy's own
        // edges are the Hierarchy aggregate's (ATH-HIER-009), not this field's to guess.
        superior_instance_id: None,
        delegation_depth,
        // §17.2's entry state. Starting it walks `CREATED → STARTING` (`AgentStarting`),
        // which is the Runtime Manager's act, not this module's.
        state: AgentInstanceState::Created,
        runtime_instance_id: None,
        workspace: workspace.clone(),
        // Frozen at start, never changed afterwards (ATH-DATA-006, ATH-SKILL-010).
        skill_set: request.skill_set.clone(),
        // The area reference is recorded once the Runtime has written the area (X-41's
        // `set_aiprj_area`); until then the row states no path rather than a guessed one.
        aiprj_log_ref: None,
        attempt,
        started_at: None,
        finished_at: None,
    };
    AgentRepository::insert_instance(store, &instance).map_err(store_fault)?;

    let event_id = record_delegated(store, request, &instance_id, now)?;
    // The outcome is recorded against the claimed key so a replay answers the same thing
    // (ATH-API-004).
    IdempotencyRepository::record_outcome(store, &request.idempotency_key, instance_id.as_str())
        .map_err(store_fault)?;

    Ok(DelegationAccepted {
        instance_id,
        workspace,
        attempt,
        delegation_depth,
        event_id,
        replayed: false,
    })
}

/// A delegation whose key was already claimed: the first execution's row is the answer
/// (`ATH-NFR-017` — a repeat is a replay, never a second Instance).
///
/// The claimed key's recorded outcome names the Instance; a claim without a readable
/// outcome is reported as a store fault rather than being answered with a guess.
fn replay_of(
    store: &AthenaStore,
    request: &DelegationRequest,
    attempt: u64,
    delegation_depth: u64,
) -> Result<DelegationAccepted, DelegationFault> {
    let outcome = IdempotencyRepository::outcome(store, &request.idempotency_key)
        .map_err(store_fault)?
        .ok_or_else(|| {
            DelegationFault::Store(format!(
                "idempotency key {} is claimed with no recorded outcome",
                request.idempotency_key
            ))
        })?;
    // The recorded outcome is rehydrated through the prefix-checked parse: a stored string
    // in the wrong shape is a store fault, never silently a valid identifier.
    let instance_id = InstanceId::parse(outcome.clone()).ok_or_else(|| {
        DelegationFault::Store(format!(
            "idempotency key {} records {outcome:?}, which is not an instance id",
            request.idempotency_key
        ))
    })?;
    let instance = AgentRepository::get_instance(store, &instance_id)
        .map_err(store_fault)?
        .ok_or_else(|| {
            DelegationFault::Store(format!(
                "idempotency key {} names instance {}, which the store does not hold",
                request.idempotency_key,
                instance_id.as_str()
            ))
        })?;
    Ok(DelegationAccepted {
        instance_id: instance.id,
        workspace: instance.workspace,
        attempt,
        delegation_depth,
        event_id: EventId::new(),
        replayed: true,
    })
}

/// Record the delegation (`AgentDelegated`, the §23.2 Hierarchy family) on the Task's own
/// correlation chain (`ATH-OBS-009`).
fn record_delegated(
    store: &mut AthenaStore,
    request: &DelegationRequest,
    instance_id: &InstanceId,
    now: &str,
) -> Result<EventId, DelegationFault> {
    let correlation = EventRepository::list_for_task(store, &request.task_id)
        .map_err(store_fault)?
        .into_iter()
        .rev()
        .map(|event| event.correlation_id)
        .find(|key| !key.is_empty())
        .unwrap_or_else(|| athena_events::redact::digest(&format!("delegation-{now}")));

    let typed = athena_events::TypedEvent::new(
        athena_events::events::AgentDelegated {},
        athena_events::EventContext {
            event_id: EventId::new(),
            occurred_at: now.to_string(),
            actor: athena_events::envelope::Actor::System,
            task_id: Some(request.task_id.clone()),
            plan_step_id: Some(request.plan_step_id.clone()),
            agent_id: Some(request.agent_id.clone()),
            instance_id: Some(instance_id.clone()),
            skill_id: None,
            correlation_id: correlation,
            causation_id: None,
        },
    )
    .map_err(|fault| DelegationFault::Construction(fault.to_string()))?;

    let stored = EventRepository::append(
        store,
        &NewEvent {
            event_id: typed.event_id.clone(),
            schema_version: typed.schema_version,
            event_type: typed.event_type.to_string(),
            occurred_at: typed.occurred_at.clone(),
            task_id: typed.task_id.clone(),
            plan_step_id: typed.plan_step_id.clone(),
            agent_id: typed.agent_id.clone(),
            instance_id: typed.instance_id.clone(),
            skill_id: typed.skill_id.clone(),
            correlation_id: typed.correlation_id.clone(),
            causation_id: typed.causation_id.clone(),
            actor: typed.actor.as_str().to_string(),
            payload_json: typed.payload_json.clone(),
        },
    )
    .map_err(store_fault)?;
    Ok(stored.event_id)
}

fn store_fault(fault: athena_persistence::error::PersistenceError) -> DelegationFault {
    DelegationFault::Store(fault.to_string())
}

/// The agent-facing answer for an admitted delegation (`ATH-CORE-034`). The payload names
/// the Instance the agent's next message addresses and the Workspace it runs in, plus the
/// `replayed` fact — an agent that re-sent a claimed key learns that it did, rather than
/// believing it started a second Instance.
#[must_use]
pub fn acceptance_response(
    request_id: &str,
    accepted: &DelegationAccepted,
) -> crate::protocol::CoreResponse {
    crate::protocol::CoreResponse::Ok {
        request_id: request_id.to_string(),
        data: serde_json::json!({
            "instance_id": accepted.instance_id.as_str(),
            "workspace": accepted.workspace,
            "attempt": accepted.attempt,
            "delegation_depth": accepted.delegation_depth,
            "replayed": accepted.replayed,
        }),
    }
}

/// The agent-facing answer for a refused delegation (`ATH-CORE-034`, §20.1's envelope). The
/// class and code are the fault's own.
#[must_use]
pub fn refusal_response(
    request_id: &str,
    correlation_id: &str,
    fault: &DelegationFault,
) -> crate::protocol::CoreResponse {
    // The one structured fact a refusal can act on: how many Instances hold the step, when
    // the single-flight rule refused. The agent's correction is to wait or re-plan, and the
    // count is what tells it which.
    let details = match fault {
        DelegationFault::StepAlreadyRunning { count, .. } => {
            serde_json::json!({ "non_terminal_instances": count })
        }
        DelegationFault::DepthExceeded { depth, ceiling, .. } => {
            serde_json::json!({ "depth": depth, "ceiling": ceiling })
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
