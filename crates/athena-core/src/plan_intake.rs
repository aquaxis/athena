//! The Plan intake path (X-43): a Core Agent's `submit_plan` becomes a stored Plan
//! revision and the Event that records its confirmation.
//!
//! `issuance::submit_plan` has always held the *semantics* — identifier minting
//! (`ATH-CORE-041`), the revision rule (`ATH-CORE-042`), dependency resolution and the
//! acyclicity check (`ATH-PLAN-006`) — but nothing called it: a Core Agent that submitted a
//! plan had its submission observed as an intention and its Task settled as if the
//! submission were the answer. This module is the missing half: it accepts the submission,
//! records the confirmation Event the specification requires (`PlanCreated` on a Task's
//! first plan, `PlanRevised` afterwards — `ATH-PLAN-004`/`ATH-PLAN-005`), and hands the
//! caller the response to return to the agent (`ATH-CORE-034`).
//!
//! The same cadence/semantics split the rest of the core keeps (D-35): this module owns the
//! rule and touches the store; the resident owns the clock and the message IO.
//!
//! **Where the Event's `rationale` comes from.** The §23.2 catalog makes `rationale` a
//! mandatory key of `PlanCreated`/`PlanRevised`, while `CoreRequest::SubmitPlan` carries no
//! such field (spec §7.5.5) — so the value cannot come from the submission. It is read from
//! the **Task row**: its `goal` when the row states one, else its `instruction`. Both are
//! recorded facts about the work the plan serves, and [`PlanAccepted::rationale_source`]
//! reports which one was used, so a reader is never left guessing (`ATH-GEN-004`: nothing
//! is invented, and the substitution is disclosed rather than silent).

use crate::issuance::{PlanRefusal, PlanSubmission, submit_plan};
use athena_persistence::repository::{EventRepository, TaskRepository};
use athena_persistence::{AthenaStore, NewEvent};
use athena_types::{EventId, PlanId, PlanStepDraft, TaskId};

/// Which Task field supplied the confirmation Event's `rationale`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RationaleSource {
    /// The Task row stated a `goal`.
    TaskGoal,
    /// The Task row stated no goal; its `instruction` was used.
    TaskInstruction,
}

impl RationaleSource {
    /// The wire word for logs and records.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TaskGoal => "task.goal",
            Self::TaskInstruction => "task.instruction",
        }
    }
}

/// One accepted submission: what landed, and what the caller answers the agent with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanAccepted {
    /// The Plan Core Host minted.
    pub plan_id: PlanId,
    /// The revision Core Host stamped (`ATH-CORE-042`).
    pub revision: u64,
    /// How many steps the revision holds.
    pub step_count: u64,
    /// Whether this was the Task's first plan — `PlanCreated` was recorded, else
    /// `PlanRevised` (`ATH-PLAN-005`).
    pub first_revision: bool,
    /// The confirmation Event's identifier.
    pub event_id: EventId,
    /// Which Task field supplied the Event's `rationale`.
    pub rationale_source: RationaleSource,
}

/// Why an intake did not land.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IntakeFault {
    /// The submission itself was refused; the envelope the agent receives is built from
    /// this refusal's own class and code (the issuance module owns that vocabulary).
    #[error(transparent)]
    Refused(#[from] PlanRefusal),
    /// The Task the submission names is not in the store, so no plan can belong to it and
    /// no rationale can be read.
    #[error("task {0} is absent")]
    TaskAbsent(String),
    /// The store refused a read or the Event append; surfaced whole.
    #[error("store fault: {0}")]
    Store(String),
    /// The confirmation Event could not be constructed against the catalog (ATH-EVENT-017).
    #[error("event construction: {0}")]
    Construction(String),
}

impl IntakeFault {
    /// The §20.1 class of this fault. A refusal keeps issuance's own classification; an
    /// absent Task is a validation fault; a store or catalog fault is internal and never
    /// retried (`ATH-FAIL-002`).
    #[must_use]
    pub fn class(&self) -> crate::protocol::ErrorClass {
        match self {
            Self::Refused(refusal) => refusal.class(),
            Self::TaskAbsent(_) => crate::protocol::ErrorClass::ValidationError,
            Self::Store(_) | Self::Construction(_) => crate::protocol::ErrorClass::InternalError,
        }
    }

