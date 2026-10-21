//! Temporary diagnostic: the lease flow, instrumented.
#[cfg(test)]
mod debug_lease {
    use crate::AthenaStore;
    use crate::db::now_rfc3339;
    use crate::repository::{TaskRepository, WorkspaceLeaseRepository};
    use athena_types::{Priority, RiskLevel, Task, TaskId, TaskState};

    fn task() -> Task {
        Task {
            id: TaskId::new(),
            parent_id: None,
            title: "t".into(),
            instruction: "i".into(),
            workspace: "/w".into(),
            goal: None,
            constraints: vec![],
            completion_criteria: vec![],
            state: TaskState::Created,
            priority: Priority::High,
            plan_id: None,
            risk_level: RiskLevel::Medium,
            attempt_count: 0,
            max_attempts: 3,
            iteration_count: 0,
            max_iterations: 50,
            deadline: None,
            created_by: "u".into(),
            created_at: now_rfc3339(),
            updated_at: now_rfc3339(),
            finished_at: None,
        }
    }

    #[test]
    fn debug_flow() {
        let mut s = AthenaStore::in_memory().expect("store");
        let t1 = task();
        let t2 = task();
        TaskRepository::insert(&mut s, &t1).expect("1");
        TaskRepository::insert(&mut s, &t2).expect("2");
        eprintln!("first={:?} second={:?}", t1.id, t2.id);
        eprintln!(
            "acq1 = {:?}",
            WorkspaceLeaseRepository::acquire(&mut s, "/w", &t1.id)
        );
        eprintln!("holder = {:?}", WorkspaceLeaseRepository::holder(&s, "/w"));
        eprintln!(
            "release = {:?}",
            WorkspaceLeaseRepository::release(&mut s, "/w")
        );
        eprintln!(
            "holder-after-release = {:?}",
            WorkspaceLeaseRepository::holder(&s, "/w")
        );
        eprintln!(
            "acq2 = {:?}",
            WorkspaceLeaseRepository::acquire(&mut s, "/w", &t2.id)
        );
        eprintln!(
            "holder-final = {:?}",
            WorkspaceLeaseRepository::holder(&s, "/w")
        );
    }
}
