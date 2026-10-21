//! Engine-backed NFR measurements (X-14 (d): NFR-02 / NFR-03 / NFR-04 / NFR-13
//! re-recorded against the operative store engine, `redb`).
//!
//! The earlier record (`docs/nfr_compliance.md` §3) was taken on the SQLite engine; the
//! compliance record is engine-specific, so after the SQLite prohibition (instruction
//! source 最優先事項, 2026-10-18) settled the engine end-choice at `redb` (O-6), these
//! four rows re-measure on the file-backed engine — the durability basis a deployed
//! project actually runs (`<ATHENA_HOME>/var/athena.db`). Every measurement is
//! deterministic CI work with no LLM non-determinism (`ATH-TEST-022`).
//!
//! What each row measures here:
//!
//! - **NFR-02**: reads p95 < 200 ms at 10,000 Tasks — `list_page` paginates (key-skip)
//!   and `get` resolves by id, over the file-backed store holding 10,000 Task rows.
//! - **NFR-03**: non-terminal Tasks restored from the store after a crash. For an
//!   embedded engine the kill-9 face degenerates to close-and-reopen (the process death
//!   is the harness's handle drop; the file remains); the measure is the reopen +
//!   `list_non_terminal` restore against the 10 s target.
//! - **NFR-04**: ≥ 200 events/s persisted without loss — one engine commit per Event
//!   (the durability-honest worst case — the transition+Event atomicity unit,
//!   `ATH-PERS-003`), with the per-Core monotonic sequence and the replay length/order
//!   asserted.
//! - **NFR-13**: safe shutdown — committed writes survive a clean close and reopen with
//!   every row intact (the durability face the ≤ 30 s shutdown budget assumes).

use athena_persistence::repository::{EventRepository, NewEvent, TaskRepository};
use athena_persistence::{AthenaStore, now_rfc3339};
use athena_types::{EventId, Priority, RiskLevel, Task, TaskId, TaskState};
use std::time::Instant;

/// One Task row shaped like a real one (store-test sample discipline).
fn sample_task(index: usize, state: TaskState) -> Task {
    Task {
        id: TaskId::new(),
        parent_id: None,
        title: format!("NFR re-record task {index}"),
        instruction: "Analyse the timing violation and propose a fix.".into(),
        workspace: format!("/work/nfr-project-{index}"),
        goal: Some("root cause identified with a verified fix".into()),
        constraints: vec!["do not update the production bitstream".into()],
        completion_criteria: vec![
            "root cause identified".into(),
            "fix verified by test".into(),
        ],
        state,
        priority: Priority::High,
        plan_id: None,
        risk_level: RiskLevel::Medium,
        attempt_count: 0,
        max_attempts: 3,
        iteration_count: 3,
        max_iterations: 50,
        deadline: None,
        created_by: "hidemi".into(),
        created_at: now_rfc3339(),
        updated_at: now_rfc3339(),
        finished_at: None,
    }
}

/// Seed `count` Task rows into the store, half non-terminal.
fn seed_tasks(store: &mut AthenaStore, count: usize) -> Vec<TaskId> {
    let mut ids = Vec::with_capacity(count);
    for index in 0..count {
        // A mix — the restore face only sees the non-terminal half.
        let state = if index % 2 == 0 {
            TaskState::Running
        } else {
            TaskState::Success
        };
        let task = sample_task(index, state);
        ids.push(task.id.clone());
        TaskRepository::insert(store, &task).expect("seeded");
    }
    ids
}

fn p95(mut samples_ms: Vec<f64>) -> f64 {
    samples_ms.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
    let rank = ((samples_ms.len() as f64) * 0.95).ceil() as usize;
    samples_ms[rank.saturating_sub(1).min(samples_ms.len() - 1)]
}

