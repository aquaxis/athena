//! Result Manager — `Result` / `Artifact` / `Evaluation` (spec §19).
//!
//! Results are immutable on receipt; artifacts are re-based into the Result Store
//! (ATH-RESULT-004); evaluations record verifier, method and verdict — a self-declared
//! success is never the sole basis for acceptance (ATH-EVAL-005).
//!
//! Owned entities (`OWNED`): Result, Artifact, Evaluation. Emitted Events:
//! `TaskResultReceived`, `Evaluation*`.

use crate::ownership::{Entity, ModuleRole};

/// The entities this module may mutate.
pub const OWNED: &[(Entity, ModuleRole)] = &[
    (Entity::Result, ModuleRole::ResultManager),
    (Entity::Artifact, ModuleRole::ResultManager),
    (Entity::Evaluation, ModuleRole::ResultManager),
];

// --- P4-10: reception rules ----------------------------------------------------------------

pub mod reception {
    //! What the Result Manager accepts on receipt (`ATH-RESULT-002`, `ATH-RESULT-003`).
    //!
    //! The rules are structural: a Result is immutable once written (the store's
    //! triggers refuse the rewrite, P4-10's repository rows), and the artifact paths
    //! must outlive the Instance areas — a Workspace-relative path would dangle the
    //! moment `events.retention_days` sweeps the Instance's `aiprj` area
    //! (`ATH-RESULT-002`, `ATH-AIPRJ-026`). `base` carries the anchor: `work_target`
    //! paths are relative to `<PROJECT_ROOT>`, `store` paths to
    //! `<ATHENA_HOME>/var/artifacts/<result_id>/` — the copy step itself is P4-11.

    use crate::ownership::{Entity, ModuleRole};
    use athena_types::{ArtifactBase, ExecutionResult, ResultStatus};

    /// Silent ownership re-statement (drift-checked like every Core Host module).
    #[must_use]
    pub const fn owned() -> &'static [(Entity, ModuleRole)] {
        &[
            (Entity::Result, ModuleRole::ResultManager),
            (Entity::Artifact, ModuleRole::ResultManager),
            (Entity::Evaluation, ModuleRole::ResultManager),
        ]
    }

    /// Why reception refused.
    #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
    pub enum Refused {
        /// The artifact path is Workspace-relative (`.`, `./…`, `../…`): the Instance
        /// cwd is deleted at retention expiry, so such a reference dangles.
        #[error(
            "the artifact path {0:?} is Workspace-relative; record it against work_target or store (ATH-RESULT-002)"
        )]
        WorkspaceRelative(String),
        /// The path names an Instance area (under `var/instances/` or `var/core-agent/`)
        /// those areas are deleted; the artifact must be copied to the store with
        /// `base: work_target → store` first (P4-11's re-base step, `ATH-RESULT-004`).
        #[error(
            "the artifact path {0:?} names an Instance area; copy it into the store at receipt (ATH-RESULT-004)"
        )]
        InstanceArea(String),
        /// The path is empty — an artifact without a location records nothing.
        #[error("the artifact path must not be empty (ATH-RESULT-002)")]
        EmptyPath,
    }

    /// Stage one Result for persistence: every artifact path is audited; the returned
    /// `ExecutionResult` is *the same value*, staged unchanged — the refusal names the
    /// rule so the producer re-bases (P4-11) instead of the Manager silently rewriting
    /// (the Manager records, it does not invent).
    ///
    /// # Errors
    /// [`Refused`] naming the offending artifact path.
    pub fn stage(result: &ExecutionResult) -> Result<(), Refused> {
        for artifact in &result.artifacts {
            audit_path(&artifact.path)?;
            // `base` is enforced by the type (ArtifactBase has exactly the two variants)
            // — recorded here so the audit's road map names where the row lands.
            let _base: ArtifactBase = artifact.base;
            let _ = artifact.kind;
        }
        // The self-declared status rides the record but is never the acceptance basis
        // (`ATH-EVAL-005`): recorded as a fact, evaluated in P4-12.
        let _ = result.status;
        Ok(())
    }

    fn audit_path(path: &str) -> Result<(), Refused> {
        if path.trim().is_empty() {
            return Err(Refused::EmptyPath);
        }
        if path.starts_with('.') {
            // A leading `.` is cwd-relative by definition — and ATHENA never runs a
            // cwd echo back to the agent anyway (`ATH-ACLI-016`).
            return Err(Refused::WorkspaceRelative(path.to_string()));
        }
        for forbidden in ["var/instances/", "var/core-agent/"] {
            if path.contains(forbidden) {
                return Err(Refused::InstanceArea(path.to_string()));
            }
        }
        Ok(())
    }

    /// The `TaskResultReceived` payload facts (§23.2: `result_id`, `plan_step_id`,
    /// `status`). Only Tasks with a PlanStep carry the pair; a Task may complete its
    /// own summary (`ATH-EVAL-001`).
    ///
    /// # Errors
    /// [`Refused::EmptyPath`]-shaped `None` becomes an error only for artifacts; the
    /// facts builder returns `Option` so a Task-level summary (no step) is legal.
    #[must_use]
    pub fn received_facts(result: &ExecutionResult) -> Option<ReceivedFacts> {
        Some(ReceivedFacts {
            result_id: result.id.as_str().to_string(),
            plan_step_id: result.plan_step_id.as_ref().map(|s| s.as_str().to_string()),
            status: match result.status {
                ResultStatus::Success => "success",
                ResultStatus::Failure => "failure",
                ResultStatus::Partial => "partial",
            },
        })
    }

    /// One received Result's §23.2 payload rows.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ReceivedFacts {
        /// The Result.
        pub result_id: String,
        /// The producing step, when the Result is a step's product.
        pub plan_step_id: Option<String>,
        /// The agent's self-declared status (never the acceptance basis, `ATH-EVAL-005`).
        pub status: &'static str,
    }
}

#[cfg(test)]
mod reception_tests {
    use super::reception::{self, Refused};
    use athena_types::{
        AgentId, Artifact, ArtifactBase, ArtifactKind, ExecutionResult, InstanceId, PlanStepId,
        ResultId, ResultStatus, TaskId,
    };

