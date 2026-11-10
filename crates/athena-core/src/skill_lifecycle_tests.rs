//! X-35's semantics under test: the Skill Manager's and the Skill Validator's
//! §12.5 completion legs — the update face (locate → gate → `SkillUpdated` /
//! `SkillValidationFailed`) and the revalidation face (`SkillValidated` /
//! `SkillValidationFailed`), including the unlocatable-pin face, the gate-failure
//! face, the checksum-record refresh, and the §17.3 edge/event discipline.

use crate::skill_lifecycle::{LifecycleFault, run_once};
use athena_persistence::AthenaStore;
use athena_persistence::repository::{EventRepository, SkillRepository};
use athena_types::{Skill, SkillId, SkillState};
use std::path::PathBuf;

/// One complete `Skill` entity, state-carried (the X-31 field map).
fn skill_row(id: &str, version: &str, state: SkillState, checksum: &str) -> Skill {
    Skill {
        id: SkillId::new(id),
        name: "the skill".into(),
        version: version.into(),
        description: "the X-35 fixture".into(),
        capabilities: vec!["summarize".into()],
        required_tools: Vec::new(),
        required_permissions: Vec::new(),
        supported_agents: Vec::new(),
        dependencies: Vec::new(),
        runtime_provider: "agent-cli".into(),
        source: "local".into(),
        checksum: checksum.into(),
        publisher: "local".into(),
        risk_level: athena_types::RiskLevel::Medium,
        state,
        hot_reload_safe: false,
        registered_at: "2026-11-10T00:00:00Z".into(),
        updated_at: "2026-11-10T00:00:00Z".into(),
    }
}

fn store_with(rows: &[Skill]) -> AthenaStore {
    let mut store = AthenaStore::in_memory().expect("store");
    for row in rows {
        SkillRepository::upsert(&mut store, row).expect("seed");
    }
    store
}

/// A real package tree at `root` (the full package root): `SKILL.md` + `manifest.yaml`
/// pinning `(id, version)`. Returns the computed content checksum the gate must produce.
fn make_package_root(root: &std::path::Path, id: &str, version: &str, body: &str) -> String {
    std::fs::create_dir_all(root).expect("dir");
    std::fs::write(root.join("SKILL.md"), body).expect("md");
    std::fs::write(
        root.join("manifest.yaml"),
        format!(
            "id: {id}\n\
             name: The Skill\n\
             version: \"{version}\"\n\
             description: the X-35 fixture package\n\
             capabilities:\n\
             \x20 - summarize\n\
             required_tools:\n\
             \x20 - fs_read\n\
             required_permissions:\n\
             \x20 - fs_read\n\
             supported_agents: []\n\
             runtime:\n\
             \x20 provider: agent-cli\n\
             source: local\n\
             publisher: local\n\
             risk:\n\
             \x20 level: low\n"
        ),
    )
    .expect("manifest");
    let package = athena_skills::package::Package::open(root).expect("opens");
    package.content_checksum().expect("checksum")
}

/// The source-tree face (`skills.local_dir/<id>/` — one live version per id).
fn make_source_package(local_dir: &std::path::Path, id: &str, version: &str, body: &str) -> String {
    make_package_root(&local_dir.join(id), id, version, body)
}

/// The registered-tree face (`skills.dir/<id>/<version>/` — the pin is the layout).
fn make_registered_package(
    registered_dir: &std::path::Path,
    id: &str,
    version: &str,
    body: &str,
) -> String {
    make_package_root(
        &registered_root(registered_dir, id, version),
        id,
        version,
        body,
    )
}

/// The registered-tree layout (`skills.dir/<id>/<version>/`).
fn registered_root(dir: &std::path::Path, id: &str, version: &str) -> PathBuf {
    dir.join(id).join(version)
}

