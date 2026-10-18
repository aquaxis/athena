//! Enumerated value domains (spec section 26.1).
//!
//! State sets are declared here; the permitted transitions between them live in `athena-state`
//! so that the transition tables have a single home (ATH-STATE-001, ATH-STATE-002).

use serde::{Deserialize, Serialize};

/// Declare a simple enum with snake_case serialization and a `TERMINAL`/`ALL` surface.
macro_rules! simple_enum {
    (
        $(#[$meta:meta])*
        $name:ident { $( $(#[$vmeta:meta])* $variant:ident => $wire:literal ),+ $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
            schemars::JsonSchema,
        )]
        pub enum $name {
            $(
                $(#[$vmeta])*
                // Variants carry their wire form as documentation so the public surface stays
                // documented without restating each value in prose.
                #[doc = concat!("Serialized as `", $wire, "`.")]
                #[serde(rename = $wire)]
                $variant
            ),+
        }

        impl $name {
            /// Every variant, for exhaustive table-driven tests (ATH-TEST-013).
            pub const ALL: &'static [Self] = &[ $( Self::$variant ),+ ];

            /// The wire representation.
            #[must_use]
            pub fn as_str(&self) -> &'static str {
                match self { $( Self::$variant => $wire ),+ }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

simple_enum!(
    /// Runtime kinds (spec section 26.1). Core must not branch on this value (ATH-RT-007).
    RuntimeType {
        AgentCli => "agent-cli",
        LocalProcess => "local-process",
        Docker => "docker",
        Ssh => "ssh",
        Kubernetes => "kubernetes",
        Cloud => "cloud",
    }
);

simple_enum!(
    /// Risk classification. Decided by Core Host from configuration, never from agent prose
    /// (ATH-HITL-002, ATH-CORE-038).
    RiskLevel {
        Low => "low",
        Medium => "medium",
        High => "high",
    }
);

simple_enum!(
    /// Scheduling priority (spec section 18.3).
    Priority {
        Low => "low",
        Normal => "normal",
        High => "high",
    }
);

simple_enum!(
    /// ATHENA's tool vocabulary (spec section 11.1).
    ///
    /// These are ATHENA's names. `agent-cli` uses a different vocabulary, so the Adapter owns an
    /// explicit projection (design section 5.3.1); this enum must not drift toward the
    /// substrate's names.
    ToolName {
        FsRead => "fs_read",
        FsWrite => "fs_write",
        Shell => "shell",
        Git => "git",
        Search => "search",
        SendTo => "send_to",
        Process => "process",
        Terminal => "terminal",
        Network => "network",
    }
);

impl ToolName {
    /// Default risk of the tool (spec section 11.1).
    ///
    /// This is the tool's baseline only. The operative classification for a concrete invocation
    /// comes from `policy.rules`, where the first matching rule wins and an unmatched operation
    /// is `High` (ATH-CFG-009).
    #[must_use]
    pub fn default_risk(&self) -> RiskLevel {
        match self {
            Self::FsRead | Self::Search | Self::SendTo => RiskLevel::Low,
            Self::FsWrite | Self::Git => RiskLevel::Medium,
            Self::Shell | Self::Process | Self::Terminal | Self::Network => RiskLevel::High,
        }
    }
}

simple_enum!(
    /// Message kinds (spec section 10.1).
    MessageType {
        Instruction => "instruction",
        Report => "report",
        Question => "question",
        Answer => "answer",
        Status => "status",
        Error => "error",
        Complete => "complete",
        ApprovalRequest => "approval_request",
        ControlRequest => "control_request",
        ControlResponse => "control_response",
        Cancel => "cancel",
        Interrupt => "interrupt",
    }
);

impl MessageType {
    /// Whether the Broker may never drop or refuse this message (ATH-MSG-012).
    ///
    /// Losing a completed agent's report is unrecoverable, so back-pressure must not apply to
    /// these kinds.
    #[must_use]
    pub fn is_undroppable(&self) -> bool {
        matches!(
            self,
            Self::Report
                | Self::Complete
                | Self::Error
                | Self::Cancel
                | Self::Interrupt
                | Self::ControlResponse
        )
    }

    /// Whether the Broker may answer `TransientError` on queue overflow (ATH-MSG-009).
    #[must_use]
    pub fn is_back_pressurable(&self) -> bool {
        matches!(self, Self::Instruction | Self::Status | Self::Question)
    }

    /// Whether this is a control message delivered ahead of queued traffic (ATH-MSG-017).
    #[must_use]
    pub fn is_control(&self) -> bool {
        matches!(self, Self::Cancel | Self::Interrupt)
    }
}

simple_enum!(
    /// Task states (spec section 17.1).
    TaskState {
        Created => "CREATED",
        Queued => "QUEUED",
        Running => "RUNNING",
        WaitingApproval => "WAITING_APPROVAL",
        WaitingInput => "WAITING_INPUT",
        Evaluating => "EVALUATING",
        Replanning => "REPLANNING",
        Retrying => "RETRYING",
        Cancelling => "CANCELLING",
        Success => "SUCCESS",
        Failed => "FAILED",
        Cancelled => "CANCELLED",
    }
);

impl TaskState {
    /// Terminal states (spec section 17.1).
    pub const TERMINAL: &'static [Self] = &[Self::Success, Self::Failed, Self::Cancelled];

    /// Whether the state is terminal.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        Self::TERMINAL.contains(self)
    }

    /// Aggregation precedence when several PlanSteps run concurrently (spec section 17.1).
    ///
    /// Lower value wins. Task state is derived from its PlanSteps, so a deterministic
    /// precedence is what makes the derivation single-valued.
    #[must_use]
    pub fn aggregation_rank(&self) -> u8 {
        match self {
            Self::Cancelling => 0,
            Self::WaitingApproval => 1,
            Self::WaitingInput => 2,
            Self::Retrying => 3,
            Self::Replanning => 4,
            Self::Evaluating => 5,
            Self::Running => 6,
            Self::Queued => 7,
            // Not part of the precedence chain; ranked last so comparisons stay total.
            Self::Created | Self::Success | Self::Failed | Self::Cancelled => 8,
        }
    }
}

simple_enum!(
    /// PlanStep states (spec section 17.5).
    PlanStepState {
        Pending => "PENDING",
        Ready => "READY",
        Running => "RUNNING",
        Suspended => "SUSPENDED",
        Orphaned => "ORPHANED",
        Success => "SUCCESS",
        Failed => "FAILED",
        Skipped => "SKIPPED",
    }
);

impl PlanStepState {
    /// Terminal states (spec section 17.5).
    pub const TERMINAL: &'static [Self] = &[Self::Success, Self::Failed, Self::Skipped];

    /// Whether the state is terminal.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        Self::TERMINAL.contains(self)
    }

    /// Whether re-planning may delete a step in this state (ATH-STATE-028).
    ///
    /// A `RUNNING` step must go through cancellation instead, because its side effects are
    /// already in flight.
    #[must_use]
    pub fn is_deletable_by_replanning(&self) -> bool {
        matches!(self, Self::Pending | Self::Ready)
    }
}

simple_enum!(
    /// Agent Instance states (spec section 17.2).
    AgentInstanceState {
        Created => "CREATED",
        Starting => "STARTING",
        Idle => "IDLE",
        Busy => "BUSY",
        Waiting => "WAITING",
        Suspended => "SUSPENDED",
        Stopping => "STOPPING",
        Stopped => "STOPPED",
        Failed => "FAILED",
    }
);

impl AgentInstanceState {
    /// Whether the state is terminal. `STOPPED` alone is terminal; `FAILED` is quasi-terminal
    /// and still transitions to `STOPPING` for resource reclamation (spec section 17.2,
    /// ATH-STATE-007).
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Stopped)
    }

    /// Whether the instance is excluded from concurrency accounting (ATH-SCHED-010,
    /// ATH-STATE-020).
    ///
    /// Excluding waiters is what prevents a superior holding a slot its own subordinates need —
    /// the hold-and-wait that would otherwise deadlock a 1:N hierarchy at default limits.
    #[must_use]
    pub fn is_excluded_from_limits(&self) -> bool {
        matches!(self, Self::Waiting | Self::Suspended)
    }
}

simple_enum!(
    /// Skill states, tracked per `(id, version)` (spec section 17.3, ATH-STATE-026).
    SkillState {
        Discovered => "DISCOVERED",
        Validating => "VALIDATING",
        Available => "AVAILABLE",
        Enabled => "ENABLED",
        Disabled => "DISABLED",
        Updating => "UPDATING",
        Failed => "FAILED",
        Removed => "REMOVED",
    }
);

impl SkillState {
    /// Whether Core Agent may select a skill in this state (ATH-STATE-008).
    #[must_use]
    pub fn is_selectable(&self) -> bool {
        matches!(self, Self::Enabled)
    }

    /// Whether the state is terminal.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Removed)
    }
}

simple_enum!(
    /// Approval states (spec section 17.4).
    ApprovalState {
        Requested => "REQUESTED",
        Granted => "GRANTED",
        Rejected => "REJECTED",
        Expired => "EXPIRED",
        Consumed => "CONSUMED",
    }
);

impl ApprovalState {
    /// Terminal states (spec section 17.4).
    pub const TERMINAL: &'static [Self] = &[Self::Rejected, Self::Expired, Self::Consumed];

    /// Whether the state is terminal.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        Self::TERMINAL.contains(self)
    }

    /// Whether the outcome denies the operation. Expiry is equivalent to rejection
    /// (ATH-HITL-005, ATH-STATE-011) — silence is never consent.
    #[must_use]
    pub fn denies(&self) -> bool {
        matches!(self, Self::Rejected | Self::Expired)
    }
}

