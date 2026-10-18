//! The Runtime registry (ATH-RT-007, ATH-RT-008).
//!
//! Adapters register themselves at run time; `athena-runtime` never names one, and Core
//! never branches on the kind — it looks a Runtime up by the type its configuration or the
//! selected definition asked for, and everything beyond that query is behind the trait.

use crate::error::{Result, RuntimeError};
use crate::interface::Runtime;
use athena_types::RuntimeType;
use std::collections::HashMap;
use std::sync::Arc;

/// Registered Runtime objects, keyed by their declared kind.
#[derive(Default)]
pub struct RuntimeRegistry {
    runtime_objects: HashMap<RuntimeType, Arc<dyn Runtime>>,
}

impl RuntimeRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a Runtime, refusing a duplicate kind.
    ///
    /// Two Adapters for one kind would make the registry silently choose between them —
    /// a Core decision that belongs to configuration. Registration happens at boot
    /// (ATH-RT-008), so a duplicated kind is a wiring mistake, surfaced here.
    ///
    /// # Errors
    /// [`RuntimeError::Internal`] when the kind is already held.
    pub fn register(&mut self, runtime: Arc<dyn Runtime>) -> Result<()> {
        let kind = runtime.runtime_type();
        if self.runtime_objects.contains_key(&kind) {
            return Err(RuntimeError::Internal(format!(
                "runtime kind {kind} is already registered; refusing to shadow"
            )));
        }
        self.runtime_objects.insert(kind, runtime);
        Ok(())
    }

    /// The Runtime registered for a kind.
    #[must_use]
    pub fn get(&self, kind: &RuntimeType) -> Option<Arc<dyn Runtime>> {
        self.runtime_objects.get(kind).cloned()
    }

    /// The Runtime registered for a kind.
    ///
    /// # Errors
    /// [`RuntimeError::Unsupported`] when no Runtime of this kind is registered — the fact
    /// a caller learns from instead of a Core-side fallback (ATH-RT-007).
    pub fn require(&self, kind: &RuntimeType) -> Result<Arc<dyn Runtime>> {
        self.get(kind)
            .ok_or_else(|| RuntimeError::Unsupported(format!("no runtime registered for {kind}")))
    }

    /// Every registered kind.
    #[must_use]
    pub fn registered(&self) -> Vec<RuntimeType> {
        let mut kinds: Vec<_> = self.runtime_objects.keys().copied().collect();
        kinds.sort();
        kinds
    }

    /// How many Runtimes are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.runtime_objects.len()
    }

    /// Whether the registry holds no Runtime.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.runtime_objects.is_empty()
    }
}

impl std::fmt::Debug for RuntimeRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeRegistry")
            .field(
                "registered",
                &self.runtime_objects.keys().collect::<Vec<_>>(),
            )
            .finish()
    }
}