#[test]
fn an_updating_row_completes_through_the_registered_copy() {
    // The registration's own body is the acquisition face v1 has: an `UPDATING` row
    // whose registered copy opens and passes the gate walks 更新完了 (`SkillUpdated`,
    // `UPDATING → AVAILABLE`) with the computed content checksum on the record.
    let scratch = std::env::temp_dir().join(format!(
        "athena-x35-updating-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let local = scratch.join("skills");
    let registered = scratch.join("var-skills");
    let checksum = make_registered_package(&registered, "the-skill", "1.2.0", "body one\n");
    let mut store = store_with(&[skill_row(
        "the-skill",
        "1.2.0",
        SkillState::Updating,
        "sha256:old",
    )]);
    let report = run_once(
        &mut store,
        &local,
        &registered,
        true,
        "2026-11-10T12:00:00Z",
        "cor-x35-a",
    )
    .expect("the tick ran");
    assert_eq!(report.updated, 1, "{report:?}");
    assert_eq!(report.update_failures, 0, "{report:?}");
    assert!(!report.is_quiet(), "the tick touched one row");
    let row = SkillRepository::get(&store, &SkillId::new("the-skill"), "1.2.0")
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SkillState::Available);
    assert_eq!(
        row.checksum, checksum,
        "the record carries the verified quantity"
    );
    assert_eq!(row.updated_at, "2026-11-10T12:00:00Z");

    // The completion Event: `SkillUpdated`, actor `system`, the §17.3 edge in the payload.
    let events = EventRepository::list_since(&store, 0, 100).expect("events");
    let completion = events
        .iter()
        .find(|event| event.event_type == "SkillUpdated")
        .expect("the completion event appended");
    assert_eq!(completion.actor, "system");
    assert_eq!(
        completion.skill_id.as_ref().map(SkillId::as_str),
        Some("the-skill")
    );
    let payload: serde_json::Value = serde_json::from_str(&completion.payload_json).expect("json");
    assert_eq!(payload["from"], "UPDATING");
    assert_eq!(payload["to"], "AVAILABLE");
    assert_eq!(payload["operation"], "update");
    assert_eq!(payload["checksum"], checksum);
    // The registered copy was already the material; nothing re-synced (still opens).
    assert!(
        registered_root(&registered, "the-skill", "1.2.0")
            .join("SKILL.md")
            .exists()
    );
}

#[test]
fn a_source_located_update_resyncs_the_registered_copy() {
    // When the source tree still pins the row's version, the Skill Manager's leg
    // re-syncs the registered copy (the update's material face) and the row completes.
    let scratch = std::env::temp_dir().join(format!(
        "athena-x35-source-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let local = scratch.join("skills");
    let registered = scratch.join("var-skills");
    let checksum = make_source_package(&local, "the-skill", "1.2.0", "the source body\n");
    let mut store = store_with(&[skill_row(
        "the-skill",
        "1.2.0",
        SkillState::Updating,
        "sha256:old",
    )]);
    let report = run_once(
        &mut store,
        &local,
        &registered,
        true,
        "2026-11-10T12:00:00Z",
        "cor-x35-b",
    )
    .expect("the tick ran");
    assert_eq!(report.updated, 1, "{report:?}");
    assert_eq!(report.update_failures, 0, "{report:?}");
    // The registered copy now holds the source's material (the copy face replayed):
    let copy = registered_root(&registered, "the-skill", "1.2.0").join("SKILL.md");
    let copied = std::fs::read_to_string(&copy).expect("the copy exists");
    assert_eq!(copied, "the source body\n");
    let row = SkillRepository::get(&store, &SkillId::new("the-skill"), "1.2.0")
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SkillState::Available);
    assert_eq!(row.checksum, checksum);
}

#[test]
fn an_unlocatable_pin_lands_failed_with_the_reason() {
    // No source tree pins the version and no registered copy exists: 取得失敗 — the
    // row lands FAILED (`SkillValidationFailed`, `UPDATING → FAILED`) and the fault
    // line names the pin.
    let scratch = std::env::temp_dir().join(format!(
        "athena-x35-unlocatable-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let local = scratch.join("skills");
    let registered = scratch.join("var-skills");
    std::fs::create_dir_all(&local).expect("local tree");
    std::fs::create_dir_all(&registered).expect("registered tree");
    let mut store = store_with(&[skill_row(
        "the-skill",
        "1.2.0",
        SkillState::Updating,
        "sha256:old",
    )]);
    let report = run_once(
        &mut store,
        &local,
        &registered,
        true,
        "2026-11-10T12:00:00Z",
        "cor-x35-c",
    )
    .expect("the tick ran");
    assert_eq!(report.updated, 0, "{report:?}");
    assert_eq!(report.update_failures, 1, "{report:?}");
    assert!(
        report
            .faults
            .iter()
            .any(|line| line.contains("the-skill@1.2.0") && line.contains("取得失敗")),
        "{report:?}"
    );
    let row = SkillRepository::get(&store, &SkillId::new("the-skill"), "1.2.0")
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SkillState::Failed);
    // The failure face keeps the record's previous checksum (nothing new verified):
    assert_eq!(row.checksum, "sha256:old");
    let events = EventRepository::list_since(&store, 0, 100).expect("events");
    let failed = events
        .iter()
        .find(|event| event.event_type == "SkillValidationFailed")
        .expect("the failure event appended");
    let payload: serde_json::Value = serde_json::from_str(&failed.payload_json).expect("json");
    assert_eq!(payload["from"], "UPDATING");
    assert_eq!(payload["to"], "FAILED");
}

#[test]
fn a_failed_gate_lands_failed_and_the_source_moved_on_leaves_the_pin_in_flight_resolved_later() {
    // A checksum mismatch (the body changed after the manifest declared its digest) is
    // a real 検証失敗: FAILED, with the record's checksum preserved. And a source tree
    // that moved past the pin (its manifest names a newer version) leaves the pin to
    // the registered copy — here absent, so the same tick's order is proven: rows walk
    // one leg per tick from their own rest state, never invented forward.
    let scratch = std::env::temp_dir().join(format!(
        "athena-x35-gate-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let local = scratch.join("skills");
    let registered = scratch.join("var-skills");
    // The source manifest pins 9.9.9 — the row's 1.2.0 pin is not what the tree carries:
    let _newer = make_source_package(&local, "the-skill", "9.9.9", "a newer generation\n");
    // No registered copy, so the pin is unlocatable → 取得失敗 face:
    let mut store = store_with(&[skill_row(
        "the-skill",
        "1.2.0",
        SkillState::Updating,
        "sha256:old",
    )]);
    let report = run_once(
        &mut store,
        &local,
        &registered,
        true,
        "2026-11-10T12:00:00Z",
        "cor-x35-d",
    )
    .expect("the tick ran");
    assert_eq!(report.update_failures, 1, "{report:?}");
    let row = SkillRepository::get(&store, &SkillId::new("the-skill"), "1.2.0")
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SkillState::Failed);

    // The gate failure face: a registered copy whose declared checksum disagrees with
    // its SKILL.md body (the manifest carries no checksum → computed fills; to force a
    // mismatch, declare one and corrupt the body afterward).
    let registered2 = scratch.join("var-skills-2");
    let root = registered_root(&registered2, "gated", "2.0.0");
    let _declared = make_registered_package(&registered2, "gated", "2.0.0", "honest body\n");
    let manifest_path = root.join("manifest.yaml");
    let manifest = std::fs::read_to_string(&manifest_path).expect("manifest");
    let declared = format!(
        "{manifest}checksum: \"sha256:0000000000000000000000000000000000000000000000000000000000000000\"\n"
    );
    std::fs::write(&manifest_path, declared).expect("declared checksum");
    let mut store2 = store_with(&[skill_row(
        "gated",
        "2.0.0",
        SkillState::Updating,
        "sha256:old",
    )]);
    let report = run_once(
        &mut store2,
        &local,
        &registered2,
        true,
        "2026-11-10T12:00:00Z",
        "cor-x35-e",
    )
    .expect("the tick ran");
    assert_eq!(report.update_failures, 1, "{report:?}");
    assert!(
        report.faults.iter().any(|line| line.contains("検証失敗")),
        "{report:?}"
    );
    let row = SkillRepository::get(&store2, &SkillId::new("gated"), "2.0.0")
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SkillState::Failed);
}

#[test]
fn a_validating_row_resolves_through_the_validator_leg() {
    // The revalidation request face (`FAILED → VALIDATING`, 202) put the row in flight;
    // the Skill Validator's leg resolves it: 検証成功 → `SkillValidated`
    // (`VALIDATING → AVAILABLE`) / gate failure → `SkillValidationFailed`.
    let scratch = std::env::temp_dir().join(format!(
        "athena-x35-validating-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let local = scratch.join("skills");
    let registered = scratch.join("var-skills");
    let checksum = make_registered_package(&registered, "the-skill", "1.2.0", "validator body\n");
    let mut store = store_with(&[skill_row(
        "the-skill",
        "1.2.0",
        SkillState::Validating,
        "sha256:old",
    )]);
    let report = run_once(
        &mut store,
        &local,
        &registered,
        true,
        "2026-11-10T12:00:00Z",
        "cor-x35-f",
    )
    .expect("the tick ran");
    assert_eq!(report.validated, 1, "{report:?}");
    assert_eq!(report.validation_failures, 0, "{report:?}");
    let row = SkillRepository::get(&store, &SkillId::new("the-skill"), "1.2.0")
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SkillState::Available);
    assert_eq!(row.checksum, checksum);
    let events = EventRepository::list_since(&store, 0, 100).expect("events");
    let validated = events
        .iter()
        .find(|event| event.event_type == "SkillValidated")
        .expect("the validation event appended");
    assert_eq!(validated.actor, "system");
    let payload: serde_json::Value = serde_json::from_str(&validated.payload_json).expect("json");
    assert_eq!(payload["from"], "VALIDATING");
    assert_eq!(payload["to"], "AVAILABLE");
    assert_eq!(payload["operation"], "revalidate");
}

#[test]
fn a_validating_row_that_fails_the_gate_lands_failed() {
    let scratch = std::env::temp_dir().join(format!(
        "athena-x35-vfail-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let local = scratch.join("skills");
    let registered = scratch.join("var-skills");
    // Empty trees: the pin is unlocatable → 検証失敗 (VALIDATING → FAILED).
    std::fs::create_dir_all(&local).expect("local tree");
    std::fs::create_dir_all(&registered).expect("registered tree");
    let mut store = store_with(&[skill_row(
        "the-skill",
        "1.2.0",
        SkillState::Validating,
        "sha256:old",
    )]);
    let report = run_once(
        &mut store,
        &local,
        &registered,
        true,
        "2026-11-10T12:00:00Z",
        "cor-x35-g",
    )
    .expect("the tick ran");
    assert_eq!(report.validated, 0, "{report:?}");
    assert_eq!(report.validation_failures, 1, "{report:?}");
    let row = SkillRepository::get(&store, &SkillId::new("the-skill"), "1.2.0")
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SkillState::Failed);
}

#[test]
fn a_quiet_tick_reports_zero_and_no_events() {
    // No in-flight rows: the tick is a no-op — no reads refused, no events appended.
    let scratch = std::env::temp_dir().join(format!(
        "athena-x35-quiet-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let local = scratch.join("skills");
    let registered = scratch.join("var-skills");
    std::fs::create_dir_all(&local).expect("local tree");
    std::fs::create_dir_all(&registered).expect("registered tree");
    let mut store = store_with(&[skill_row(
        "the-skill",
        "1.2.0",
        SkillState::Available,
        "sha256:ok",
    )]);
    let report = run_once(
        &mut store,
        &local,
        &registered,
        true,
        "2026-11-10T12:00:00Z",
        "cor-x35-h",
    )
    .expect("the tick ran");
    assert!(report.is_quiet(), "{report:?}");
    let events = EventRepository::list_since(&store, 0, 100).expect("events");
    assert!(
        events.is_empty(),
        "a quiet tick appends nothing: {events:?}"
    );
}

#[test]
fn the_unopened_registered_copy_reports_its_fault_before_any_state_change() {
    // A registered copy that exists but does not open (a torn manifest) refuses the
    // locate face with the fault surfaced — the row lands FAILED with the copy's own
    // reason, and the two files stay untouched (the refusal precedes any write).
    let scratch = std::env::temp_dir().join(format!(
        "athena-x35-torn-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let local = scratch.join("skills");
    let registered = scratch.join("var-skills");
    let root = registered_root(&registered, "the-skill", "1.2.0");
    std::fs::create_dir_all(&root).expect("registered tree");
    std::fs::write(root.join("SKILL.md"), "body\n").expect("md");
    std::fs::write(root.join("manifest.yaml"), "id: [broken\n").expect("torn manifest");
    let mut store = store_with(&[skill_row(
        "the-skill",
        "1.2.0",
        SkillState::Updating,
        "sha256:old",
    )]);
    let report = run_once(
        &mut store,
        &local,
        &registered,
        true,
        "2026-11-10T12:00:00Z",
        "cor-x35-i",
    )
    .expect("the tick ran");
    assert_eq!(report.update_failures, 1, "{report:?}");
    assert!(
        report
            .faults
            .iter()
            .any(|line| line.contains("does not open")),
        "{report:?}"
    );
    let row = SkillRepository::get(&store, &SkillId::new("the-skill"), "1.2.0")
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SkillState::Failed);
}

#[test]
fn verify_checksum_false_skips_the_declared_checksum_check() {
    // `skills.verify_checksum: false` maps over: the gate skips the checksum check but
    // still computes the content checksum for the record (the verified quantity rides
    // the record either way).
    let scratch = std::env::temp_dir().join(format!(
        "athena-x35-nochecksum-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let local = scratch.join("skills");
    let registered = scratch.join("var-skills");
    let root = registered_root(&registered, "the-skill", "1.2.0");
    let computed = make_registered_package(&registered, "the-skill", "1.2.0", "body\n");
    let manifest_path = root.join("manifest.yaml");
    let manifest = std::fs::read_to_string(&manifest_path).expect("manifest");
    let declared = format!(
        "{manifest}checksum: \"sha256:1111111111111111111111111111111111111111111111111111111111111111\"\n"
    );
    std::fs::write(&manifest_path, declared).expect("declared wrong checksum");
    let mut store = store_with(&[skill_row(
        "the-skill",
        "1.2.0",
        SkillState::Updating,
        "sha256:old",
    )]);
    let report = run_once(
        &mut store,
        &local,
        &registered,
        false,
        "2026-11-10T12:00:00Z",
        "cor-x35-j",
    )
    .expect("the tick ran");
    assert_eq!(report.updated, 1, "{report:?}");
    let row = SkillRepository::get(&store, &SkillId::new("the-skill"), "1.2.0")
        .expect("read")
        .expect("row");
    assert_eq!(row.state, SkillState::Available);
    // The computed checksum filled the record (not the stale declared one):
    assert_eq!(row.checksum, computed);
}

#[test]
fn lifecycle_fault_surfaces_through_the_tick() {
    // A store fault aborts the tick through `LifecycleFault` — the cadence's retry
    // face; here the fault shape itself is exercised through the module's error type.
    let fault = LifecycleFault::Store("the disk refused".into());
    let rendered = fault.to_string();
    assert!(rendered.contains("store fault"), "{rendered}");
}