    /// The stable machine code within the class — a refusal's own code where it has one.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Refused(refusal) => refusal.code(),
            Self::TaskAbsent(_) => "TASK_NOT_FOUND",
            Self::Store(_) | Self::Construction(_) => "PLAN_INTAKE_FAILED",
        }
    }
}

/// Accept one `submit_plan` (`ATH-PLAN-001`/`004`/`005`, `ATH-CORE-041`/`042`).
///
/// The order is the one `ATH-PERS-003` wants: the plan row lands first (through
/// `issuance::submit_plan`, which performs every validation), then the confirmation Event
/// is appended — `PlanCreated` for a Task's first plan, `PlanRevised` for every later
/// revision. A refusal appends nothing: the caller records the denial and answers the agent.
///
/// The Task's own state is **not** touched. A plan submission is not an answer, and the
/// caller must not settle the Task on it — that conflation is what this path replaces.
///
/// # Errors
/// [`IntakeFault`] — a refusal surfaced from issuance, an absent Task, a store fault, or a
/// catalog construction fault.
pub fn accept_plan(
    store: &mut AthenaStore,
    task_id: &TaskId,
    base_revision: u64,
    steps: Vec<PlanStepDraft>,
    now: &str,
) -> Result<PlanAccepted, IntakeFault> {
    // The Task row is read first: it supplies the goal the plan serves and the rationale
    // the confirmation Event must carry. An absent Task is refused before any write.
    let task = TaskRepository::get(store, task_id)
        .map_err(|fault| IntakeFault::Store(fault.to_string()))?
        .ok_or_else(|| IntakeFault::TaskAbsent(task_id.as_str().to_string()))?;
    let (rationale, rationale_source) = match task.goal.as_deref() {
        Some(goal) if !goal.trim().is_empty() => (goal.to_string(), RationaleSource::TaskGoal),
        _ => (task.instruction.clone(), RationaleSource::TaskInstruction),
    };

    let step_count = steps.len() as u64;
    let issued = submit_plan(
        store,
        PlanSubmission {
            task_id: task_id.clone(),
            base_revision,
            goal: rationale.clone(),
            steps,
        },
        now,
    )?;

    let event_id = record_confirmation(store, &issued, &rationale, step_count, now)?;
    Ok(PlanAccepted {
        plan_id: issued.plan.id.clone(),
        revision: issued.plan.revision,
        step_count,
        first_revision: issued.first_revision,
        event_id,
        rationale_source,
    })
}

/// Append the confirmation Event the revision's standing demands (`ATH-PLAN-005`, D-1: the
/// Event is Core Host's to record, after the store commit).
fn record_confirmation(
    store: &mut AthenaStore,
    issued: &crate::issuance::IssuedPlan,
    rationale: &str,
    step_count: u64,
    now: &str,
) -> Result<EventId, IntakeFault> {
    let task_id = issued.plan.task_id.clone();
    // The correlation key rides the Task's own chain (ATH-OBS-009): the last non-empty
    // correlation the Task's Events carry. A Task whose chain cannot be read gets a
    // derived key rather than an invented identifier.
    let correlation = EventRepository::list_for_task(store, &task_id)
        .map_err(|fault| IntakeFault::Store(fault.to_string()))?
        .into_iter()
        .rev()
        .map(|event| event.correlation_id)
        .find(|key| !key.is_empty())
        .unwrap_or_else(|| athena_events::redact::digest(&format!("plan-intake-{now}")));

    let context = athena_events::EventContext {
        event_id: EventId::new(),
        occurred_at: now.to_string(),
        actor: athena_events::envelope::Actor::System,
        task_id: Some(task_id),
        plan_step_id: None,
        agent_id: None,
        instance_id: None,
        skill_id: None,
        correlation_id: correlation,
        causation_id: None,
    };
    // One shape, two names: the catalog gives `PlanCreated` and `PlanRevised` identical
    // mandatory keys, and `first_revision` is which of the two the standing calls for.
    let typed = if issued.first_revision {
        athena_events::TypedEvent::new(
            athena_events::events::PlanCreated {
                plan_id: issued.plan.id.clone(),
                revision: issued.plan.revision,
                step_count,
                rationale: rationale.to_string(),
            },
            context,
        )
        .map(into_new_event)
    } else {
        athena_events::TypedEvent::new(
            athena_events::events::PlanRevised {
                plan_id: issued.plan.id.clone(),
                revision: issued.plan.revision,
                step_count,
                rationale: rationale.to_string(),
            },
            context,
        )
        .map(into_new_event)
    }
    .map_err(|fault| IntakeFault::Construction(fault.to_string()))?;

    let stored = EventRepository::append(store, &typed)
        .map_err(|fault| IntakeFault::Store(fault.to_string()))?;
    Ok(stored.event_id)
}