#[test]
fn nfr02_reads_p95_under_200ms_at_ten_thousand_tasks_on_redb() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("athena.db");
    let mut store = AthenaStore::open(&db_path).expect("file-backed store");
    let ids = seed_tasks(&mut store, 10_000);

    // 500 mixed read samples: 300 list pages (a spread of shallow and deep offsets, the
    // face the API's pagination walks) + 200 reads by id.
    let mut samples = Vec::with_capacity(500);
    for index in 0..300 {
        let offset = ((index * 97) % 9_900) as u64; // spread across the table
        let started = Instant::now();
        let page = TaskRepository::list_page(&store, offset, 20).expect("page");
        samples.push(started.elapsed().as_secs_f64() * 1_000.0);
        assert_eq!(page.len(), 20);
    }
    for index in 0..200 {
        let id = ids[(index * 53) % ids.len()].clone();
        let started = Instant::now();
        let read = TaskRepository::get(&store, &id).expect("get");
        samples.push(started.elapsed().as_secs_f64() * 1_000.0);
        assert!(read.is_some(), "seeded id must resolve");
    }

    let p95_ms = p95(samples);
    println!("NFR-02 redb: reads p95 {p95_ms:.2} ms over 500 mixed samples @10,000 Tasks");
    assert!(
        p95_ms < 200.0,
        "NFR-02 on the redb engine: reads p95 {p95_ms:.1} ms must stay under 200 ms"
    );
}

#[test]
fn nfr03_non_terminal_tasks_restore_within_10s_after_cold_reopen_on_redb() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("athena.db");
    {
        let mut store = AthenaStore::open(&db_path).expect("store");
        seed_tasks(&mut store, 10_000);
    } // the "crash": the owning process is gone — the engine handle is dropped
    let _ = std::fs::File::open(&db_path).expect("file persists");

    // The fresh boot's restore path: cold reopen + the restore query.
    let started = Instant::now();
    let store = AthenaStore::open(&db_path).expect("reopened");
    let restored = TaskRepository::list_non_terminal(&store).expect("restored");
    let elapsed = started.elapsed();

    assert_eq!(restored.len(), 5_000, "the running half restores");
    println!("NFR-03 redb: cold reopen + restore of 5,000 non-terminal Tasks took {elapsed:?}");
    assert!(
        elapsed.as_secs_f64() < 10.0,
        "NFR-03 on the redb engine: restore took {elapsed:?}, target 10 s"
    );
}

#[test]
fn nfr04_event_write_throughput_at_least_200_per_second_without_loss_on_redb() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = AthenaStore::open(&dir.path().join("athena.db")).expect("file-backed store");
    let task = TaskId::new();
    let total = 400_usize; // beyond the 200/s target's smallest sample, file-backed

    let start = Instant::now();
    let mut last_sequence = 0_u64;
    for index in 0..total {
        let event = NewEvent {
            event_id: EventId::new(),
            schema_version: 1,
            event_type: "nfr_measurement".into(),
            occurred_at: now_rfc3339(),
            task_id: Some(task.clone()),
            plan_step_id: None,
            agent_id: None,
            instance_id: None,
            skill_id: None,
            correlation_id: format!("cor-nfr-redb-{index}"),
            causation_id: None,
            actor: "system".into(),
            payload_json: "{}".into(),
        };
        // One engine commit per Event — the durability-honest worst case (the
        // transition+Event atomicity unit, ATH-PERS-003).
        let stored = EventRepository::append(&mut store, &event).expect("appended");
        assert_eq!(stored.sequence, last_sequence + 1, "monotonic per Core");
        last_sequence = stored.sequence;
    }
    let elapsed = start.elapsed();
    let rate = total as f64 / elapsed.as_secs_f64();
    println!(
        "NFR-04 redb: {rate:.0} events/s over {total} per-event commits (elapsed {elapsed:?})"
    );
    assert!(
        rate >= 200.0,
        "NFR-04 on the redb engine: {rate:.0} events/s must meet 200/s (elapsed {elapsed:?})"
    );

    // No loss, replay order = append order.
    let replay = EventRepository::list_for_task(&store, &task).expect("replay");
    assert_eq!(replay.len(), total);
    for (position, event) in replay.iter().enumerate() {
        assert_eq!(event.sequence, (position + 1) as u64);
    }
}

#[test]
fn nfr13_committed_writes_survive_clean_close_and_reopen_on_redb() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("athena.db");
    let ids = {
        let mut store = AthenaStore::open(&db_path).expect("store");
        seed_tasks(&mut store, 40)
    }; // drop closes the store — the clean-shutdown durability face

    let store = AthenaStore::open(&db_path).expect("reopened");
    let all = TaskRepository::list_all(&store).expect("all");
    assert_eq!(all.len(), ids.len(), "committed writes survive the close");
    for id in &ids {
        assert!(
            TaskRepository::exists(&store, id).expect("exists"),
            "{id} survived the reopen"
        );
    }
}
