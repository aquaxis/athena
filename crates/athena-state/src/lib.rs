//! State Manager transition tables (spec section 17).
//!
//! Core Host's State Manager is the sole writer of state (ATH-STATE-001). Agents, the Core
//! Agent Process, and the Runtime may *request* a transition but never perform one, so this
//! crate exposes resolution functions and no mutation: the caller applies the resolved row
//! inside the transaction that also appends the Event (ATH-PERS-003).
//!
//! Each of the five machines is an exhaustive `match` over `(state, trigger)` returning
//! `Option<Transition>`. Absence means the pairing is not in the table and must be refused as
//! `ValidationError` (ATH-STATE-002). Every resolved row names the Event the transition must
//! emit (ATH-STATE-003) and the actor permitted to request it.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod approval;
pub mod instance;
pub mod plan_step;
pub mod skill;
pub mod task;

pub use approval::{
    ApprovalTransition, ApprovalTrigger, approval_created, approval_transition, has_remaining_uses,
};
pub use instance::{
    InstanceTransition, InstanceTrigger, core_agent_transition_is_permitted, instance_transition,
};
pub use plan_step::{PlanStepTransition, PlanStepTrigger, plan_step_transition};
pub use skill::{SkillTransition, SkillTrigger, skill_transition};
pub use task::{TaskTransition, TaskTrigger, derive_task_state, task_transition};