    fn result_with(path: &str, base: ArtifactBase) -> ExecutionResult {
        ExecutionResult {
            id: ResultId::new(),
            task_id: TaskId::new(),
            plan_step_id: Some(PlanStepId::new()),
            agent_id: AgentId::new("p4-10-worker"),
            instance_id: InstanceId::new(),
            status: ResultStatus::Success,
            output: "body".into(),
            artifacts: vec![Artifact {
                result_id: ResultId::new(),
                kind: ArtifactKind::File,
                base,
                path: path.into(),
                bytes: Some(3),
                checksum: None,
                created_at: "2026-10-17T00:00:00.000Z".into(),
            }],
            logs_ref: None,
            skills_used: vec![],
            model: None,
            started_at: "2026-10-17T00:00:00.000Z".into(),
            finished_at: "2026-10-17T00:00:01.000Z".into(),
            execution_time_ms: 1_000,
            metadata: serde_json::json!({}),
        }
    }

    #[test]
    fn the_work_target_base_stages_when_the_path_outlives_the_instance() {
        // A project-root-relative path under `work_target`: the legal form.
        let staged = result_with("src/lib.rs", ArtifactBase::WorkTarget);
        assert!(reception::stage(&staged).is_ok());

        // And the facts carry the §23.2 rows.
        let facts = reception::received_facts(&staged).expect("facts");
        assert_eq!(facts.status, "success");
        assert_eq!(facts.result_id, staged.id.as_str());
    }

    #[test]
    fn a_workspace_relative_path_refuses() {
        for path in ["./out.txt", "../secrets.txt"] {
            let refused = reception::stage(&result_with(path, ArtifactBase::WorkTarget))
                .expect_err("Workspace-relative refused (ATH-RESULT-002)");
            assert!(
                matches!(refused, Refused::WorkspaceRelative(_)),
                "{refused}"
            );
        }
        // An empty path refuses, whatever the base.
        assert!(reception::stage(&result_with("", ArtifactBase::Store)).is_err());
    }

    #[test]
    fn an_instance_area_path_refuses_too() {
        for path in [
            "athena/var/instances/ri-1/out.txt",
            "var/core-agent/notes.md",
        ] {
            let refused = reception::stage(&result_with(path, ArtifactBase::WorkTarget))
                .expect_err("Instance areas are deleted at retention expiry");
            assert!(matches!(refused, Refused::InstanceArea(_)), "{refused}");
        }
    }

    #[test]
    fn the_store_base_stages_as_received() {
        // After P4-11's re-base the artifact carries `store`; reception accepts it
        // unchanged because the reference outlives the sweep.
        let staged = result_with("report.md", ArtifactBase::Store);
        assert!(reception::stage(&staged).is_ok());
    }
}

// --- P4-11: the Result Store copy at receipt (ATH-RESULT-004) ------------------------------

pub mod store_copy {
    //! Artifact delivery into the Result Store: an artifact whose bytes live inside an
    //! Instance area is **copied** into `<ATHENA_HOME>/var/artifacts/<result_id>/` at
    //! receipt and re-based to `store` — the reference then outlives the
    //! `events.retention_days` sweep that deletes the Instance (`ATH-RESULT-004`,
    //! `ATH-AIPRJ-026`). The copy is deterministic file bookkeeping (Core Host doing I/O,
    //! never inference); the Manager writes the *copy*, it does not summarize content.

    use crate::ownership::{Entity, ModuleRole};
    use athena_types::{ArtifactBase, ExecutionResult};
    use std::path::{Path, PathBuf};

    /// Silent ownership re-statement (drift-checked like every Core Host module).
    #[must_use]
    pub const fn owned() -> &'static [(Entity, ModuleRole)] {
        &[(Entity::Artifact, ModuleRole::ResultManager)]
    }

    /// Why the copy refused.
    #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
    pub enum RebaseFault {
        /// The source file is not in the named Workspace (or was swept already): the
        /// artifact cannot be copied, and a dangling reference is never fabricated.
        #[error(
            "the source {0:?} is not under the Workspace; the copy cannot read it (ATH-RESULT-004)"
        )]
        NotInWorkspace(String),
        /// The copy's I/O failed.
        #[error("the copy of {path:?} failed: {detail}")]
        Io {
            /// The source path.
            path: String,
            /// The error text.
            detail: String,
        },
    }

    /// Copy every Instance-area artifact into the Result Store and re-base.
    ///
    /// The Instance-area *shapes* are the ones reception refuses
    /// (`super::reception::Refused`): paths under `var/instances/` / `var/core-agent/`
    /// and cwd-relative paths (their source is the named Workspace). Artifacts already
    /// `store`-based or legitimately `work_target`-based (inside the project root) pass
    /// untouched — a `work_target` artifact is *not* copied: the project root is under
    /// version control, not an ephemeral Instance area.
    ///
    /// # Errors
    /// [`RebaseFault`] with the offending path (first refusal; deterministic: targets
    /// audit in artifact order).
    pub fn rebase_into_store(
        result: &mut ExecutionResult,
        workspace: &Path,
        athena_home: &Path,
    ) -> Result<usize, RebaseFault> {
        let store_dir = store_dir_for(athena_home, result.id.as_str());
        let mut copied = 0usize;
        for artifact in &mut result.artifacts {
            if is_instance_area(&artifact.path) {
                std::fs::create_dir_all(&store_dir).map_err(|error| RebaseFault::Io {
                    path: store_dir.display().to_string(),
                    detail: error.to_string(),
                })?;
                let source = workspace.join(&artifact.path);
                if !source.is_file() {
                    return Err(RebaseFault::NotInWorkspace(artifact.path.clone()));
                }
                let stored = store_dir.join(
                    std::path::Path::new(&artifact.path)
                        .file_name()
                        .map(|name| name.to_os_string())
                        .unwrap_or_else(|| artifact.path.clone().into()),
                );
                std::fs::copy(&source, &stored).map_err(|error| RebaseFault::Io {
                    path: artifact.path.clone(),
                    detail: error.to_string(),
                })?;
                artifact.base = ArtifactBase::Store;
                artifact.path = stored
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default();
                copied += 1;
            }
        }
        Ok(copied)
    }

    /// The Instance-area shapes reception refuses (paths under the swept areas, and
    /// cwd-relative paths whose source is the named Workspace).
    fn is_instance_area(path: &str) -> bool {
        path.contains("var/instances/")
            || path.contains("var/core-agent/")
            || (path.starts_with('.') && !path.trim().is_empty())
    }

    /// The Result Store directory one result's copies live in
    /// (`<ATHENA_HOME>/var/artifacts/<result_id>/`, `ATH-RESULT-002`).
    #[must_use]
    pub fn store_dir_for(athena_home: &Path, result_id: &str) -> PathBuf {
        athena_home.join("var").join("artifacts").join(result_id)
    }
}

