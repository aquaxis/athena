//! What the Runtime declares and how it reports instance health
//! (ATH-RT-005, ATH-SEC-015).

use athena_types::{RuntimeType, ToolName};
use serde::{Deserialize, Serialize};

/// The isolation boundary the Runtime actually enforces for an instance
/// (ATH-RT-005, ATH-SEC-015).
///
/// Declared, not inferred: `capabilities()` states what the Runtime provides, and Core Host
/// may rely on the declaration — an agent on externally sourced skills, for example, SHOULD
/// run where the boundary is stronger than a bare process (R-SEC-11). A declaration this
/// Runtime does not enforce is a specification violation, not a trade-off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationBoundary {
    /// A plain OS process on the same host, as `agent-cli` provides.
    Process,
    /// A container boundary.
    Container,
    /// A remote boundary — another host or machine.
    Remote,
}

impl IsolationBoundary {
    /// Whether this boundary is at least as strong as `other`.
    ///
    /// `Process ⊂ Container ⊂ Remote`: `stronger_than(process)` is true for container and
    /// remote, so callers reason about upgrade requirements instead of enum ordering
    /// (R-SEC-11).
    #[must_use]
    pub fn at_least_as_strong_as(self, other: Self) -> bool {
        fn rank(value: IsolationBoundary) -> u8 {
            match value {
                IsolationBoundary::Process => 0,
                IsolationBoundary::Container => 1,
                IsolationBoundary::Remote => 2,
            }
        }
        rank(self) >= rank(other)
    }
}

/// The observed status of one Runtime Instance.
///
/// This is the Runtime's process-level observation. Mapping it into the §17.2 Agent
/// Instance state machine is the Runtime Manager's job (design section 3.3) — the Runtime
/// holds no state machine of its own (ATH-CORE-020).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeInstanceStatus {
    /// `start` has not completed yet (ATH-RT-006 startup deadline applies).
    Starting,
    /// The process is alive and reachable.
    Running,
    /// The process reported an outstanding wait (approval, clarification, message reply).
    Waiting,
    /// The process was suspended with a frozen snapshot (ATH-STATE-021).
    Suspended,
    /// The process ended; `exit_code` is what the OS reported.
    Stopped {
        /// The exit code, when known.
        exit_code: Option<i32>,
    },
    /// The process broke its supervision contract: abnormal exit, transport loss
    /// (ATH-ACLI-005).
    Failed {
        /// What went wrong.
        detail: String,
    },
}

impl RuntimeInstanceStatus {
    /// Whether the instance is live — a status the Core may route messages to.
    #[must_use]
    pub fn is_alive(&self) -> bool {
        matches!(
            self,
            Self::Running | Self::Waiting | Self::Starting | Self::Suspended
        )
    }
}

/// What a Runtime offers (ATH-RT-005).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeCapabilities {
    /// The kind this Runtime implements.
    pub runtime_type: RuntimeType,
    /// The isolation boundary it actually enforces.
    pub isolation: IsolationBoundary,
    /// The tools the transported agent can reach through this Runtime.
    pub supported_tools: Vec<ToolName>,
    /// How many instances this Runtime can hold concurrently (ATH-RT-009).
    pub max_concurrent_instances: u64,
}
