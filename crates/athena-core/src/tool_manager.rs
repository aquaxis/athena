//! Tool Manager — `ToolExecution` records and grant enforcement (spec §11).
//!
//! The enforcement point for out-of-grant requests (`PermissionError`, R-TOOL-02), for
//! filesystem path boundaries (R-TOOL-05), and for high-risk executions that require a
//! consumed approval (R-TOOL-03) — in concert with the Policy Manager, which classifies.
//!
//! Owned entities (`OWNED`): ToolExecution. Emitted Events: `ToolExecution*`.

use crate::ownership::{Entity, ModuleRole};

/// The entities this module may mutate.
pub const OWNED: &[(Entity, ModuleRole)] = &[(Entity::ToolExecution, ModuleRole::ToolManager)];

pub mod enforcement {
    //! P4-02: the Tool Manager as the enforcement point for out-of-grant requests.
    //!
    //! The decision is pure and Core-Host-side (`ATH-SEC-022`): a grant set is the only
    //! input, the requested tool is the only target — agent prose plays no part
    //! (`ATH-CORE-022`, `ATH-CORE-038`). The refusal is `PermissionError`, the §20.1
    //! class `ATH-TOOL-002` prescribes for an out-of-grant request; the caller (the Core
    //! Host supervision wiring) records the `ToolExecution` row with status `Denied` and
    //! the `PrivilegedRequestDenied` Event through the same store primitives the
    //! Inspector uses (D-1).
    //!
    //! `ATH-TOOL-05 path boundaries` live with P4-03; the approval requirement for the
    //! high-risk tools rides the Policy Manager's classification (P1-24's first-match)
    //! and the Approval Manager (`ATH-TOOL-03`).

    use crate::ownership::{Entity, ModuleRole};
    use crate::protocol::ErrorClass;
    use athena_tools::ToolGrants;

    /// The one decision P4-02 owns: is this execution inside the instance's grant?
    ///
    /// `Ok(())` for a permitted tool; `Err(PermissionError)` — with the §20.1 envelope
    /// facts already shaped — when the tool is out of grant, the deny-by-default
    /// decision (`ATH-TOOL-001`).
    ///
    /// # Errors
    /// The `PermissionError` classification for an out-of-grant tool (never a guess —
    /// deny is the default when nothing is granted, [`athena_tools::ToolGrants`]).
    pub fn check_grant(
        grants: &ToolGrants,
        requested: athena_types::ToolName,
    ) -> Result<(), Denied> {
        if grants.permitted(requested) {
            return Ok(());
        }
        Err(Denied {
            tool: requested,
            policy_id: format!("agent.grant.{}", requested),
            error_class: ErrorClass::PermissionError,
            detail: String::new(),
        })
    }

