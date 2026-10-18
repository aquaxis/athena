//! Issuance-path behaviour (ATH-CORE-041/042, R-CORE-27/28, ATH-PLAN-004/005/006).

use crate::issuance::{IssuedPlan, PlanRefusal, PlanSubmission, submit_plan};
use athena_persistence::db::now_rfc3339;
use athena_persistence::repository::{PlanRepository, TaskRepository};
use athena_types::{PlanStepDraft, Task, TaskId};

/// A fresh backing store per test case, opened in memory, with one seeded `Task` row —
/// the `plans` table references `tasks`, so issuance needs the owning Task to exist.
struct BackedStore {
    store: athena_persistence::SqliteStore,
    task_id: TaskId,
}

impl BackedStore {
    fn new() -> Self {
        let mut store =
            athena_persistence::SqliteStore::in_memory().expect("in-memory store opens");
        let task = Task {
            id: TaskId::new(),
            parent_id: None,
            title: "Issuance test".into(),
            instruction: "seed the owning row".into(),
            workspace: "/work/issuance-test".into(),
            goal: None,
            constraints: vec![],
            completion_criteria: vec![],
            state: athena_types::TaskState::Created,
            priority: athena_types::Priority::Normal,
            plan_id: None,
            risk_level: athena_types::RiskLevel::Medium,
            attempt_count: 0,
            max_attempts: 3,
            iteration_count: 0,
            max_iterations: 50,
            deadline: None,
            created_by: "test".into(),
            created_at: now_rfc3339(),
            updated_at: now_rfc3339(),
            finished_at: None,
        };
        TaskRepository::insert(&mut store, &task).expect("the owning Task seeds");
        Self {
            store,
            task_id: task.id,
        }
    }
}

/// Construct one draft.
#[must_use]
fn draft(order: u64, intent: impl Into<String>, depends_on: Vec<u64>) -> PlanStepDraft {
    PlanStepDraft {
        order,
        intent: intent.into(),
        depends_on_orders: depends_on,
        assigned_agent: None,
        skill_set: vec![],
    }
}

/// A submission over the seeded Task at `base_revision` 0.
#[must_use]
fn submission(
    store: &BackedStore,
    base_revision: u64,
    steps: Vec<PlanStepDraft>,
) -> PlanSubmission {
    PlanSubmission {
        task_id: store.task_id.clone(),
        base_revision,
        goal: "produce the report".into(),
        steps,
    }
}

/// The plan a base-revision case seeds first (revision 1).
fn first_plan(store: &mut BackedStore) -> IssuedPlan {
    let initial = submission(store, 0, vec![draft(0, "initial step", vec![])]);
    submit_plan(&mut store.store, initial, "2026-10-14T00:00:00.000Z")
        .expect("the first plan issues when base_revision is 0")
}

#[test]
fn the_first_plan_starts_from_revision_one() {
    // ATH-CORE-042: Core Host increments; the agent bases on 0 when no Plan exists.
    let owned = BackedStore::new();
    let mut owned = owned;
    let issued = first_plan(&mut owned);
    assert_eq!(issued.plan.revision, 1);
    assert!(issued.first_revision);
    assert_eq!(issued.plan.task_id, owned.task_id);
    assert_eq!(issued.plan.steps.len(), 1);
}

#[test]
fn a_stale_base_revision_is_refused_as_validation() {
    // ATH-CORE-042 / R-CORE-28.
    let mut store = BackedStore::new();
    first_plan(&mut store);
    let stale = submission(&store, 0, vec![draft(0, "a step", vec![])]);

    let refusal =
        submit_plan(&mut store.store, stale, "2026-10-14T00:00:01.000Z").expect_err("refused");
    assert!(
        matches!(
            refusal,
            PlanRefusal::BaseRevisionConflict {
                submitted: 0,
                current: 1
            }
        ),
        "{refusal}"
    );
    assert_eq!(
        refusal.class(),
        crate::protocol::ErrorClass::ValidationError
    );
    assert_eq!(refusal.code(), "PLAN_BASE_REVISION_CONFLICT");
    assert_eq!(refusal.current_revision(), Some(1));
}

#[test]
fn a_current_base_revision_is_accepted_and_bumps_the_revision() {
    let mut store = BackedStore::new();
    first_plan(&mut store);
    let replanned = submission_owned(
        &store,
        1,
        vec![
            draft(0, "constraint analysis", vec![]),
            draft(1, "verify", vec![0]),
        ],
    );
    let issued =
        submit_plan(&mut store.store, replanned, "2026-10-14T00:00:01.000Z").expect("issued");
    assert_eq!(
        issued.plan.revision, 2,
        "Core Host increments (ATH-CORE-042)"
    );
    assert!(
        !issued.first_revision,
        "the caller records PlanRevised, not PlanCreated"
    );
    // Both revisions are retained — the Plan is never overwritten (ATH-PLAN-004).
    assert_eq!(
        PlanRepository::revisions_for_task(&store.store, &store.task_id).expect("live list"),
        vec![1, 2]
    );
}