simple_enum!(
    /// Hierarchy levels (spec section 8.2).
    AgentLevel {
        Producer => "producer",
        Director => "director",
        Lead => "lead",
        Worker => "worker",
    }
);

impl AgentLevel {
    /// The level immediately below, or `None` for `Worker`.
    ///
    /// Delegation targets the immediate child level only (ATH-HIER-004).
    #[must_use]
    pub fn subordinate(&self) -> Option<Self> {
        match self {
            Self::Producer => Some(Self::Director),
            Self::Director => Some(Self::Lead),
            Self::Lead => Some(Self::Worker),
            Self::Worker => None,
        }
    }

    /// Depth from `Producer`, used for the delegation-depth check (ATH-HIER-005).
    #[must_use]
    pub fn depth(&self) -> u64 {
        match self {
            Self::Producer => 0,
            Self::Director => 1,
            Self::Lead => 2,
            Self::Worker => 3,
        }
    }
}

simple_enum!(
    /// Runtime isolation boundaries (spec section 29.5). Declared by `capabilities()` and
    /// actually enforced (ATH-RT-005, ATH-SEC-015).
    Isolation {
        Process => "process",
        Container => "container",
        RemoteHost => "remote-host",
        Pod => "pod",
    }
);

simple_enum!(
    /// Artifact kinds (spec section 26.1).
    ArtifactKind {
        File => "file",
        Patch => "patch",
        Report => "report",
        Log => "log",
        Data => "data",
    }
);