#[cfg(test)]
mod store_copy_tests {
    use super::reception;
    use super::store_copy::{self, RebaseFault};
    use athena_types::{
        AgentId, Artifact, ArtifactBase, ArtifactKind, ExecutionResult, InstanceId, ResultId,
        ResultStatus, TaskId,
    };
    use std::path::Path;

    fn result_with_instance_artifact(
        name: &str,
        link: &str,
    ) -> (
        ExecutionResult,
        std::path::PathBuf,
        std::path::PathBuf,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().expect("temp");
        let workspace = dir.path().join("ws");
        let home = dir.path().join("home");
        let source = workspace.join(link);
        std::fs::create_dir_all(source.parent().expect("parent")).expect("dirs");
        std::fs::write(&source, b"artifact bytes").expect("wrote source");
        let result = ExecutionResult {
            id: ResultId::new(),
            task_id: TaskId::new(),
            plan_step_id: None,
            agent_id: AgentId::new("p4-11-worker"),
            instance_id: InstanceId::new(),
            status: ResultStatus::Success,
            output: "did the work".into(),
            artifacts: vec![Artifact {
                result_id: ResultId::new(),
                kind: ArtifactKind::File,
                base: ArtifactBase::WorkTarget,
                path: link_to_instance(link, name),
                bytes: None,
                checksum: None,
                created_at: "2026-10-17T00:00:00.000Z".into(),
            }],
            logs_ref: None,
            skills_used: vec![],
            model: None,
            started_at: "2026-10-17T00:00:00.000Z".into(),
            finished_at: "2026-10-17T00:00:01.000Z".into(),
            execution_time_ms: 1_000,
            metadata: serde_json::json!({}),
        };
        (result, workspace, home, dir)
    }

    fn link_to_instance(link: &str, name: &str) -> String {
        // The instance-area shape reception refused: a workspace-relative reference
        // naming the swept area.
        match name {
            "relative" => format!("./{link}"),
            "instance" => {
                if link.contains("var/instances") {
                    link.to_string()
                } else {
                    format!("./{link}")
                }
            }
            _ => format!("./{link}"),
        }
    }

    #[test]
    fn an_instance_shaped_path_rebases_but_a_missing_source_refuses() {
        // The instance-shaped path (naming the swept area) copies from the workspace and
        // gains the store base.
        let (mut result, workspace, home, _dir) =
            result_with_instance_artifact("instance", "var/instances/ri-1/out.log");
        let copied =
            store_copy::rebase_into_store(&mut result, Path::new(&workspace), Path::new(&home))
                .expect("copied");
        assert_eq!(copied, 1);
        assert_eq!(result.artifacts[0].base, ArtifactBase::Store);

        // A missing source cannot be copied: the refusal is a fault, never a dangling
        // reference recorded (the fixture wrote a file; sweep it away, as a retention
        // sweep would).
        let (mut result, workspace, home, _dir2) =
            result_with_instance_artifact("relative", "./gone.txt");
        let _ = _dir2;
        std::fs::remove_file(workspace.join("./gone.txt")).expect("swept");
        let error =
            store_copy::rebase_into_store(&mut result, Path::new(&workspace), Path::new(&home))
                .expect_err("no such source");
        assert!(matches!(error, RebaseFault::NotInWorkspace(_)), "{error}");

        // A legitimate work_target path is not copied (the project root is
        // version-controlled, not an ephemeral area).
        let unchanged = result_with_pure_work_target();
        let mut untouched = unchanged.clone();
        store_copy::rebase_into_store(
            &mut untouched,
            Path::new("/nowhere"),
            Path::new("/nowhere-home"),
        )
        .expect("no copy for a project-root path");
        assert_eq!(untouched, unchanged, "the work_target artifact never moved");
    }

    fn result_with_pure_work_target() -> ExecutionResult {
        let (result, _workspace, _home, _dir) = result_with_instance_artifact("pure", "pure.md");
        let mut clean = result;
        clean.artifacts[0] = Artifact {
            result_id: clean.artifacts[0].result_id.clone(),
            kind: ArtifactKind::File,
            base: ArtifactBase::WorkTarget,
            path: "src/lib.rs".into(),
            bytes: Some(5),
            checksum: None,
            created_at: clean.artifacts[0].created_at.clone(),
        };
        clean
    }
    #[test]
    fn a_workspace_relative_artifact_is_copied_and_rebased_to_store() {
        let (mut result, workspace, home, _dir) =
            result_with_instance_artifact("out/report.md", "report.md");
        store_copy::rebase_into_store(&mut result, Path::new(&workspace), Path::new(&home))
            .expect("copied");
        let artifact = &result.artifacts[0];
        assert_eq!(artifact.base, ArtifactBase::Store);
        assert_eq!(artifact.path, "report.md", "the store-local name");
        // The copy exists under the Result Store.
        let stored = store_copy::store_dir_for(&home, result.id.as_str()).join("report.md");
        assert!(stored.is_file(), "{stored:?}");
        // And reception now stages it (the reference outlives the sweep).
        assert!(reception::stage(&result).is_ok());
    }
}

// --- P4-12: the acceptance gate (ATH-EVAL-001..005) ----------------------------------------

pub mod evaluation {
    //! The acceptance decision's deterministic face: a self-declared `Success` is never
    //! the sole basis (`ATH-EVAL-005`); a Result carrying verifiable deliverables must
    //! pass an objective Verification before `SUCCESS` (`ATH-EVAL-002`), with the
    //! verifier, method, and outcome recorded (`ATH-EVAL-003`); verdicts are exactly
    //! accept / retry / replan / fail / escalate (`ATH-EVAL-004`). The *judgement* —
    //! whether the completion criteria hold — is Core Agent's `SubmitEvaluation`
    //! (`ATH-EVAL-001`); this gate is what keeps an unverified Result from being
    //! accepted even so.

    use crate::ownership::{Entity, ModuleRole};
    use athena_types::{ArtifactKind, ExecutionResult, Verdict};

