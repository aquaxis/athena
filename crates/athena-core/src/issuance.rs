//! Core-Host-only identifier issuance (ATH-CORE-041, R-CORE-27) and the Plan revision
//! path (ATH-CORE-042, R-CORE-28).
//!
//! The agent submits *drafts* (`PlanStepDraft` — the type has no identifier fields, D-5);
//! this module mints the `PlanId` / `PlanStepId` values, resolves draft dependencies (by
//! `order`, within the same submission), and stamps the new revision. Identifiers for the
//! aggregates an agent later references — `InstanceId`, `ResultId`, `ApprovalId` — are
//! minted where those aggregates come into being, which is equally Core Host.
//!
//! Revision rule (ATH-CORE-042): a `SubmitPlan` carries the revision the agent edited
//! against (`base_revision`). Core Host rejects any submission whose `base_revision` is
//! not the current revision, as `ValidationError` — this is what keeps two racing plans
//! from both landing. A Task's first plan starts from revision 0 (nothing exists yet) and
//! becomes revision 1; later submissions must base on that.
//!
//! Cycle check (ATH-PLAN-006, R-PLN-04): `depends_on` must be acyclic, validated at
//! creation — here, before any row is written.
//!
//! Event attribution (ATH-PLAN-005, D-1): the returned [`IssuedPlan::first_revision`] flag
//! tells the caller whether to record `PlanCreated` or `PlanRevised`, through the Event
//! Manager after the store commit (ATH-PERS-003).

use crate::protocol::ErrorClass;
use athena_persistence::repository::PlanRepository;
use athena_types::{Plan, PlanId, PlanStep, PlanStepDraft, PlanStepId, PlanStepState, TaskId};
use std::collections::HashMap;

/// A `SubmitPlan` translated into Core Host's terms.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanSubmission {
    /// The Task the plan belongs to.
    pub task_id: TaskId,
    /// The revision the agent believes is current (ATH-CORE-042).
    pub base_revision: u64,
    /// The objective, as Core Agent stated it during Understand.
    pub goal: String,
    /// Draft steps, without identifiers (D-5).
    pub steps: Vec<PlanStepDraft>,
}

/// Why a submission did not reach the store.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanRefusal {
    /// `base_revision` did not match the current revision (ATH-CORE-042).
    #[error("base_revision conflict: submitted {submitted}, current {current}")]
    BaseRevisionConflict {
        /// What the agent edited against.
        submitted: u64,
        /// What Core Host currently holds.
        current: u64,
    },
    /// Two drafts claimed the same `order`, which makes dependency references ambiguous.
    #[error("duplicate draft order {0}")]
    DuplicateOrder(u64),
    /// A draft depends on an `order` this submission does not define.
    #[error("draft depends on order {0}, which this submission does not define")]
    UnknownDependency(u64),
    /// `depends_on` forms a cycle; untangling it is a Core Agent decision, not a Core
    /// Host edit (ATH-PLAN-006).
    #[error("depends_on contains a cycle through orders {0:?}")]
    DependencyCycle(Vec<u64>),
    /// A plan with no steps cannot drive execution.
    #[error("a plan must carry at least one step")]
    EmptyPlan,
    /// The store refused the write — any expected refusals were handled before reaching it
    /// (`ATH-PLAN-004` uniqueness is the backstop). Reported as `InternalError`.
    #[error("plan store refused the committed write: {0}")]
    StoreFailure(String),
}

impl PlanRefusal {
    /// The §20.1 class this refusal reports.
    #[must_use]
    pub const fn class(&self) -> ErrorClass {
        match self {
            Self::BaseRevisionConflict { .. }
            | Self::DuplicateOrder(_)
            | Self::UnknownDependency(_)
            | Self::DependencyCycle(_)
            | Self::EmptyPlan => ErrorClass::ValidationError,
            Self::StoreFailure(_) => ErrorClass::InternalError,
        }
    }

    /// A stable machine code within the class.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::BaseRevisionConflict { .. } => "PLAN_BASE_REVISION_CONFLICT",
            Self::DuplicateOrder(_) => "PLAN_DRAFT_DUPLICATE_ORDER",
            Self::UnknownDependency(_) => "PLAN_DRAFT_UNKNOWN_DEPENDENCY",
            Self::DependencyCycle(_) => "PLAN_DEPENDENCY_CYCLE",
            Self::EmptyPlan => "PLAN_EMPTY",
            Self::StoreFailure(_) => "PLAN_STORE_FAILURE",
        }
    }

    /// The revision Core Host was holding, where the refusal carries one.
    #[must_use]
    pub const fn current_revision(&self) -> Option<u64> {
        match self {
            Self::BaseRevisionConflict { current, .. } => Some(*current),
            _ => None,
        }
    }
}

/// What a successful submission produced.
#[derive(Debug, Clone, PartialEq)]
pub struct IssuedPlan {
    /// The persisted plan, at its new revision.
    pub plan: Plan,
    /// Whether this was the Task's first plan (drives `PlanCreated` vs `PlanRevised`).
    pub first_revision: bool,
}

