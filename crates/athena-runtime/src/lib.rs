//! The Runtime Interface (spec section 14.2), defined **before any Adapter**
//! (ATH-RT-003, ATH-DEV-002).
//!
//! Core operates every agent only through the seven operations of this trait —
//! `start` / `send` / `receive` / `status` / `stop` / `collect_result` / `capabilities`
//! (ATH-RT-001, ATH-RT-002) — and must hold no `match` on the Runtime kind
//! (ATH-RT-007). Adapters implement this trait and register into [`registry::RuntimeRegistry`]
//! at run time; adding a Runtime neither changes a caller (ATH-RT-008) nor introduces an
//! edge `athena-runtime` → any Adapter crate (spec section 32.2).
//!
//! `StartContext` (design section 5.1) carries everything the Runtime needs and nothing it
//! must decide: workspace and work targets are given, permission sets are given, deadlines
//! are given. Selection, re-planning and risk judgement stay in Core Host
//! (ATH-ACLI-006, ATH-CORE-020).
//!
//! **boundary message [design note]** The `Message` type defined in module `message` is the
//! Runtime-boundary view of the spec §10.2 envelope. The full envelope belongs to
//! `athena-messaging` (Phase 2); `athena-runtime` depends on `athena-types` only, so the
//! boundary type is declared here and the Broker's envelope maps onto it at delivery
//! (design section 6.1: `deliver via Runtime::send`).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod context;
pub mod contract;
pub mod error;
pub mod interface;
pub mod message;
pub mod registry;
pub mod status;
pub mod stub;

pub use context::{FilesystemScope, StartContext};
pub use contract::{
    ContractFailure, assert_contract, contract_context_at, contract_definition, run_contract,
    run_contract_at_workspace,
};

/// The `aiprj` revision ATHENA ships (the vendored expansion, P3-08: aquaxis/aiprj 1.1.0,
/// MIT). Declared at the Interface layer so Core (`WorkspaceInitialized` payloads) and
/// Adapters read one constant without an Adapter edge.
pub const AIPRJ_VERSION: &str = "1.1.0";
pub use error::{Result, RuntimeError};
pub use interface::{Runtime, RuntimeInstanceRef};
pub use message::Message;
pub use registry::RuntimeRegistry;
pub use status::{IsolationBoundary, RuntimeCapabilities, RuntimeInstanceStatus};
pub use stub::{StubClock, StubConfig, StubReaction, StubResult, StubRuntime, StubScript};

#[cfg(test)]
mod interface_tests;

#[cfg(test)]
mod stub_tests;

#[cfg(test)]
#[path = "surface_tests.rs"]
mod surface_tests;