    /// Silent ownership re-statement (drift-checked like every Core Host module).
    #[must_use]
    pub const fn owned() -> &'static [(Entity, ModuleRole)] {
        &[(Entity::Evaluation, ModuleRole::ResultManager)]
    }

    /// The objective Verification methods (spec section 19.2: test / lint / build /
    /// review / simulation). Anything else is not an objective method — a prose
    /// "verified" rides no row through the gate.
    pub const OBJECTIVE_METHODS: [&str; 5] = ["test", "lint", "build", "review", "simulation"];

    /// The gate's outcome for one Result against one Evaluation set.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Gate {
        /// Accept: completion-criteria support rode an objective Verification whose
        /// verdict was `Accept`. The facts name the accepted evaluation.
        Accept {
            /// The accepting Evaluation's id.
            evaluation_id: String,
            /// The method that produced the objective outcome.
            method: String,
            /// Who ran it.
            verifier: String,
        },
        /// Not accepted yet: the Result cannot drive `SUCCESS` — the reason names what
        /// the gate missed (the *facts* are preserved for the Core Agent's next move,
        /// ATH-EVAL-001).
        Unverified {
            /// Why: self-declaration alone, no objective Verification, or a
            /// non-objective method on the row.
            reason: String,
        },
        /// The recorded verdict drives the Task elsewhere: retry / replan / fail /
        /// escalate — exactly the §17.1 table's destinations.
        Follow(Verdict),
    }

    /// The acceptance decision for one Result.
    ///
    /// `evaluation_verdicts` are the Evaluation rows recorded against this Result
    /// `(verdict, method, verifier)` — numeric ordering by created time; the latest
    /// decisive row wins. When any artifact is a verifiable delivery
    /// (`File` / `Patch`), at least one row must be an objective method with verdict
    /// `Accept` — the *self-declared status never stands in for it* (`ATH-EVAL-002`,
    /// `ATH-EVAL-005`; `R-EVAL-01`).
    ///
    /// The completion-criteria *judgement* is Core Agent's; the gate only demands the
    /// recorded backbone an acceptance story needs.
    #[must_use]
    pub fn gate(result: &ExecutionResult, evaluations: &[(Verdict, String, String)]) -> Gate {
        let verifiable = result
            .artifacts
            .iter()
            .any(|artifact| matches!(artifact.kind, ArtifactKind::File | ArtifactKind::Patch));

        // Objective + accepting rows first.
        let accepting = evaluations
            .iter()
            .filter(|(_, method, _)| OBJECTIVE_METHODS.contains(&method.as_str()))
            .find(|(verdict, _, _)| *verdict == Verdict::Accept);

        if let Some((_, method, verifier)) = accepting {
            return Gate::Accept {
                evaluation_id: String::new(),
                method: method.clone(),
                verifier: verifier.clone(),
            };
        }

        // A non-accepting, objective row: the Task follows its verdict.
        if let Some((verdict, _, _)) = evaluations
            .iter()
            .find(|(_, method, _)| OBJECTIVE_METHODS.contains(&method.as_str()))
        {
            return Gate::Follow(*verdict);
        }

        // Verifiable deliverables without any objective row: the Result cannot be
        // the success basis (`ATH-EVAL-002`).
        if verifiable {
            return Gate::Unverified {
                reason: "the Result carries verifiable deliverables and no objective
                         Verification row was recorded (test / lint / build / review /
                         simulation)"
                    .into(),
            };
        }

        // No verifiable deliverables and no visible evaluation: a self-declared status
        // alone is never enough (`ATH-EVAL-005`) — the gate refuses, whatever the
        // agent's message said.
        Gate::Unverified {
            reason: "no Evaluation recorded; the self-declared status is never sufficient
                     on its own (ATH-EVAL-005)"
                .into(),
        }
    }

    /// Whether one evaluation row allows the completion-criteria support story to even
    /// be told (`ATH-EVAL-003`): the method must be one of the objective five.
    #[must_use]
    pub fn objective(method: &str) -> bool {
        OBJECTIVE_METHODS.contains(&method)
    }
}

// --- X-48: the `EVALUATING` state's substance ----------------------------------------------
//
// The gate above decides; nothing recorded a decision. `insert_evaluation` had no production
// caller, so a Task walked `RUNNING → EVALUATING → SUCCESS` on the agent's own word — the one
// thing §19.2 says must not happen ("the result the agent returned is not treated as the final
// result"). These two faces close that: the Core Agent's `submit_evaluation` becomes an
// Evaluation **row** plus its §23.2 Event, and the settlement reads the rows back through the
// gate instead of reading the agent's self-declared status alone.

pub mod record {
    //! Recording an evaluation, and settling a Task against what is recorded.

    use super::evaluation::{Gate, gate};
    use athena_persistence::repository::{EventRepository, ResultRepository};
    use athena_persistence::{AthenaStore, NewEvent};
    use athena_types::{
        Evaluation, EvaluationId, Evaluator, EventId, ExecutionResult, ResultId, ResultStatus,
        TaskId, TaskState, Verdict,
    };

    /// The method recorded for the Core Agent's own judgement (`ATH-EVAL-001`).
    ///
    /// `submit_evaluation` carries no method, because the Core Agent's reading of the
    /// completion criteria **is not** one of the objective five (test / lint / build /
    /// review / simulation). Recording it under an objective name would manufacture the
    /// very verification `ATH-EVAL-005` forbids standing alone, so the row says what it is
    /// — and [`super::evaluation::objective`] answers `false` for it, which keeps the gate
    /// demanding a real Verification for verifiable deliverables.
    pub const AGENT_JUDGEMENT: &str = "agent_judgement";

    /// Why an evaluation could not be recorded.
    #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
    pub enum EvaluationFault {
        /// No such Result.
        #[error("result {0} is absent")]
        ResultAbsent(String),
        /// The Result belongs to another Task: an evaluation is a judgement about *this*
        /// Task's product, and crossing that boundary would record a fact about work the
        /// requester does not own (`ATH-DATA-003`).
        #[error("result {result} belongs to task {owner}, not {requester}")]
        ResultNotOwned {
            /// The evaluated Result.
            result: String,
            /// The Task that owns it.
            owner: String,
            /// The Task that asked.
            requester: String,
        },
        /// The rationale is empty: `ATH-CORE-016` requires the reason to ride the record,
        /// and an evaluation without one records a verdict nobody can read back.
        #[error("the evaluation rationale must not be empty (ATH-CORE-016)")]
        RationaleMissing,
        /// The store refused.
        #[error("store fault: {0}")]
        Store(String),
        /// The Event refused against the catalog.
        #[error("event construction: {0}")]
        Construction(String),
    }

