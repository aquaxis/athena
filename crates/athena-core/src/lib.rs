//! Core Host (spec §7.5) — supervisor, recorder, and policy enforcement point.
//!
//! Core decides **nothing** (ATH-CORE-022). Every module here owns exactly the entities the
//! §26.2 ownership table grants it (ATH-DATA-003), emits exactly the Events that table
//! names, and never calls another module's mutation path directly (D-1): cross-module
//! effects travel through the State Manager / Event Manager pair so that the
//! "transition + Event in one transaction" rule (ATH-PERS-003) has a single implementation
//! point.
//!
//! Module map (design §3.3):
//!
//! | Module | Entity ownership | Events |
//! |---|---|---|
//! | [`core_agent_session`] | Core Agent Instance lifecycle | `CoreAgent*` |
//! | [`agent_manager`] | `AgentDefinition` | — |
//! | [`broker`] | `Message`, `message_deliveries` | `AgentMessage*` |
//! | [`runtime_manager`] | `AgentInstance`, `RuntimeInstance` | `Agent*`, `RuntimeInstance*` |
//! | [`tool_manager`] | `ToolExecution` | `ToolExecution*` |
//! | [`memory_manager`] | `MemoryRecord` | `MemoryRecord*` |
//! | [`state_manager`] | `Task`, `Plan`, `PlanStep` | `Task*`, `Plan*` |
//! | [`event_manager`] | `events` append + SSE fan-out | — (the emitter) |
//! | [`approval_manager`] | `Approval` | `Approval*` |
//! | [`result_manager`] | `Result`, `Artifact`, `Evaluation` | `TaskResultReceived`, `Evaluation*` |
//! | [`scheduler`] | PlanStep queue | `SchedulerBlocked`, `TaskStalled` |
//! | [`policy_manager`] | risk classification, permission checks | `PrivilegedRequestDenied` |
//! | [`protocol`] | — (the boundary Core ↔ Core Agent) | — |

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod agent_manager;
pub mod approval_manager;
pub mod broker;
pub mod budgets;
pub mod clarifications;
pub mod core_agent_session;
pub mod event_manager;
pub mod failure;
pub mod hierarchy_manager;
pub mod inspection;
pub mod issuance;
pub mod memory_manager;
pub mod observability;
pub mod orphan;
pub mod ownership;
pub mod permission_integrity;
pub mod policy_manager;
pub mod protocol;
pub mod result_manager;
pub mod runtime_manager;
pub mod scheduler;
#[cfg(test)]
mod scheduler_tests;
pub mod working_memory;

pub mod schema;
#[cfg(test)]
mod session_tests;
pub mod skills_bridge;
pub mod state_manager;
pub mod tool_manager;

#[cfg(test)]
mod inspection_tests;

#[cfg(test)]
mod protocol_tests;

pub use athena_events::redact::digest;
pub use athena_persistence::now_rfc3339;
pub use athena_persistence::{NewEvent, WorkspaceLeaseRepository};
/// Task transition tables re-exported for the API's run / cancel handlers; the state
/// type and the lease manager follow the same need.
pub use athena_state::{TaskTransition, TaskTrigger, task_transition};
pub use athena_types::TaskState;
pub use budgets::{BudgetSignal, Budgets, IterationEffect};
pub use core_agent_session::{
    ContextDocument, ControlSurfaceChecksum, Recovery, RestartWindow, StartFailureKind,
    SurfaceMismatch, recovery_decision, start_core_agent, verify_surface,
};
pub use inspection::{Inspection, InspectionFacts, Inspector, Reason, deny_envelope};
pub use issuance::{IssuedPlan, PlanRefusal, PlanSubmission, submit_plan};

pub use broker::{BrokerFault, broker_deliver};
pub use observability::{
    Log, LogLine, LogSink, Logger, MetricFamily, Stall, StdLogSink, WaitEdge, all_families,
    heartbeat_age_since, silence, wait_cycle,
};
pub use ownership::{Entity, ModuleRole, OWNERSHIP};

/// The repository surfaces the API needs, re-exported so `athena-api` needs no direct
/// `athena-persistence` edge (spec §32.2 allows `athena-api → athena-core` only).
pub use athena_persistence::{
    AgentRepository, ApprovalRepository, AthenaStore, ClarificationRepository,
    ConversationRepository, EventRepository, IdempotencyRepository, MemoryRepository, Page,
    PersistenceError, SkillRepository, StoredEvent, TaskRepository,
};

/// Wrap a plain-text failure into the store's error face (the API's fault mapping does
/// not reach the Backend variant directly).
#[must_use]
pub fn persistence_internal(detail: String) -> PersistenceError {
    PersistenceError::Plainly(detail)
}

/// The boot surface the resident binary composes (spec §32.2, revised 2026-10-18): the
/// binary resolves the configuration, opens the store, and drives the Runtime Interface —
/// all through the `athena-api → athena-core` edge. Only the Adapter link
/// (`athena-api → runtimes/athena-runtime-agent-cli`) is declared directly by the binary,
/// as the composition root.
pub use athena_config::{
    AuthMode as ConfigAuthMode, CliInputs as ConfigCliInputs, Config, Loaded, load as load_config,
    redacted_snapshot as config_redacted,
};
pub use athena_persistence::Database;
pub use athena_runtime::Runtime;
pub use athena_runtime::{FilesystemScope, RuntimeError, RuntimeInstanceRef, StartContext};
pub use protocol::{CoreRequest, CoreResponse, ErrorClass, ErrorEnvelope, WritableMemoryTier};
pub use scheduler::{
    BlockingLimit, Inventory, InventoryInstance, QueuedStep, RetryBudget, SchedulerLimits, Tick,
    tick,
};
pub use schema::{
    GateOutcome, ValidationGate, committed_schema_path, core_request_schema,
    core_request_schema_json,
};
pub use working_memory::{Evicted, WorkingItem, WorkingMemory, WorkingWrite, WriteReport};

#[cfg(test)]
#[path = "broker_tests.rs"]
mod broker_tests;
