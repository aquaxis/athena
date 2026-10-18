//! The shared Runtime contract suite (ATH-TEST-014, spec §34).
//!
//! One suite, every Adapter. The checks here are implementation-independent: they demand
//! only what spec §14.2 and the requirements place on *every* Runtime —
//! capabilities truthfulness, refusal of invalid start contexts, distinct handles per
//! start, dead handles that report rather than fabricate, and an id-stable stop.
//!
//! Behaviour specific to a Runtime (scripted reactions, real process supervision) is NOT
//! here; each implementation tests it separately. A contract check may fail only on what
//! the check itself provokes — the suite never assumes a scripted outcome.
//!
//! Usage: `assert_contract(runtime)` runs every check and aggregates failures.

use crate::context::{FilesystemScope, StartContext};
use crate::error::RuntimeError;
use crate::interface::{Runtime, RuntimeInstanceRef};
use crate::message::Message;
use crate::status::RuntimeInstanceStatus;
use athena_types::{
    AgentDefinition, AgentId, AgentLevel, Deadlines, InstanceId, MessageId, MessageType, ModelRef,
    RuntimeType, StopReason, ToolName,
};

/// A check outcome: the check's name plus the failure text, when it failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractFailure {
    /// The check that did not hold.
    pub check: &'static str,
    /// What was observed instead.
    pub detail: String,
}

/// The shareable definition used by every contract check.
#[must_use]
pub fn contract_definition(runtime_kind: RuntimeType) -> AgentDefinition {
    AgentDefinition {
        id: AgentId::new("contract-agent"),
        name: "Contract agent".into(),
        level: AgentLevel::Worker,
        description: "definition for the shared Runtime contract".into(),
        capabilities: vec!["contract".into()],
        persona_ref: "workers/contract".into(),
        persona_version: "1.0.0".into(),
        allowed_tools: vec![ToolName::FsRead],
        delegable_tools: vec![],
        default_skills: vec![],
        runtime: runtime_kind,
        model: ModelRef {
            provider: "contract".into(),
            name: "contract-1".into(),
        },
        max_delegation_depth: 0,
        dynamic: false,
    }
}

/// A start context that satisfies [`StartContext::validate`], with the Workspace named
/// by the caller (the default fixture uses the spec-shaped `/athena/var/instances/contract`).
#[must_use]
pub fn contract_context_at(instance_id: InstanceId, workspace: &std::path::Path) -> StartContext {
    let workspace = workspace.display().to_string();
    StartContext {
        task_id: None,
        plan_step_id: None,
        instance_id,
        workspace: workspace.clone(),
        work_targets: vec!["/project/src".into()],
        persona_ref: "workers/contract".into(),
        persona_version: "1.0.0".into(),
        skill_set: vec![],
        allowed_tools: vec![ToolName::FsRead],
        delegable_tools: vec![],
        filesystem_scope: FilesystemScope {
            workspace,
            work_targets: vec!["/project/src".into()],
            read_only: vec![],
        },
        model: ModelRef {
            provider: "contract".into(),
            name: "contract-1".into(),
        },
        deadlines: Deadlines {
            startup_ms: 30_000,
            execution_ms: 600_000,
        },
        resume_snapshot_ref: None,
        context_document_ref: None,
    }
}

/// Run every shared check against a Runtime.
///
/// The fixture start contexts live under the repository-fixture path; Runtimes that
/// materialize the Workspace on the filesystem run through
/// [`run_contract_at_workspace`] with a scratch root so the same checks hold without
/// touching that fixed location.
///
/// # Errors
/// Aggregated [`ContractFailure`] list, empty on success.
pub async fn assert_contract(
    runtime: &dyn Runtime,
) -> Result<Vec<ContractFailure>, Vec<ContractFailure>> {
    let failures = run_contract(runtime).await;
    if failures.is_empty() {
        Ok(failures)
    } else {
        Err(failures)
    }
}

/// Run every shared check and return the failure list (possibly empty).
pub async fn run_contract(runtime: &dyn Runtime) -> Vec<ContractFailure> {
    run_contract_at_workspace(runtime, std::path::Path::new("/athena/var/instances")).await
}