/// Resolve a draft submission into Core-minted identifiers and persist the plan.
///
/// Event emission belongs to the caller through the Event Manager after the store commit
/// (ATH-PERS-003, D-1) — see [`IssuedPlan::first_revision`].
///
/// # Errors
/// [`PlanRefusal::BaseRevisionConflict`] when the agent edited against a revision that is
/// no longer current, and the structural refusals of the drafts; on a store-level
/// uniqueness race, `PlanRefusal::StoreFailure`.
pub fn submit_plan(
    store: &mut dyn PlanRepository,
    submission: PlanSubmission,
    created_at: &str,
) -> Result<IssuedPlan, PlanRefusal> {
    let current = store
        .latest_for_task(&submission.task_id)
        .map_err(PlanRefusal::store_fault)?
        .map(|plan| plan.revision)
        .unwrap_or(0);
    let first_revision = current == 0;

    if submission.base_revision != current {
        return Err(PlanRefusal::BaseRevisionConflict {
            submitted: submission.base_revision,
            current,
        });
    }

    build_plan(store, submission, current + 1, first_revision, created_at)
}

/// Build the plan rows from the drafts: identifiers, dependency resolution, cycle check.
fn build_plan(
    store: &mut dyn PlanRepository,
    submission: PlanSubmission,
    revision: u64,
    first_revision: bool,
    created_at: &str,
) -> Result<IssuedPlan, PlanRefusal> {
    if submission.steps.is_empty() {
        return Err(PlanRefusal::EmptyPlan);
    }

    // Orders index the submission: duplicates make dependency references ambiguous.
    let mut by_order: HashMap<u64, &PlanStepDraft> = HashMap::new();
    for draft in &submission.steps {
        if by_order.insert(draft.order, draft).is_some() {
            return Err(PlanRefusal::DuplicateOrder(draft.order));
        }
    }

    // Every dependency must name an order in this submission (drafts cannot reach across
    // revisions: a prior-revision step's identity stays available to the agent only
    // through what Core Host told it already).
    for draft in &submission.steps {
        for order in &draft.depends_on_orders {
            if !by_order.contains_key(order) {
                return Err(PlanRefusal::UnknownDependency(*order));
            }
        }
    }

    // Cycle check (ATH-PLAN-006): Kahn's algorithm over the draft graph. Any order the
    // sweep does not settle belongs to a cycle (unknown orders were already refused).
    let mut indegree: HashMap<u64, usize> = submission
        .steps
        .iter()
        .map(|draft| (draft.order, draft.depends_on_orders.len()))
        .collect();
    let mut queue: Vec<u64> = submission
        .steps
        .iter()
        .filter(|draft| draft.depends_on_orders.is_empty())
        .map(|draft| draft.order)
        .collect();
    let mut settled: Vec<u64> = Vec::with_capacity(submission.steps.len());
    while let Some(order) = queue.pop() {
        settled.push(order);
        for (candidate, draft) in &by_order {
            if draft.depends_on_orders.contains(&order)
                && let Some(count) = indegree.get_mut(candidate)
                && {
                    *count -= 1;
                    true
                }
                && *count == 0
            {
                queue.push(*candidate);
            }
        }
    }

    if settled.len() < submission.steps.len() {
        let mut wedge: Vec<u64> = submission
            .steps
            .iter()
            .filter(|draft| !settled.contains(&draft.order))
            .map(|draft| draft.order)
            .collect();
        wedge.sort_unstable();
        return Err(PlanRefusal::DependencyCycle(wedge));
    }

    let plan_id = PlanId::new();
    let mut issued: HashMap<u64, PlanStepId> = HashMap::new();
    for draft in &submission.steps {
        issued.insert(draft.order, PlanStepId::new());
    }

    let steps: Vec<PlanStep> = submission
        .steps
        .iter()
        .map(|draft| PlanStep {
            id: issued[&draft.order].clone(),
            plan_id: plan_id.clone(),
            order: draft.order,
            intent: draft.intent.clone(),
            depends_on: draft
                .depends_on_orders
                .iter()
                .map(|order| issued[order].clone())
                .collect(),
            assigned_agent: draft.assigned_agent.clone(),
            skill_set: draft.skill_set.clone(),
            state: PlanStepState::Pending,
            result_id: None,
        })
        .collect();

    // A draft's proposed agent is a suggestion; its acceptance is ComposeHierarchy's job
    // (ATH-AGENT-004), so the issued step keeps the proposal recorded until then.
    let plan = Plan {
        id: plan_id,
        task_id: submission.task_id.clone(),
        revision,
        goal: submission.goal.clone(),
        steps,
        created_at: created_at.into(),
        superseded_by: None,
    };
    store.insert(&plan).map_err(|err| {
        // `plans` carry a `(task_id, revision)` uniqueness constraint — the versioning
        // backstop (ATH-PLAN-004). Reaching it here means a race after inspection, not a
        // second current-revision pass.
        PlanRefusal::StoreFailure(format!("{err}"))
    })?;

    Ok(IssuedPlan {
        plan,
        first_revision,
    })
}

impl PlanRefusal {
    fn store_fault(err: athena_persistence::error::PersistenceError) -> Self {
        Self::StoreFailure(format!("{err}"))
    }
}

#[cfg(test)]
#[path = "issuance_tests.rs"]
mod issuance_tests;
