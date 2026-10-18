use super::apply;
use super::HEADER;
use athena_core::IdempotencyRepository as _;
use athena_core::SqliteStore;

fn headers_with(key: Option<&str>) -> axum::http::HeaderMap {
    let mut headers = axum::http::HeaderMap::new();
    if let Some(key) = key {
        headers.insert(HEADER, key.parse().expect("key parses"));
    }
    headers
}

#[test]
fn a_key_executes_exactly_once() {
    // R-API-05: the first write executes and records; the replay answers the same
    // outcome with no second execution.
    let mut store = SqliteStore::in_memory().expect("store");
    let once = std::cell::Cell::new(0_u32);
    let headers = headers_with(Some("client-op-1"));
    let first = apply(&headers, &mut store, || {
        once.set(once.get() + 1);
        "executed-once".to_string()
    })
    .expect("applied");
    assert_eq!(first, Applied::FirstWrite { outcome: "executed-once".into() });
    let replay = apply(&headers, &mut store, || {
        once.set(once.get() + 1);
        "must-not-run".to_string()
    })
    .expect("applied");
    assert_eq!(replay, Applied::Replay { outcome: "executed-once".into() });
    assert_eq!(once.get(), 1, "the mutation ran exactly once");
}

#[test]
fn no_key_means_free_reexecution() {
    // A client without the header owns the re-execution semantics: the request runs every
    // time (the Core's idempotency guard is keyed, not ambient).
    let mut store = SqliteStore::in_memory().expect("store");
    let headers = headers_with(None);
    let first = apply(&headers, &mut store, || "a".to_string()).expect("applied");
    let second = apply(&headers, &mut store, || "b".to_string()).expect("applied");
    assert_eq!(first, Applied::FirstWrite { outcome: "a".into() });
    assert_eq!(second, Applied::FirstWrite { outcome: "b".into() });
}

#[test]
fn the_crash_hole_replays_as_first_write_without_breaking_exclusivity() {
    // Claims persist before the outcome lands; a replay of a claimed-but-unrecorded key
    // completes the recording (never a silent 500, never a double-write of a pair).
    let mut store = SqliteStore::in_memory().expect("store");
    athena_core::IdempotencyRepository::claim(&mut store, "hop-key")
        .expect("the crash ate the write, not the claim");
    let headers = headers_with(Some("hop-key"));
    let applied = apply(&headers, &mut store, || "recovered".to_string()).expect("applied");
    // The outcome is complete and recorded, answerable exactly as though nothing crashed.
    assert_eq!(applied, Applied::FirstWrite { outcome: "recovered".into() });
    let next = apply(&headers, &mut store, || "again".to_string()).expect("applied");
    assert_eq!(next, Applied::Replay { outcome: "recovered".into() });
}

#[test]
fn a_store_fault_is_internal_never_retried() {
    // A dangling store is a local fault surface, not a client condition.
    struct BrokenClaim;
    impl IdempotencyRepository for BrokenClaim {
        fn claim(&mut self, _key: &str) -> Result<bool, PersistenceError> {
            Err(PersistenceError::Decode { entity: "idempotency", column: "claim", detail: "broken by test".into() })
        }
        fn is_claimed(&self, _key: &str) -> Result<bool, PersistenceError> {
            unreachable!()
        }
        fn record_outcome(&mut self, _key: &str, _outcome: &str) -> Result<(), PersistenceError> {
            unreachable!()
        }
        fn outcome(&self, _key: &str) -> Result<Option<String>, PersistenceError> {
            unreachable!()
        }
        fn prune_before(&mut self, _cutoff_rfc3339: &str) -> Result<u64, PersistenceError> {
            unreachable!()
        }
    }
    let mut broken = BrokenClaim;
    let headers = headers_with(Some("some-key"));
    let reject = apply(&headers, &mut broken, || String::new()).expect_err("broken");
    assert!(reject.to_string().contains("idempotency store failed"), "{reject}");
}