#[test]
fn issuance_mints_core_host_identifiers_only() {
    // ATH-CORE-041: the drafts carry no identifier; the store's rows do, and they were
    // minted here.
    let mut owned_store = BackedStore::new();
    let only = submission(&owned_store, 0, vec![draft(0, "the only step", vec![])]);
    let issued =
        submit_plan(&mut owned_store.store, only, "2026-10-14T00:00:00.000Z").expect("issued");
    let plan = PlanRepository::get(&owned_store.store, &issued.plan.id)
        .expect("fetch")
        .expect("present");
    assert!(plan.id.as_str().starts_with("plan-"));
    for step in &plan.steps {
        assert!(step.id.as_str().starts_with("step-"));
    }
}

/// A submission built before the mutable store borrow.
fn submission_owned(store: &BackedStore, base: u64, steps: Vec<PlanStepDraft>) -> PlanSubmission {
    submission(store, base, steps)
}

#[test]
fn draft_dependencies_resolve_to_core_minted_step_ids() {
    let mut owned = BackedStore::new();
    let chained = submission_owned(
        &owned,
        0,
        vec![
            draft(0, "first", vec![]),
            draft(1, "second", vec![0]),
            draft(2, "third", vec![0, 1]),
        ],
    );
    let issued =
        submit_plan(&mut owned.store, chained, "2026-10-14T00:00:00.000Z").expect("issued");
    let plan = PlanRepository::get(&owned.store, &issued.plan.id)
        .expect("fetch")
        .expect("present");
    let by_order: std::collections::HashMap<u64, &athena_types::PlanStepId> = plan
        .steps
        .iter()
        .map(|step| (step.order, &step.id))
        .collect();
    // `depends_on` is set-like: the store hydrates the relation ordered by identifier
    // (`plan_step_deps`), so membership — not list order — is the contract.
    let second = plan
        .steps
        .iter()
        .find(|s| s.order == 1)
        .expect("second step");
    assert_eq!(second.depends_on, vec![by_order[&0].clone()]);
    let third = plan
        .steps
        .iter()
        .find(|s| s.order == 2)
        .expect("third step");
    // Compare as id sets, since the store hydrates the relation ordered by identifier.
    let mut expected = vec![by_order[&0].clone(), by_order[&1].clone()];
    expected.sort();
    let mut actual = third.depends_on.clone();
    actual.sort();
    assert_eq!(actual, expected);
    // The referenced ids are the ones inside *this* plan — no cross-revision linking.
    assert!(
        by_order
            .values()
            .all(|id| plan.steps.iter().any(|s| &s.id == *id))
    );
}

#[test]
fn a_cycle_is_refused_before_any_row_is_written() {
    // ATH-PLAN-006: validate at creation; when refused, nothing is stored.
    let mut owned = BackedStore::new();
    let cyclic = submission(
        &owned,
        0,
        vec![
            draft(0, "a", vec![2]),
            draft(1, "b", vec![0]),
            draft(2, "c", vec![1]),
        ],
    );
    let refusal =
        submit_plan(&mut owned.store, cyclic, "2026-10-14T00:00:00.000Z").expect_err("refused");
    let cycle = match refusal {
        PlanRefusal::DependencyCycle(orders) => orders,
        other => panic!("{other}"),
    };
    assert_eq!(cycle, vec![0, 1, 2]);
    // Validation precedes registration; the store holds no rows from the refused drafts.
    assert!(
        PlanRepository::latest_for_task(&owned.store, &owned.task_id)
            .expect("fetch")
            .is_none(),
        "a refused plan must leave nothing behind"
    );
}

#[test]
fn duplicate_orders_are_refused() {
    let mut owned = BackedStore::new();
    let colliding = submission(
        &owned,
        0,
        vec![draft(0, "a", vec![]), draft(0, "b", vec![])],
    );
    let refusal =
        submit_plan(&mut owned.store, colliding, "2026-10-14T00:00:00.000Z").expect_err("refused");
    assert!(
        matches!(refusal, PlanRefusal::DuplicateOrder(0)),
        "{refusal}"
    );
}

#[test]
fn an_unknown_dependency_is_refused() {
    let mut owned = BackedStore::new();
    let dangling = submission(&owned, 0, vec![draft(0, "a", vec![7])]);
    let refusal =
        submit_plan(&mut owned.store, dangling, "2026-10-14T00:00:00.000Z").expect_err("refused");
    assert!(
        matches!(refusal, PlanRefusal::UnknownDependency(7)),
        "{refusal}"
    );
}

#[test]
fn an_empty_plan_is_refused() {
    let mut owned = BackedStore::new();
    let voided = submission(&owned, 0, vec![]);
    let refusal =
        submit_plan(&mut owned.store, voided, "2026-10-14T00:00:00.000Z").expect_err("refused");
    assert!(matches!(refusal, PlanRefusal::EmptyPlan), "{refusal}");
    // And like every structural refusal, it leaves no rows behind.
    assert!(
        PlanRepository::latest_for_task(&owned.store, &owned.task_id)
            .expect("fetch")
            .is_none()
    );
}
