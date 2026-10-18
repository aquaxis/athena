//! The `agent-cli` Runtime Adapter (spec §14.4, Phase 3).
//!
//! This session's slice: the **Adapter skeleton** (P3-01) implementing the seven
//! operations of the Runtime Interface over a pluggable process driver, the
//! **Instance Workspace construction** (P3-05) and **aiprj write-area initialization**
//! (P3-06 — write areas only, never copying the conventions), the **per-Instance
//! `.agent-cli/` generation** (P3-09/P3-11 — only files agent-cli reads, from the
//! `var/agent-cli-template/` origin, never `install.sh`), the **`filesystem_scope`
//! construction** (P3-12), and the **abnormal-exit classification** (P3-14).
//!
//! The real `agent-cli` IPC wiring (P3-15's `spawn` of instance #0, `send`/`ask`
//! handling) is verification-gated (ATH-DEV-022) and lands with the process-manager
//! tasks; this skeleton's process driver covers the contract surface.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod adapter;
pub mod generation;
pub mod instruction;
pub mod manager;
pub mod process;
pub mod workspace;

pub use adapter::{AgentCliAdapter, ProcessDriver, SpawnedProcess, argv_for, command_for};
pub use adapter::{IpcTransport, RealIpc};
pub use generation::{
    GeneratedConfig, GenerationFault, ShortTermRender, TemplateOrigin, build_scope,
    generate_agent_cli,
};
pub use instruction::{EnvelopeFault, Procedure, delegation_envelope, operating_model_section};
pub use manager::{Budgets, Clock, ProcessManager, ReconcileReport, Record, WallClock};
pub use process::TokioDriver;
pub use workspace::{WorkspaceFault, core_agent_workspace, init_workspace, workspace_layout};

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