simple_enum!(
    /// Artifact path bases (spec section 19.1, ATH-RESULT-002).
    ///
    /// Only these two exist. A Workspace-relative base is absent by design: Instance areas are
    /// deleted at retention expiry (ATH-AIPRJ-029), so such a reference would dangle.
    ArtifactBase {
        WorkTarget => "work_target",
        Store => "store",
    }
);

simple_enum!(
    /// Memory tiers (spec section 13.1).
    MemoryTier {
        Short => "short",
        Working => "working",
        Long => "long",
    }
);

impl MemoryTier {
    /// Whether Core Agent may write this tier via `WriteMemory` (spec section 7.5.5).
    ///
    /// Short-term memory lives in the Agent Process, so ATHENA does not accept writes to it
    /// (ATH-MEM-010).
    #[must_use]
    pub fn is_writable_by_core_agent(&self) -> bool {
        matches!(self, Self::Working | Self::Long)
    }
}

simple_enum!(
    /// Whether an interrupted operation's side effects are known (spec section 26.1).
    ///
    /// `Unknown` is what forces Verification before a retry (ATH-CORE-048, ATH-STATE-029).
    SideEffects {
        Applied => "applied",
        None => "none",
        Unknown => "unknown",
    }
);

simple_enum!(
    /// Clarification states (spec section 15.3).
    ClarificationState {
        Requested => "requested",
        Answered => "answered",
        Expired => "expired",
    }
);

simple_enum!(
    /// Tool execution outcomes (spec section 26.6).
    ToolExecutionStatus {
        Started => "started",
        Succeeded => "succeeded",
        Failed => "failed",
        Denied => "denied",
    }
);

simple_enum!(
    /// Evaluation verdicts (spec section 19.2, ATH-EVAL-004).
    Verdict {
        Accept => "accept",
        Retry => "retry",
        Replan => "replan",
        Fail => "fail",
        Escalate => "escalate",
    }
);

simple_enum!(
    /// Who produced an utterance (spec section 26.6).
    UtteranceRole {
        Human => "human",
        CoreAgent => "core-agent",
    }
);

