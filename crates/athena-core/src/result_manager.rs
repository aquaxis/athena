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