/// The typed envelope flattened into the store's append row (the same transcription the
/// other emitters use).
fn into_new_event<T: athena_events::Payload>(typed: athena_events::TypedEvent<T>) -> NewEvent {
    NewEvent {
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
    }
}

/// The agent-facing answer for an accepted submission (`ATH-CORE-034`: every `CoreRequest`
/// is answered). The data payload names what Core Host minted — the identifiers the agent
/// may reference next, and the revision it must base its following edit on
/// (`ATH-CORE-042`).
#[must_use]
pub fn acceptance_response(
    request_id: &str,
    accepted: &PlanAccepted,
) -> crate::protocol::CoreResponse {
    crate::protocol::CoreResponse::Ok {
        request_id: request_id.to_string(),
        data: serde_json::json!({
            "plan_id": accepted.plan_id.as_str(),
            "revision": accepted.revision,
            "step_count": accepted.step_count,
            "first_revision": accepted.first_revision,
        }),
    }
}

/// Wrap one `CoreResponse` as the message the Core Agent receives (`ATH-CORE-034`'s answer
/// leg). The kind is `control_response` from the §10.1 set — the response to a privileged
/// request is control traffic, not an instruction — and it rides the Task's own delegation
/// thread (`ATH-MSG-004`: one thread per delegation), so request and answer sit on the same
/// chain. Core is transport here, never an endpoint (`ATH-MSG-011`), which is why the
/// sender fields stay `None`.
///
/// # Errors
/// The response's own serialization fault, surfaced whole.
pub fn control_response_message(
    task_id: &TaskId,
    response: &crate::protocol::CoreResponse,
) -> Result<athena_runtime::Message, serde_json::Error> {
    let payload = response.to_payload_json()?;
    Ok(athena_runtime::Message {
        message_id: athena_types::MessageId::new(),
        message_type: athena_types::MessageType::ControlResponse,
        from_agent: None,
        from_instance_id: None,
        thread_id: crate::core_loop::core_thread(task_id),
        reply_to: None,
        priority: athena_types::Priority::Normal,
        // The body stays the structured payload's own text: a control response's content
        // is its payload, and inventing prose around it would put words in Core's mouth.
        body: payload.clone(),
        payload_json: Some(payload),
        deadline: None,
    })
}

/// The agent-facing answer for a refusal (`ATH-CORE-034`, §20.1's envelope). The class and
/// code are the refusal's own — this function renames nothing.
#[must_use]
pub fn refusal_response(
    request_id: &str,
    correlation_id: &str,
    fault: &IntakeFault,
) -> crate::protocol::CoreResponse {
    let (class, code, message) = (fault.class(), fault.code(), fault.to_string());
    // The details object carries the one structured fact a refusal can act on: the
    // revision Core Host currently holds, when the refusal was a base-revision conflict
    // (`ATH-CORE-042` — the agent needs it to re-base). Nothing else is added; the
    // envelope's details must stay secret-free (ATH-EVENT-007 by analogy).
    let details = match fault {
        IntakeFault::Refused(refusal) => match refusal.current_revision() {
            Some(current) => serde_json::json!({ "current_revision": current }),
            None => serde_json::Value::Null,
        },
        _ => serde_json::Value::Null,
    };
    crate::protocol::CoreResponse::Err {
        request_id: request_id.to_string(),
        error: crate::protocol::ErrorEnvelope {
            class,
            code: code.to_string(),
            message,
            details,
            correlation_id: correlation_id.to_string(),
        },
    }
}