simple_enum!(
    /// Utterance kinds (spec section 26.6).
    UtteranceKind {
        Message => "message",
        Question => "question",
        Answer => "answer",
        Summary => "summary",
    }
);

simple_enum!(
    /// Result status reported by an agent (spec section 19.1).
    ///
    /// Self-declared success is never sufficient for acceptance (ATH-EVAL-005).
    ResultStatus {
        Success => "success",
        Failure => "failure",
        Partial => "partial",
    }
);

simple_enum!(
    /// Who caused an Event (spec section 23.1).
    Actor {
        Human => "human",
        CoreAgent => "core-agent",
        Agent => "agent",
        Scheduler => "scheduler",
        Runtime => "runtime",
        System => "system",
    }
);

simple_enum!(
    /// Reasons an instance is stopped (spec section 26.1).
    StopReason {
        Completed => "completed",
        Cancelled => "cancelled",
        Timeout => "timeout",
        Failed => "failed",
        Shutdown => "shutdown",
        Suspend => "suspend",
    }
);

simple_enum!(
    /// Filesystem access modes (spec section 26.1).
    FsMode {
        Read => "read",
        Write => "write",
    }
);

simple_enum!(
    /// Network access modes (spec section 26.1).
    NetworkMode {
        Allow => "allow",
        Deny => "deny",
        Allowlist => "allowlist",
    }
);

simple_enum!(
    /// API roles (spec section 29.2).
    Role {
        Viewer => "viewer",
        Operator => "operator",
        Admin => "admin",
    }
);

impl Role {
    /// Whether this role satisfies a requirement for `needed`.
    ///
    /// Roles are cumulative: `admin` includes `operator`, which includes `viewer`
    /// (spec section 29.2).
    #[must_use]
    pub fn satisfies(&self, needed: Role) -> bool {
        self.rank() >= needed.rank()
    }