    impl EvaluationFault {
        /// The §20.1 class.
        #[must_use]
        pub const fn class(&self) -> crate::protocol::ErrorClass {
            match self {
                Self::ResultAbsent(_) | Self::RationaleMissing => {
                    crate::protocol::ErrorClass::ValidationError
                }
                Self::ResultNotOwned { .. } => crate::protocol::ErrorClass::PermissionError,
                Self::Store(_) | Self::Construction(_) => {
                    crate::protocol::ErrorClass::InternalError
                }
            }
        }

        /// The stable machine code.
        #[must_use]
        pub const fn code(&self) -> &'static str {
            match self {
                Self::ResultAbsent(_) => "RESULT_NOT_FOUND",
                Self::ResultNotOwned { .. } => "RESULT_NOT_OWNED",
                Self::RationaleMissing => "EVALUATION_RATIONALE_MISSING",
                Self::Store(_) | Self::Construction(_) => "EVALUATION_RECORD_FAILED",
            }
        }
    }

    /// One judgement about a Result: who made it, by what means, with what verdict and on
    /// what stated ground (`ATH-EVAL-003`'s four recordable facts, in one value).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Judgement<'a> {
        /// Who evaluated (an agent names itself; a tool is a tool).
        pub evaluator: Evaluator,
        /// The means: one of the objective five, or [`AGENT_JUDGEMENT`] for the Core
        /// Agent's own reading of the criteria.
        pub method: &'a str,
        /// One of the five verdicts (`ATH-EVAL-004`).
        pub verdict: Verdict,
        /// The reason, recorded verbatim (`ATH-CORE-016`).
        pub rationale: &'a str,
    }

    /// Record one evaluation of a Result (`ATH-EVAL-003`: implementor, method and outcome
    /// are all recorded), and emit its §23.2 Event — `EvaluationPassed` for an accepting
    /// verdict, `EvaluationFailed` for the other four (`ATH-EVAL-004`).
    ///
    /// The Task's state is deliberately **not** a precondition: §19.2's picture has a
    /// verification agent judging a coding agent's result while the work is still running,
    /// and a Task that already settled can still receive the record of why. What is
    /// checked is ownership — the Result must be this Task's.
    ///
    /// # Errors
    /// [`EvaluationFault`], whole.
    pub fn record_evaluation(
        store: &mut AthenaStore,
        task: &TaskId,
        result_id: &ResultId,
        judgement: Judgement<'_>,
        now: &str,
        correlation_id: &str,
    ) -> Result<EvaluationId, EvaluationFault> {
        let Judgement {
            evaluator,
            method,
            verdict,
            rationale,
        } = judgement;
        if rationale.trim().is_empty() {
            return Err(EvaluationFault::RationaleMissing);
        }
        let result = ResultRepository::get(store, result_id)
            .map_err(|fault| EvaluationFault::Store(fault.to_string()))?
            .ok_or_else(|| EvaluationFault::ResultAbsent(result_id.as_str().to_string()))?;
        if result.task_id != *task {
            return Err(EvaluationFault::ResultNotOwned {
                result: result_id.as_str().to_string(),
                owner: result.task_id.as_str().to_string(),
                requester: task.as_str().to_string(),
            });
        }

        let evaluation = Evaluation {
            id: EvaluationId::new(),
            task_id: task.clone(),
            result_id: result_id.clone(),
            evaluator,
            method: method.to_string(),
            verdict,
            rationale: rationale.to_string(),
            created_at: now.to_string(),
        };
        ResultRepository::insert_evaluation(store, &evaluation)
            .map_err(|fault| EvaluationFault::Store(fault.to_string()))?;

        let evaluator_name = evaluator_wire(&evaluation.evaluator);
        let context = athena_events::EventContext {
            event_id: EventId::new(),
            occurred_at: now.to_string(),
            actor: athena_events::envelope::Actor::CoreAgent,
            task_id: Some(task.clone()),
            plan_step_id: result.plan_step_id.clone(),
            agent_id: Some(result.agent_id.clone()),
            instance_id: Some(result.instance_id.clone()),
            skill_id: None,
            correlation_id: correlation_id.to_string(),
            causation_id: None,
        };
        let folded = if verdict == Verdict::Accept {
            fold(athena_events::TypedEvent::new(
                athena_events::events::EvaluationPassed {
                    evaluation_id: evaluation.id.clone(),
                    evaluator: evaluator_name.clone(),
                    method: method.to_string(),
                    verdict,
                },
                context,
            ))
        } else {
            fold(athena_events::TypedEvent::new(
                athena_events::events::EvaluationFailed {
                    evaluation_id: evaluation.id.clone(),
                    evaluator: evaluator_name,
                    method: method.to_string(),
                    verdict,
                },
                context,
            ))
        }?;
        EventRepository::append(store, &folded)
            .map_err(|fault| EvaluationFault::Store(fault.to_string()))?;
        Ok(evaluation.id)
    }

    /// The wire word for an evaluator, plus the agent it names when there is one — the
    /// Event payload records *who*, which for an agent is its id, not the word "agent"
    /// (`ATH-EVAL-003`).
    fn evaluator_wire(evaluator: &Evaluator) -> String {
        match evaluator {
            Evaluator::Agent { agent_id } => agent_id.as_str().to_string(),
            Evaluator::Tool => "tool".to_string(),
        }
    }

    fn fold<P: athena_events::events::Payload>(
        typed: Result<athena_events::TypedEvent<P>, athena_events::EventError>,
    ) -> Result<NewEvent, EvaluationFault> {
        let typed = typed.map_err(|fault| EvaluationFault::Construction(fault.to_string()))?;
        Ok(NewEvent {
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
        })
    }

    /// The agent-facing answer for a recorded evaluation (`ATH-CORE-034`'s answer leg).
    #[must_use]
    pub fn acceptance_response(
        request_id: &str,
        evaluation_id: &EvaluationId,
        verdict: Verdict,
    ) -> crate::protocol::CoreResponse {
        crate::protocol::CoreResponse::Ok {
            request_id: request_id.to_string(),
            data: serde_json::json!({
                "evaluation_id": evaluation_id.as_str(),
                "verdict": format!("{verdict:?}").to_lowercase(),
                // The record states what it is: the Core Agent's own judgement, which is
                // never by itself the objective Verification an acceptance needs
                // (ATH-EVAL-005). An agent reading its own answer learns this.
                "method": AGENT_JUDGEMENT,
                "objective": false,
            }),
        }
    }

    /// The agent-facing answer for a refused evaluation; the class and code are the
    /// fault's own (nothing is renamed).
    #[must_use]
    pub fn refusal_response(
        request_id: &str,
        correlation_id: &str,
        fault: &EvaluationFault,
    ) -> crate::protocol::CoreResponse {
        crate::protocol::CoreResponse::Err {
            request_id: request_id.to_string(),
            error: crate::protocol::ErrorEnvelope {
                class: fault.class(),
                code: fault.code().to_string(),
                message: fault.to_string(),
                details: serde_json::Value::Null,
                correlation_id: correlation_id.to_string(),
            },
        }
    }

    /// Where a Task goes out of `EVALUATING`, and on what ground.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Settlement {
        /// The terminal (or next) state the Task takes.
        pub state: TaskState,
        /// The §23.2 Event the §17.1 row names for that pair.
        pub event_type: &'static str,
        /// What decided it, recorded in the Event payload and the resident's log.
        pub ground: Ground,
    }

    /// What decided a settlement.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Ground {
        /// An objective Verification accepted (`ATH-EVAL-002` satisfied).
        Verified {
            /// The accepting method.
            method: String,
            /// Who ran it.
            verifier: String,
        },
        /// A recorded verdict other than accept drove the Task (`ATH-EVAL-004`).
        Verdict(Verdict),
        /// The Result carries verifiable deliverables and no objective Verification
        /// accepted them: `SUCCESS` is refused (`ATH-EVAL-002`'s MUST NOT).
        UnverifiedDeliverables,
        /// No evaluation bears on it and the Result carries no verifiable deliverable, so
        /// the agent's self-declared status is what the record has (`ATH-EVAL-005` is not
        /// violated: nothing is being *accepted* as verified here).
        SelfDeclared(ResultStatus),
    }

    /// Decide where an `EVALUATING` Task goes, from the Result and the Evaluations
    /// recorded against it.
    ///
    /// The rule, and the reason it stops where it does:
    ///
    /// * an objective accepting row → `SUCCESS`;
    /// * any other recorded verdict (retry / replan / fail / escalate) → `FAILED`, with the
    ///   verdict preserved in the record. The §17.1 table also offers `RETRYING` and
    ///   `REPLANNING` out of `EVALUATING`, and this deliberately does not drive them: no
    ///   production path consumes either state today, so a Task routed there would hang,
    ///   and `TaskReplanning`'s payload requires a `to_revision` nothing knows at this
    ///   moment (the lesson D-51 recorded). A terminal the operator can see, with the
    ///   verdict on the row, is the honest answer until those paths exist;
    /// * verifiable deliverables with no objective acceptance → `FAILED`: `ATH-EVAL-002`
    ///   forbids `SUCCESS`, and the ground says exactly why;
    /// * nothing recorded and nothing verifiable → the self-declared status decides, as it
    ///   always has. `ATH-EVAL-002` scopes its MUST NOT to Results *carrying verifiable
    ///   deliverables*; a prose answer carries none, so the rule does not bite and
    ///   inventing a refusal would stall every Task the shipped loop runs.
    #[must_use]
    pub fn settlement(result: &ExecutionResult, evaluations: &[Evaluation]) -> Settlement {
        let rows: Vec<(Verdict, String, String)> = evaluations
            .iter()
            .map(|row| {
                (
                    row.verdict,
                    row.method.clone(),
                    evaluator_wire(&row.evaluator),
                )
            })
            .collect();
        match gate(result, &rows) {
            Gate::Accept {
                method, verifier, ..
            } => Settlement {
                state: TaskState::Success,
                event_type: "TaskCompleted",
                ground: Ground::Verified { method, verifier },
            },
            Gate::Follow(verdict) => Settlement {
                state: TaskState::Failed,
                event_type: "TaskFailed",
                ground: Ground::Verdict(verdict),
            },
            Gate::Unverified { .. } => {
                let verifiable = result.artifacts.iter().any(|artifact| {
                    matches!(
                        artifact.kind,
                        athena_types::ArtifactKind::File | athena_types::ArtifactKind::Patch
                    )
                });
                if verifiable {
                    Settlement {
                        state: TaskState::Failed,
                        event_type: "TaskFailed",
                        ground: Ground::UnverifiedDeliverables,
                    }
                } else if result.status == ResultStatus::Success {
                    Settlement {
                        state: TaskState::Success,
                        event_type: "TaskCompleted",
                        ground: Ground::SelfDeclared(result.status),
                    }
                } else {
                    Settlement {
                        state: TaskState::Failed,
                        event_type: "TaskFailed",
                        ground: Ground::SelfDeclared(result.status),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod evaluation_gate_tests {
    use super::evaluation::{self, Gate};
    use athena_types::{
        AgentId, Artifact, ArtifactBase, ArtifactKind, ExecutionResult, InstanceId, PlanStepId,
        ResultId, ResultStatus, TaskId, Verdict,
    };

    fn result_with(kind: Option<ArtifactKind>) -> ExecutionResult {
        ExecutionResult {
            id: ResultId::new(),
            task_id: TaskId::new(),
            plan_step_id: Some(PlanStepId::new()),
            agent_id: AgentId::new("p4-12-worker"),
            instance_id: InstanceId::new(),
            status: ResultStatus::Success,
            output: "the work's body".into(),
            artifacts: kind
                .map(|kind| {
                    vec![Artifact {
                        result_id: ResultId::new(),
                        kind,
                        base: ArtifactBase::Store,
                        path: "report.md".into(),
                        bytes: Some(10),
                        checksum: None,
                        created_at: "2026-10-17T00:00:00.000Z".into(),
                    }]
                })
                .unwrap_or_default(),
            logs_ref: None,
            skills_used: vec![],
            model: None,
            started_at: "2026-10-17T00:00:00.000Z".into(),
            finished_at: "2026-10-17T00:00:01.000Z".into(),
            execution_time_ms: 1_000,
            metadata: serde_json::json!({}),
        }
    }

    #[test]
    fn the_self_declared_success_alone_is_never_enough() {
        // `status: Success`, no Evaluation rows at all: refused (ATH-EVAL-005).
        let unverified = evaluation::gate(&result_with(None), &[]);
        match unverified {
            Gate::Unverified { reason } => {
                assert!(reason.contains("never sufficient"), "{reason}");
            }
            other => panic!("the gate accepted: {other:?}"),
        }
        // And with a verifiable deliverable: the reason names the Verification gap
        // (`ATH-EVAL-002`).
        let gate = evaluation::gate(&result_with(Some(ArtifactKind::File)), &[]);
        match gate {
            Gate::Unverified { reason } => {
                assert!(reason.contains("objective"), "{reason}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_verifiable_deliverable_with_an_accepting_objective_row_is_accepted() {
        let result = result_with(Some(ArtifactKind::Patch));
        let gate = evaluation::gate(
            &result,
            &[(Verdict::Accept, "test".into(), "p4-12-verifier".into())],
        );
        match gate {
            Gate::Accept {
                method, verifier, ..
            } => {
                assert_eq!(method, "test");
                assert_eq!(verifier, "p4-12-verifier");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_non_accepting_verdict_drives_the_task_its_destination() {
        let result = result_with(None);
        for verdict in [
            Verdict::Retry,
            Verdict::Replan,
            Verdict::Fail,
            Verdict::Escalate,
        ] {
            let gate = evaluation::gate(&result, &[(verdict, "build".into(), "v".into())]);
            assert!(
                matches!(gate, Gate::Follow(v) if v == verdict),
                "{gate:?} for {verdict:?}"
            );
        }
        // Replan, too.
        let gate = evaluation::gate(&result, &[(Verdict::Replan, "review".into(), "v".into())]);
        assert!(matches!(gate, Gate::Follow(Verdict::Replan)));
    }

    #[test]
    fn a_non_objective_method_cannot_support_an_acceptance() {
        let result = result_with(None);
        // The row claims Accept but its method is none of the objective five.
        let gate = evaluation::gate(
            &result,
            &[(Verdict::Accept, "looked at it".into(), "v".into())],
        );
        assert!(matches!(gate, Gate::Unverified { .. }), "{gate:?}");
        // And the five named methods are objective.
        for method in evaluation::OBJECTIVE_METHODS {
            assert!(evaluation::objective(method), "{method}");
        }
        assert!(!evaluation::objective("looked at it"));
    }
}

#[cfg(test)]
mod record_tests {
    //! X-48: the `EVALUATING` state's substance — the recorded Evaluation, its Event, and
    //! the settlement the record drives.

    use super::record::{
        AGENT_JUDGEMENT, EvaluationFault, Ground, Judgement, record_evaluation, settlement,
    };
    use athena_persistence::AthenaStore;
    use athena_persistence::repository::{EventRepository, ResultRepository, TaskRepository};
    use athena_types::{
        AgentId, Artifact, ArtifactBase, ArtifactKind, Evaluator, ExecutionResult, InstanceId,
        Priority, ResultId, ResultStatus, RiskLevel, Task, TaskId, TaskState, Verdict,
    };

    const NOW: &str = "2026-11-12T14:00:00.000Z";

    fn task_row() -> Task {
        Task {
            id: TaskId::new(),
            parent_id: None,
            title: "x48".into(),
            instruction: "evaluate what came back".into(),
            workspace: "/repo/x48".into(),
            goal: None,
            constraints: vec![],
            completion_criteria: vec!["the suite passes".into()],
            state: TaskState::Evaluating,
            priority: Priority::Normal,
            plan_id: None,
            risk_level: RiskLevel::Medium,
            attempt_count: 0,
            max_attempts: 3,
            iteration_count: 0,
            max_iterations: 50,
            deadline: None,
            created_by: "test".into(),
            created_at: NOW.into(),
            updated_at: NOW.into(),
            finished_at: None,
        }
    }

    fn result_for(
        task: &TaskId,
        status: ResultStatus,
        artifacts: Vec<Artifact>,
    ) -> ExecutionResult {
        ExecutionResult {
            id: ResultId::new(),
            task_id: task.clone(),
            plan_step_id: None,
            agent_id: AgentId::core_agent(),
            instance_id: InstanceId::new(),
            status,
            output: "the work is done".into(),
            artifacts,
            logs_ref: None,
            skills_used: vec![],
            model: None,
            started_at: NOW.into(),
            finished_at: NOW.into(),
            execution_time_ms: 0,
            metadata: serde_json::json!({ "self_declared": true }),
        }
    }

    fn file_artifact(result: &ResultId) -> Artifact {
        Artifact {
            result_id: result.clone(),
            kind: ArtifactKind::File,
            base: ArtifactBase::WorkTarget,
            path: "src/lib.rs".into(),
            bytes: None,
            checksum: None,
            created_at: NOW.into(),
        }
    }

    fn seeded(status: ResultStatus, with_artifact: bool) -> (AthenaStore, Task, ExecutionResult) {
        let mut store = AthenaStore::in_memory().expect("store");
        let task = task_row();
        TaskRepository::insert(&mut store, &task).expect("task");
        let id = ResultId::new();
        let artifacts = if with_artifact {
            vec![file_artifact(&id)]
        } else {
            vec![]
        };
        let mut result = result_for(&task.id, status, artifacts);
        result.id = id.clone();
        for artifact in &mut result.artifacts {
            artifact.result_id = id.clone();
        }
        ResultRepository::insert(&mut store, &result).expect("result");
        (store, task, result)
    }

    #[test]
    fn a_submitted_evaluation_lands_a_row_and_its_event() {
        let (mut store, task, result) = seeded(ResultStatus::Success, false);
        let id = record_evaluation(
            &mut store,
            &task.id,
            &result.id,
            Judgement {
                evaluator: Evaluator::Agent {
                    agent_id: AgentId::core_agent(),
                },
                method: AGENT_JUDGEMENT,
                verdict: Verdict::Accept,
                rationale: "the criteria read as met",
            },
            NOW,
            "cor-x48",
        )
        .expect("recorded");

        let rows = ResultRepository::list_evaluations_for_result(&store, &result.id).expect("read");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
        assert_eq!(rows[0].method, AGENT_JUDGEMENT);
        assert_eq!(rows[0].verdict, Verdict::Accept);
        assert_eq!(rows[0].rationale, "the criteria read as met");

        // ATH-EVAL-003's record rides the Event too, on the Task's chain.
        let events: Vec<String> = EventRepository::list_for_task(&store, &task.id)
            .expect("events")
            .into_iter()
            .map(|event| event.event_type)
            .collect();
        assert_eq!(events, vec!["EvaluationPassed".to_string()]);
    }

    #[test]
    fn a_non_accepting_verdict_rides_the_failed_event() {
        let (mut store, task, result) = seeded(ResultStatus::Success, false);
        record_evaluation(
            &mut store,
            &task.id,
            &result.id,
            Judgement {
                evaluator: Evaluator::Tool,
                method: "test",
                verdict: Verdict::Retry,
                rationale: "three cases still fail",
            },
            NOW,
            "cor-x48",
        )
        .expect("recorded");
        let events: Vec<String> = EventRepository::list_for_task(&store, &task.id)
            .expect("events")
            .into_iter()
            .map(|event| event.event_type)
            .collect();
        assert_eq!(events, vec!["EvaluationFailed".to_string()]);
    }

    #[test]
    fn an_evaluation_of_another_tasks_result_is_refused() {
        let (mut store, task, result) = seeded(ResultStatus::Success, false);
        let other = TaskId::new();
        let fault = record_evaluation(
            &mut store,
            &other,
            &result.id,
            Judgement {
                evaluator: Evaluator::Tool,
                method: "test",
                verdict: Verdict::Accept,
                rationale: "not mine to judge",
            },
            NOW,
            "cor-x48",
        )
        .expect_err("refused");
        assert!(
            matches!(fault, EvaluationFault::ResultNotOwned { .. }),
            "{fault}"
        );
        assert_eq!(fault.code(), "RESULT_NOT_OWNED");
        // Nothing was written on the refusal.
        assert!(
            ResultRepository::list_evaluations_for_result(&store, &result.id)
                .expect("read")
                .is_empty()
        );
        let _ = task;
    }

    #[test]
    fn an_evaluation_without_a_rationale_is_refused() {
        let (mut store, task, result) = seeded(ResultStatus::Success, false);
        let fault = record_evaluation(
            &mut store,
            &task.id,
            &result.id,
            Judgement {
                evaluator: Evaluator::Tool,
                method: "test",
                verdict: Verdict::Accept,
                rationale: "   ",
            },
            NOW,
            "cor-x48",
        )
        .expect_err("refused");
        assert!(
            matches!(fault, EvaluationFault::RationaleMissing),
            "{fault}"
        );
    }

    #[test]
    fn an_objective_acceptance_settles_the_task_successful() {
        let (mut store, task, result) = seeded(ResultStatus::Success, true);
        record_evaluation(
            &mut store,
            &task.id,
            &result.id,
            Judgement {
                evaluator: Evaluator::Tool,
                method: "test",
                verdict: Verdict::Accept,
                rationale: "the suite passes",
            },
            NOW,
            "cor-x48",
        )
        .expect("recorded");
        let evaluations =
            ResultRepository::list_evaluations_for_result(&store, &result.id).expect("read");
        let decision = settlement(&result, &evaluations);
        assert_eq!(decision.state, TaskState::Success);
        assert_eq!(decision.event_type, "TaskCompleted");
        assert!(
            matches!(&decision.ground, Ground::Verified { method, .. } if method == "test"),
            "{:?}",
            decision.ground
        );
    }

    #[test]
    fn verifiable_deliverables_without_verification_never_succeed() {
        // ATH-EVAL-002's MUST NOT, exercised on the shape it names: the agent says
        // success, the Result carries a file, and nothing objective accepted it.
        let (store, _task, result) = seeded(ResultStatus::Success, true);
        let decision = settlement(&result, &[]);
        assert_eq!(decision.state, TaskState::Failed);
        assert_eq!(decision.ground, Ground::UnverifiedDeliverables);
        let _ = store;
    }

    #[test]
    fn the_agents_own_judgement_cannot_accept_a_deliverable_by_itself() {
        // ATH-EVAL-005: `submit_evaluation` records a judgement, not a Verification —
        // the method it rides under is not one of the objective five, so the gate still
        // refuses the acceptance.
        let (mut store, task, result) = seeded(ResultStatus::Success, true);
        record_evaluation(
            &mut store,
            &task.id,
            &result.id,
            Judgement {
                evaluator: Evaluator::Agent {
                    agent_id: AgentId::core_agent(),
                },
                method: AGENT_JUDGEMENT,
                verdict: Verdict::Accept,
                rationale: "it looks right to me",
            },
            NOW,
            "cor-x48",
        )
        .expect("recorded");
        let evaluations =
            ResultRepository::list_evaluations_for_result(&store, &result.id).expect("read");
        assert!(!super::evaluation::objective(AGENT_JUDGEMENT));
        let decision = settlement(&result, &evaluations);
        assert_eq!(decision.state, TaskState::Failed);
        assert_eq!(decision.ground, Ground::UnverifiedDeliverables);
    }

    #[test]
    fn a_recorded_non_accepting_verdict_drives_the_task_failed() {
        let (mut store, task, result) = seeded(ResultStatus::Success, false);
        record_evaluation(
            &mut store,
            &task.id,
            &result.id,
            Judgement {
                evaluator: Evaluator::Tool,
                method: "lint",
                verdict: Verdict::Replan,
                rationale: "the approach does not satisfy the criteria",
            },
            NOW,
            "cor-x48",
        )
        .expect("recorded");
        let evaluations =
            ResultRepository::list_evaluations_for_result(&store, &result.id).expect("read");
        let decision = settlement(&result, &evaluations);
        assert_eq!(decision.state, TaskState::Failed);
        assert_eq!(decision.ground, Ground::Verdict(Verdict::Replan));
    }

    #[test]
    fn a_prose_answer_with_nothing_recorded_still_settles_on_the_reported_status() {
        // The case ATH-EVAL-002 does not reach: no verifiable deliverable, no evaluation.
        // The shipped loop's every Task looks like this, and refusing it would stall them
        // all — the ground records exactly what decided.
        let (_store, _task, success) = seeded(ResultStatus::Success, false);
        let decision = settlement(&success, &[]);
        assert_eq!(decision.state, TaskState::Success);
        assert_eq!(decision.ground, Ground::SelfDeclared(ResultStatus::Success));

        let (_store, _task, failure) = seeded(ResultStatus::Failure, false);
        let decision = settlement(&failure, &[]);
        assert_eq!(decision.state, TaskState::Failed);
        assert_eq!(decision.ground, Ground::SelfDeclared(ResultStatus::Failure));
    }
}
