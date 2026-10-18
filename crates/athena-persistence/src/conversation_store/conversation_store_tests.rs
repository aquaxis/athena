//! Tests for the `ConversationRepository` on `SqliteStore` (P6-02's support).

use crate::SqliteStore;
use crate::repository::ConversationRepository as Store;
use athena_types::{Conversation, ConversationId, TaskId, Utterance, UtteranceKind, UtteranceRole};

fn open() -> SqliteStore {
    SqliteStore::in_memory().expect("store")
}

fn create_task_row(store: &mut SqliteStore, id: &TaskId, now: &str) {
    let task = athena_types::Task {
        id: id.clone(),
        parent_id: None,
        title: "fixture".into(),
        instruction: "fixture".into(),
        workspace: "/w-fixture".into(),
        goal: None,
        constraints: vec![],
        completion_criteria: vec![],
        state: athena_types::TaskState::Created,
        priority: athena_types::Priority::Normal,
        plan_id: None,
        risk_level: athena_types::RiskLevel::Low,
        attempt_count: 0,
        max_attempts: 3,
        iteration_count: 0,
        max_iterations: 50,
        deadline: None,
        created_by: "human".into(),
        created_at: now.into(),
        updated_at: now.into(),
        finished_at: None,
    };
    store
        .create_task_with_event(&task, &athena_types::EventId::new(), now, "cor-conv")
        .expect("task fixture inserted");
}

#[test]
fn a_conversation_opens_links_a_task_and_records_utters_in_order() {
    let mut store = open();

    // 1. Open a conversation (pre-Task).
    let conversation = Conversation {
        id: ConversationId::new(),
        task_id: None,
        created_at: "2026-10-18T00:00:00Z".into(),
    };
    Store::insert(&mut store, &conversation).expect("opened");

    // 2. The link face: the Task relates (the forwards-only face; first the Task row —
    //    the conversations.task_id FK names tasks(id)).
    let task = TaskId::new();
    create_task_row(&mut store, &task, "2026-10-18T00:00:00Z");
    Store::link_task(&mut store, &conversation.id, &task).expect("linked");
    let face = Store::get(&store, &conversation.id)
        .expect("fetch")
        .expect("present");
    assert_eq!(face.task_id, Some(task.clone()));

    // 3. Word-resolvable: two utterances in recorded order (Human → Core Agent face).
    let utterances = [
        Utterance {
            id: athena_types::UtteranceId::new(),
            conversation_id: conversation.id.clone(),
            role: UtteranceRole::Human,
            kind: UtteranceKind::Message,
            body: "why does the build fail?".into(),
            clarification_id: None,
            created_at: "2026-10-18T00:00:01Z".into(),
        },
        Utterance {
            id: athena_types::UtteranceId::new(),
            conversation_id: conversation.id.clone(),
            role: UtteranceRole::CoreAgent,
            kind: UtteranceKind::Message,
            body: "checking the recent changes first".into(),
            clarification_id: None,
            created_at: "2026-10-18T00:00:02Z".into(),
        },
    ];
    for utterance in &utterances {
        Store::append_utterance(&mut store, utterance).expect("recorded");
    }
    let recorded = Store::list_utterances(&store, &conversation.id).expect("recorded face");
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].role, UtteranceRole::Human);
    assert_eq!(recorded[1].role, UtteranceRole::CoreAgent);
    assert!(recorded[0].body.contains("build fail"));
}

#[test]
fn the_link_of_an_absent_conversation_refuses_and_a_relift_is_absorbed() {
    let mut store = open();
    // A link of a conversation that does not exist refuses (404 face):
    let gone = ConversationId::new();
    let task = TaskId::new();
    assert!(
        Store::link_task(&mut store, &gone, &task).is_err(),
        "missing conversation refusal"
    );
    // An existing conversation linked twice absorbs (the forwards-only face):

    create_task_row(&mut store, &task, "2026-10-18T00:00:00Z");
    let other = TaskId::new();
    create_task_row(&mut store, &other, "2026-10-18T00:00:00Z");
    let conversation = Conversation {
        id: ConversationId::new(),
        task_id: Some(task.clone()),
        created_at: "t".into(),
    };
    Store::insert(&mut store, &conversation).expect("in");
    Store::link_task(&mut store, &conversation.id, &other)
        .expect("the later relation re-points forward");
    drop(task);
    // (ATH-API-021's face: the conversation stays forward-pointing per Task relation.)
}