    fn rank(&self) -> u8 {
        match self {
            Self::Viewer => 0,
            Self::Operator => 1,
            Self::Admin => 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_terminal_states_match_the_spec() {
        assert!(TaskState::Success.is_terminal());
        assert!(TaskState::Failed.is_terminal());
        assert!(TaskState::Cancelled.is_terminal());
        assert!(!TaskState::Running.is_terminal());
        assert!(!TaskState::Evaluating.is_terminal());
        assert_eq!(TaskState::ALL.len(), 12);
    }

    #[test]
    fn task_aggregation_precedence_follows_the_spec_order() {
        // CANCELLING > WAITING_APPROVAL > WAITING_INPUT > RETRYING > REPLANNING
        //            > EVALUATING > RUNNING > QUEUED   (spec section 17.1)
        let order = [
            TaskState::Cancelling,
            TaskState::WaitingApproval,
            TaskState::WaitingInput,
            TaskState::Retrying,
            TaskState::Replanning,
            TaskState::Evaluating,
            TaskState::Running,
            TaskState::Queued,
        ];
        for pair in order.windows(2) {
            assert!(
                pair[0].aggregation_rank() < pair[1].aggregation_rank(),
                "{} must outrank {}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn plan_step_sets_match_the_spec() {
        assert_eq!(PlanStepState::ALL.len(), 8);
        assert!(PlanStepState::Success.is_terminal());
        assert!(PlanStepState::Skipped.is_terminal());
        assert!(!PlanStepState::Orphaned.is_terminal());
    }

    #[test]
    fn only_pending_and_ready_steps_are_deletable_by_replanning() {
        // ATH-STATE-028: a RUNNING step must go through cancellation instead.
        for state in PlanStepState::ALL {
            let expected = matches!(state, PlanStepState::Pending | PlanStepState::Ready);
            assert_eq!(state.is_deletable_by_replanning(), expected, "{state}");
        }
    }

    #[test]
    fn only_stopped_is_a_terminal_instance_state() {
        // Spec section 17.2: FAILED is quasi-terminal and still reclaims via STOPPING.
        assert!(AgentInstanceState::Stopped.is_terminal());
        assert!(!AgentInstanceState::Failed.is_terminal());
        assert_eq!(AgentInstanceState::ALL.len(), 9);
    }

    #[test]
    fn waiting_and_suspended_are_excluded_from_limits() {
        // ATH-SCHED-010 / ATH-STATE-020: the hold-and-wait guard.
        for state in AgentInstanceState::ALL {
            let expected = matches!(
                state,
                AgentInstanceState::Waiting | AgentInstanceState::Suspended
            );
            assert_eq!(state.is_excluded_from_limits(), expected, "{state}");
        }
    }

    #[test]
    fn only_enabled_skills_are_selectable() {
        // ATH-STATE-008.
        for state in SkillState::ALL {
            assert_eq!(
                state.is_selectable(),
                *state == SkillState::Enabled,
                "{state}"
            );
        }
    }

    #[test]
    fn expiry_denies_like_rejection() {
        // ATH-HITL-005 / ATH-STATE-011: no response is never consent.
        assert!(ApprovalState::Expired.denies());
        assert!(ApprovalState::Rejected.denies());
        assert!(!ApprovalState::Granted.denies());
        assert!(ApprovalState::Expired.is_terminal());
    }

    #[test]
    fn undroppable_and_back_pressurable_message_sets_are_disjoint() {
        // ATH-MSG-009 vs ATH-MSG-012: a message must not be both refusable and unrefusable.
        for t in MessageType::ALL {
            assert!(
                !(t.is_undroppable() && t.is_back_pressurable()),
                "{t} cannot be both undroppable and back-pressurable"
            );
        }
        assert!(MessageType::Report.is_undroppable());
        assert!(MessageType::Complete.is_undroppable());
        assert!(MessageType::Error.is_undroppable());
        assert!(MessageType::Instruction.is_back_pressurable());
    }

    #[test]
    fn control_messages_are_cancel_and_interrupt() {
        // ATH-MSG-017: these jump the queue.
        assert!(MessageType::Cancel.is_control());
        assert!(MessageType::Interrupt.is_control());
        assert!(!MessageType::ControlResponse.is_control());
    }

    #[test]
    fn delegation_chain_is_producer_director_lead_worker() {
        // ATH-HIER-004: immediate child level only.
        assert_eq!(
            AgentLevel::Producer.subordinate(),
            Some(AgentLevel::Director)
        );
        assert_eq!(AgentLevel::Director.subordinate(), Some(AgentLevel::Lead));
        assert_eq!(AgentLevel::Lead.subordinate(), Some(AgentLevel::Worker));
        assert_eq!(AgentLevel::Worker.subordinate(), None);
        assert_eq!(AgentLevel::Worker.depth(), 3);
    }

    #[test]
    fn default_tool_risk_matches_the_spec_table() {
        // Spec section 11.1.
        assert_eq!(ToolName::FsRead.default_risk(), RiskLevel::Low);
        assert_eq!(ToolName::Search.default_risk(), RiskLevel::Low);
        assert_eq!(ToolName::SendTo.default_risk(), RiskLevel::Low);
        assert_eq!(ToolName::FsWrite.default_risk(), RiskLevel::Medium);
        assert_eq!(ToolName::Git.default_risk(), RiskLevel::Medium);
        assert_eq!(ToolName::Shell.default_risk(), RiskLevel::High);
        assert_eq!(ToolName::Process.default_risk(), RiskLevel::High);
        assert_eq!(ToolName::Terminal.default_risk(), RiskLevel::High);
        assert_eq!(ToolName::Network.default_risk(), RiskLevel::High);
    }

    #[test]
    fn core_agent_cannot_write_short_term_memory() {
        // ATH-MEM-010: short-term memory lives in the Agent Process.
        assert!(!MemoryTier::Short.is_writable_by_core_agent());
        assert!(MemoryTier::Working.is_writable_by_core_agent());
        assert!(MemoryTier::Long.is_writable_by_core_agent());
    }

    #[test]
    fn roles_are_cumulative() {
        // Spec section 29.2.
        assert!(Role::Admin.satisfies(Role::Viewer));
        assert!(Role::Admin.satisfies(Role::Operator));
        assert!(Role::Operator.satisfies(Role::Viewer));
        assert!(!Role::Viewer.satisfies(Role::Operator));
        assert!(!Role::Operator.satisfies(Role::Admin));
    }

    #[test]
    fn artifact_base_has_exactly_two_variants() {
        // ATH-RESULT-002: no Workspace-relative base exists.
        assert_eq!(ArtifactBase::ALL.len(), 2);
    }

    #[test]
    fn enums_round_trip_through_their_wire_form() {
        for state in TaskState::ALL {
            let json = serde_json::to_string(state).expect("serialize");
            assert_eq!(json, format!("\"{}\"", state.as_str()));
            let back: TaskState = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, *state);
        }
        for tool in ToolName::ALL {
            let json = serde_json::to_string(tool).expect("serialize");
            let back: ToolName = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, *tool);
        }
    }
}