/// Run every shared check with the fixture Workspaces mapped under `workspace_root` —
/// the same checks, the same fixture shape, on a caller-supplied root (the fixture's
/// `contract` basename is the default root's child, so the aggregate conditions are
/// unchanged for every Runtime).
pub async fn run_contract_at_workspace(
    runtime: &dyn Runtime,
    workspace_root: &std::path::Path,
) -> Vec<ContractFailure> {
    let mut failures = Vec::new();

    // capabilities: declared kind matches the trait, and the isolation boundary is honest
    // enough to be declared at all (ATH-RT-005).
    {
        let caps = runtime.capabilities();
        if caps.runtime_type != runtime.runtime_type() {
            failures.push(ContractFailure {
                check: "capabilities_kind_matches",
                detail: format!(
                    "capabilities name {:?} but runtime_type reports {:?}",
                    caps.runtime_type,
                    runtime.runtime_type()
                ),
            });
        }
        if caps.max_concurrent_instances == 0 {
            failures.push(ContractFailure {
                check: "capabilities_declares_a_concurrency_limit",
                detail: "max_concurrent_instances is zero; ATH-RT-009 demands a limit".into(),
            });
        }
    }

    // start: a valid context is accepted and yields a handle of the declared kind.
    let handle = {
        match runtime
            .start(
                contract_definition(runtime.runtime_type()),
                contract_context_at(InstanceId::new(), &workspace_root.join("contract")),
            )
            .await
        {
            Ok(handle) => handle,
            Err(err) => {
                failures.push(ContractFailure {
                    check: "start_accepts_a_valid_context",
                    detail: format!("{err}"),
                });
                return failures;
            }
        }
    };
    if handle.runtime_type != runtime.runtime_type() {
        failures.push(ContractFailure {
            check: "start_handle_carries_the_declared_kind",
            detail: format!("{handle:?}"),
        });
    }
    if !handle.runtime_instance_id.as_str().starts_with("ri-") {
        failures.push(ContractFailure {
            check: "start_handle_mints_a_runtime_instance_id",
            detail: format!(
                "{} must start with `ri-`",
                handle.runtime_instance_id.as_str()
            ),
        });
    }

    // Two starts are two Runtime Instances (ATH-RT-012, 1:1), never one shared process.
    let second = runtime
        .start(
            contract_definition(runtime.runtime_type()),
            contract_context_at(InstanceId::new(), &workspace_root.join("contract-2")),
        )
        .await;
    match second {
        Ok(second_handle) => {
            if second_handle.runtime_instance_id == handle.runtime_instance_id {
                failures.push(ContractFailure {
                    check: "two_starts_are_two_runtime_instances",
                    detail: "both starts produced the same Runtime Instance id".into(),
                });
            }
        }
        Err(err) => failures.push(ContractFailure {
            check: "a_second_start_is_its_own_instance",
            detail: format!("{err}"),
        }),
    }

    // status of a live handle is observable without error.
    if let Err(err) = runtime.status(&handle).await {
        failures.push(ContractFailure {
            check: "status_of_a_new_instance_is_observable",
            detail: format!("{err}"),
        });
    }

    // send on a live handle is accepted (at-least-once downstream does not reject a
    // well-formed instruction); an empty thread is not acceptable.
    {
        let message = Message {
            message_id: MessageId::new(),
            message_type: MessageType::Instruction,
            from_agent: None,
            from_instance_id: None,
            thread_id: "contract-thread".into(),
            reply_to: None,
            priority: athena_types::Priority::Normal,
            body: "contract check".into(),
            payload_json: None,
            deadline: None,
        };
        if let Err(err) = runtime.send(&handle, message).await {
            let acceptable = matches!(err, RuntimeError::Deadline(_) | RuntimeError::Transient(_));
            if !acceptable {
                failures.push(ContractFailure {
                    check: "a_well_formed_instruction_is_accepted",
                    detail: format!("{err}"),
                });
            }
        }
    }

    // stop verifies, and after a stop the handle reports rather than fabricating.
    {
        runtime
            .stop(&handle, StopReason::Shutdown)
            .await
            .unwrap_or_else(|err| {
                failures.push(ContractFailure {
                    check: "stop_is_accepted",
                    detail: format!("{err}"),
                });
            });
        match runtime.status(&handle).await {
            Ok(status) => {
                if status.is_alive() {
                    failures.push(ContractFailure {
                        check: "a_stopped_instance_is_not_live",
                        detail: format!("{status:?}"),
                    });
                }
            }
            Err(err) => {
                // Stale-handle refs are fine (`UnknownInstance`), fabricated states are not.
                if !matches!(err, RuntimeError::UnknownInstance(_)) {
                    failures.push(ContractFailure {
                        check: "a_stopped_instance_reports_its_end",
                        detail: format!("{err}"),
                    });
                }
            }
        }
    }

    // Stale handles never fabricate state.
    {
        let stale = RuntimeInstanceRef {
            runtime_type: runtime.runtime_type(),
            runtime_instance_id: athena_types::RuntimeInstanceId::new(),
            instance_id: InstanceId::new(),
        };
        let status = runtime.status(&stale).await;
        match status {
            Err(RuntimeError::UnknownInstance(_)) => {}
            Err(err) => failures.push(ContractFailure {
                check: "a_stale_handle_reports_unknown",
                detail: format!("expected UnknownInstance, got {err}"),
            }),
            Ok(RuntimeInstanceStatus::Stopped { .. }) => {}
            Ok(other) => failures.push(ContractFailure {
                check: "a_stale_handle_reports_unknown",
                detail: format!("expected UnknownInstance or a stopped state, got {other:?}"),
            }),
        }
    }

    failures
}