    /// One refusal, the facts the Event and the store row carry (an out-of-grant tool or
    /// an out-of-boundary path).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Denied {
        /// The refused tool.
        pub tool: athena_types::ToolName,
        /// The rule the refusal came from (`agent.grant.<tool>` / `agent.fs_boundary`).
        pub policy_id: String,
        /// The §20.1 class (`PermissionError`, `ATH-TOOL-002` / `ATH-TOOL-005`).
        pub error_class: ErrorClass,
        /// The refusal's own surface text (empty for the bare grant denial).
        pub detail: String,
    }

    impl Denied {
        /// The §22.2 envelope text.
        #[must_use]
        pub fn envelope(&self) -> String {
            format!(
                "tool {0:?} is outside the instance's grant; denied by default \
                 (ATH-TOOL-001, {1:?})",
                self.tool, self.error_class
            )
        }
    }

    /// Silent ownership declaration (drift-checked like every Core Host module).
    #[must_use]
    pub const fn owned_entities() -> &'static [(Entity, ModuleRole)] {
        &[(Entity::ToolExecution, ModuleRole::ToolManager)]
    }

    /// The `ToolExecution*` Event builder (P4-02): the §23.2 payload rows
    /// (`tool_execution_id`, `tool`, `operation_descriptor`, `risk_level`) materialized
    /// onto the §23.1 envelope for the Event Manager's append. `Denied` returns no
    /// `ToolExecution*` Event: its facts ride `PrivilegedRequestDenied` and the row's
    /// own `Denied` status (the §23.2 catalog has no Denied kind).
    ///
    /// # Errors
    /// Event construction failures (missing field, forbidden content, invalid envelope),
    /// and [`MaterializeFault::DeniedHasNoEvent`] when the caller hands a `Denied`
    /// execution here (a caller bug, not an observation).
    pub fn materialize(
        execution: &athena_types::ToolExecution,
        correlation_id: String,
    ) -> Result<Materialized, MaterializeFault> {
        match execution.status {
            athena_types::ToolExecutionStatus::Started => {
                let event = athena_events::TypedEvent::new(
                    athena_events::events::ToolExecutionStarted {
                        tool_execution_id: execution.id.clone(),
                        tool: execution.tool,
                        operation_descriptor: serde_json::Value::String(
                            execution.operation_descriptor.clone(),
                        ),
                        risk_level: execution.risk_level,
                    },
                    context_of(execution, correlation_id),
                )
                .map_err(|error| MaterializeFault::Event(error.to_string()))?;
                Ok(Materialized {
                    event_type: event.event_type,
                    envelope: event.envelope(),
                })
            }
            athena_types::ToolExecutionStatus::Succeeded => {
                let event = athena_events::TypedEvent::new(
                    athena_events::events::ToolExecutionCompleted {
                        tool_execution_id: execution.id.clone(),
                        tool: execution.tool,
                        operation_descriptor: serde_json::Value::String(
                            execution.operation_descriptor.clone(),
                        ),
                        risk_level: execution.risk_level,
                    },
                    context_of(execution, correlation_id),
                )
                .map_err(|error| MaterializeFault::Event(error.to_string()))?;
                Ok(Materialized {
                    event_type: event.event_type,
                    envelope: event.envelope(),
                })
            }
            athena_types::ToolExecutionStatus::Failed => {
                let event = athena_events::TypedEvent::new(
                    athena_events::events::ToolExecutionFailed {
                        tool_execution_id: execution.id.clone(),
                        tool: execution.tool,
                        operation_descriptor: serde_json::Value::String(
                            execution.operation_descriptor.clone(),
                        ),
                        risk_level: execution.risk_level,
                    },
                    context_of(execution, correlation_id),
                )
                .map_err(|error| MaterializeFault::Event(error.to_string()))?;
                Ok(Materialized {
                    event_type: event.event_type,
                    envelope: event.envelope(),
                })
            }
            athena_types::ToolExecutionStatus::Denied => Err(MaterializeFault::DeniedHasNoEvent),
        }
    }

    /// The `fs_read` / `fs_write` path-boundary face (`ATH-TOOL-005`): outside the
    /// boundary ⇒ `PermissionError`, with the boundary fault's own text preserved. The
    /// decision is [`athena_tools::PathBoundary`]'s; this names it the Tool Manager's and
    /// shapes the §20.1 refusal.
    ///
    /// # Errors
    /// [`Denied`] with policy id `agent.fs_boundary` on any boundary refusal; a
    /// non-filesystem tool handed here is the caller's bug, refused the same way.
    pub fn check_path_boundary(
        boundary: &athena_tools::PathBoundary,
        tool: athena_types::ToolName,
        path: &std::path::Path,
    ) -> Result<&'static str, Denied> {
        match boundary.permits(tool, path) {
            Ok(mode) => Ok(mode),
            Err(fault) => Err(Denied {
                tool,
                policy_id: "agent.fs_boundary".into(),
                error_class: ErrorClass::PermissionError,
                detail: fault.to_string(),
            }),
        }
    }

    fn context_of(
        execution: &athena_types::ToolExecution,
        correlation_id: String,
    ) -> athena_events::EventContext {
        athena_events::EventContext {
            event_id: athena_types::EventId::new(),
            occurred_at: execution
                .finished_at
                .clone()
                .unwrap_or_else(|| execution.started_at.clone()),
            actor: athena_types::Actor::System,
            task_id: execution.task_id.clone(),
            plan_step_id: None,
            agent_id: None,
            instance_id: Some(execution.instance_id.clone()),
            skill_id: None,
            correlation_id,
            causation_id: None,
        }
    }

    /// Why `materialize` refused (P4-02).
    #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
    pub enum MaterializeFault {
        /// Event construction refused (missing field, forbidden content, invalid envelope).
        #[error("the ToolExecution Event construction refused: {0}")]
        Event(String),
        /// The catalog has no Denied kind; the caller must arm the row and
        /// `PrivilegedRequestDenied` instead (spec section 23.2).
        #[error(
            "a Denied execution has no ToolExecution Event; arm the row and PrivilegedRequestDenied (spec section 23.2)"
        )]
        DeniedHasNoEvent,
    }

    /// One materialized Event, ready for the Event Manager's append.
    #[derive(Debug, Clone)]
    pub struct Materialized {
        /// The Event's name (`ToolExecutionStarted` / `Completed` / `Failed`).
        pub event_type: &'static str,
        /// The §23.1 envelope with the payload already hygiene-checked.
        pub envelope: athena_events::envelope::EventEnvelope,
    }
    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_granted_tool_passes_and_an_out_of_grant_tool_is_denied_by_default() {
            // The §11.2 statement shape: only the granted tools run.
            let grants = athena_tools::ToolGrants::from_allowed(&[athena_types::ToolName::FsRead])
                .expect("a valid statement");
            assert!(
                check_grant(&grants, athena_types::ToolName::FsRead).is_ok(),
                "the granted tool executes"
            );
            let denied = check_grant(&grants, athena_types::ToolName::Shell)
                .expect_err("an out-of-grant tool is refused (ATH-TOOL-002)");
            assert_eq!(denied.error_class, ErrorClass::PermissionError);
            assert_eq!(denied.policy_id, "agent.grant.shell");
            assert!(denied.envelope().contains("denied by default"));
        }

        #[test]
        fn the_fs_boundary_refuses_out_of_boundary_paths_as_permission_errors() {
            use athena_tools::PathBoundary;
            use std::path::PathBuf;

            // The P3-12 shape: workspace + work targets writable, conventions read-only,
            // the control surface and other areas excluded.
            let boundary = PathBoundary {
                writable: vec![
                    PathBuf::from("/athena/var/instances/ai-1"),
                    PathBuf::from("/repo/src"),
                ],
                read_only: vec![PathBuf::from("/repo/.aiprj/rules")],
                excluded: vec![PathBuf::from("/athena/var/instances/ai-1/.agent-cli")],
            };

            // Inside the workspace: an fs_write proceeds.
            assert_eq!(
                check_path_boundary(
                    &boundary,
                    athena_types::ToolName::FsWrite,
                    std::path::Path::new("/athena/var/instances/ai-1/out.txt"),
                )
                .expect("inside the boundary"),
                "write"
            );

            // Out of the workspace ⇒ PermissionError with the fs_boundary policy id.
            let refused = check_path_boundary(
                &boundary,
                athena_types::ToolName::FsWrite,
                std::path::Path::new("/etc/hosts"),
            )
            .expect_err("out of boundary (ATH-TOOL-005)");
            assert_eq!(refused.error_class, ErrorClass::PermissionError);
            assert_eq!(refused.policy_id, "agent.fs_boundary");
            assert!(refused.detail.contains("out of the filesystem boundary"));

            // The control surface refuses even inside the workspace (ATH-CORE-043).
            let control = check_path_boundary(
                &boundary,
                athena_types::ToolName::FsWrite,
                std::path::Path::new("/athena/var/instances/ai-1/.agent-cli/config.toml"),
            )
            .expect_err("the control surface is excluded");
            assert!(control.detail.contains("excluded root"));

            // The conventions are readable, never rewritable.
            assert!(
                check_path_boundary(
                    &boundary,
                    athena_types::ToolName::FsRead,
                    std::path::Path::new("/repo/.aiprj/rules/exec_job.md"),
                )
                .is_ok()
            );
            assert!(
                check_path_boundary(
                    &boundary,
                    athena_types::ToolName::FsWrite,
                    std::path::Path::new("/repo/.aiprj/rules/exec_job.md"),
                )
                .is_err()
            );
        }

        #[test]
        fn the_execution_events_materialize_with_the_payload_rows() {
            use athena_types::{InstanceId, ToolExecution, ToolExecutionStatus};

            let execution = ToolExecution {
                id: athena_types::ToolExecutionId::new(),
                instance_id: InstanceId::new(),
                task_id: Some(athena_types::TaskId::new()),
                tool: athena_types::ToolName::FsWrite,
                operation_descriptor: "fs_write(target=/repo/src/lib.rs)".into(),
                risk_level: athena_types::RiskLevel::Medium,
                approval_id: None,
                status: ToolExecutionStatus::Started,
                started_at: "2026-10-17T09:00:00Z".into(),
                finished_at: None,
            };

            // Started materializes `ToolExecutionStarted` with the §23.2 rows.
            let started = materialize(&execution, "cor-p4-02".into()).expect("started");
            assert_eq!(started.event_type, "ToolExecutionStarted");
            assert_eq!(started.envelope.kind.as_str(), "ToolExecutionStarted");
            let wire: serde_json::Value =
                serde_json::from_str(&started.envelope.payload_json).expect("payload");
            assert_eq!(
                wire.get("tool_execution_id").and_then(|v| v.as_str()),
                Some(execution.id.as_str())
            );
            assert_eq!(wire.get("tool").and_then(|v| v.as_str()), Some("fs_write"));
            assert!(wire.get("operation_descriptor").is_some());
            assert_eq!(
                wire.get("risk_level").and_then(|v| v.as_str()),
                Some("medium")
            );

            // Succeeded → Completed; a Denied execution has no ToolExecution Event.
            let mut finished = execution.clone();
            finished.status = ToolExecutionStatus::Succeeded;
            finished.finished_at = Some("2026-10-17T09:01:00Z".into());
            assert_eq!(
                materialize(&finished, "cor-p4-02".into())
                    .expect("completed")
                    .event_type,
                "ToolExecutionCompleted"
            );

            let mut refused = execution.clone();
            refused.status = ToolExecutionStatus::Denied;
            assert!(
                matches!(
                    materialize(&refused, "cor-p4-02".into()),
                    Err(MaterializeFault::DeniedHasNoEvent)
                ),
                "the §23.2 catalog has no Denied kind"
            );
        }

        #[test]
        fn no_statement_denies_everything() {
            let none = athena_tools::ToolGrants::denying_all();
            for tool in athena_tools::BASE_TOOLS {
                assert!(
                    check_grant(&none, tool.name).is_err(),
                    "deny by default holds for {}",
                    tool.name
                );
            }
        }
    }
}
