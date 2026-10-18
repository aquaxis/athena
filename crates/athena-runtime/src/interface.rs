//! The Runtime trait itself (spec §14.2) — the only surface Core uses
//! (ATH-RT-001, ATH-RT-002).

use crate::context::StartContext;
use crate::error::Result;
use crate::message::Message;
use crate::status::{RuntimeCapabilities, RuntimeInstanceStatus};
use athena_types::{AgentDefinition, ExecutionResult, RuntimeType, StopReason};
use futures::Stream;
use futures::stream::BoxStream;

/// The handle to one Runtime-managed execution unit.
///
/// Exactly one Agent Instance maps to one Runtime Instance (ATH-RT-012); the pair of
/// identifiers in the handle is what makes that correspondence explicit. The
/// `RuntimeInstanceId` is the Runtime's, not Core Host's — Core records it against the
/// Agent Instance, never the reverse.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct RuntimeInstanceRef {
    /// The Runtime kind the handle belongs to.
    pub runtime_type: RuntimeType,
    /// The Runtime's own instance identifier (`ri-…`).
    pub runtime_instance_id: athena_types::RuntimeInstanceId,
    /// The Core-issued Agent Instance this executes for.
    pub instance_id: athena_types::InstanceId,
}

/// Start / send / receive / status / stop / collect_result / capabilities
/// (ATH-RT-001).
///
/// Implemented behind `async_trait` so registrations can live as
/// `Arc<dyn Runtime>` objects in the registry (ATH-RT-008, ATH-RT-007).
///
/// **[D-7]** `collect_result` returns an `ExecutionResult` whose `id` is provisional, not
/// the persisted aggregate: identifier issuance stays with Core Host (ATH-CORE-041), so
/// Result Manager replaces the id at receipt. Everything else on the value is the agent's
/// reported work product, accepted on no account before verification (ATH-EVAL-002).
///
/// No method decides anything: no agent or skill selection, no re-planning, no risk
/// classification (ATH-ACLI-006, ATH-CORE-020).
#[async_trait::async_trait]
pub trait Runtime: Send + Sync {
    /// The kind this Runtime implements.
    fn runtime_type(&self) -> RuntimeType;

    /// Start an agent instance carrying the given definition and context.
    ///
    /// The Runtime assigns `RuntimeInstanceId` (its own aggregate, `ri-…`). `ctx` must
    /// satisfy [`StartContext::validate`] — the USER of start validates, the Runtime
    /// re-checks.
    ///
    /// # Errors
    /// [`RuntimeError::Validation`] on an inadmissible start context,
    /// [`RuntimeError::Deadline`] past the startup budget, or
    /// [`RuntimeError::Transient`] when a re-attempt without a fresh Core decision (and
    /// excluded from the restart-window count, ATH-CORE-031) may succeed.
    async fn start(
        &self,
        definition: AgentDefinition,
        ctx: StartContext,
    ) -> Result<RuntimeInstanceRef>;

    /// Deliver a message to the instance.
    ///
    /// Delivery is at-least-once; the Router dedups per `(to_instance_id, message_id)`
    /// (ATH-MSG-003), not this trait.
    ///
    /// # Errors
    /// [`RuntimeError::UnknownInstance`], [`RuntimeError::Transient`] for retryable
    /// transport faults (the Broker owns send retry/backoff, ATH-MSG-015), or
    /// [`RuntimeError::Deadline`] past the message-reply deadline (ATH-MSG-006).
    async fn send(&self, instance: &RuntimeInstanceRef, message: Message) -> Result<()>;

    /// The stream of messages the instance emits.
    ///
    /// At-least-once; subscribers dedup on `message_id`. The returned stream outlives the
    /// borrow; dropping a stream must never stop the process (whose lifecycle
    /// is `stop`'s).
    fn receive(&self, instance: &RuntimeInstanceRef) -> BoxStream<'static, Message>;

    /// Observe the instance's process-level status.
    ///
    /// # Errors
    /// [`RuntimeError::UnknownInstance`] when the handle is stale.
    async fn status(&self, instance: &RuntimeInstanceRef) -> Result<RuntimeInstanceStatus>;

    /// Stop the instance for the reason given, with the configured grace period
    /// (ATH-FAIL-015, design section 5.4).
    ///
    /// `StopReason::Suspend` freezes the instance with a snapshot reference; resumption
    /// uses the identical Skill Set (ATH-STATE-021).
    ///
    /// # Errors
    /// [`RuntimeError::UnknownInstance`] or [`RuntimeError::Deadline`] when the stop grace
    /// period passed and force-termination is needed.
    async fn stop(&self, instance: &RuntimeInstanceRef, reason: StopReason) -> Result<()>;

    /// Ingest the agent's work product.
    ///
    /// **[D-7]** The returned `ExecutionResult` is unbounded by Core-side acceptance: the
    /// persisted `Result` is built by Result Manager, which re-mints the id and records
    /// provenance (ATH-CORE-041).
    ///
    /// # Errors
    /// [`RuntimeError::UnknownInstance`], or [`RuntimeError::Failure`] when the agent
    /// ended without a collectible product (ATH-ACLI-005).
    async fn collect_result(&self, instance: &RuntimeInstanceRef) -> Result<ExecutionResult>;

    /// What this Runtime declares about itself (ATH-RT-005, ATH-SEC-015).
    fn capabilities(&self) -> RuntimeCapabilities;
}

/// Collect a `Stream` as a `BoxStream` in one call site, so Adapter implementations share
/// the same projection shape as the trait's `receive`.
pub fn boxed_stream<S>(stream: S) -> BoxStream<'static, Message>
where
    S: Stream<Item = Message> + Send + 'static,
{
    Box::pin(stream)
}
