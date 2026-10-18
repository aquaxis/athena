//! The precedence enforcement (P5-11; `ATH-SKILL-026` MUST, `ATH-SKILL-027` MUST NOT):
//!
//! ```text
//! ATHENA Security / System Policy   ▸  highest — unreachable from any skill content
//! Agent aiprj Project Rules         ▸  the read-only control surface
//! Skill Instructions                ▸  the placed material under the workspace
//! Task-specific Prompt              ▸  the message channel's body
//! ```
//!
//! Enforcement is **by construction** (design §9.4): policy lives in Core Host and no
//! skill content can derive from, or write into, the security layer; `aiprj` rules
//! arrive through the read-only control surface; skill text is material placed under
//! the **workspace**; the task prompt is a message `body`. A skill therefore cannot
//! nullify the `aiprj` rules — its bytes never reach the control surface, and a skill
//! whose text *claims* to override them is refused before placement becomes effective.
//!
//! This module is the pure face of that order: the sequence the compliance paths emit,
//! the authority each layer owns, and the refusal for a skill text attempting the
//! nullification the spec forbids.

use athena_types::Actor;
use serde::Deserialize;

/// The precedence layers, in binding **descending** order (the first member rules the
/// last).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum Layer {
    /// `athena` security / system policy (Core Host's policy rules).
    SecurityPolicy = 0,
    /// The `aiprj` project rules (the read-only control surface).
    AiprjRules = 1,
    /// A placed skill's instructions (workspace material).
    SkillInstructions = 2,
    /// The task-specific prompt (a message body).
    TaskPrompt = 3,
}

/// Why a layered instruction was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PrecedenceFault {
    /// A lower layer's text claims authority over a higher one (`ATH-SKILL-027`).
    #[error("{claiming:?} attempts to nullify {nullified:?}; the layer's authority refuses it")]
    LayerClaim {
        /// The layer whose text claimed.
        claiming: Layer,
        /// The layer it claimed to nullify.
        nullified: Layer,
    },
}

impl Layer {
    /// The layer's authority face — the rule-binding question.
    #[must_use]
    pub const fn rules_over(&self, other: &Self) -> bool {
        // The enum's discriminant ordering *is* the precedence: a smaller value rules.
        (*self as u8) < (*other as u8)
    }

    /// Which layer may write its own text (`ATH-SKILL-027`'s structural face: skill
    /// instructions arrive from the workspace material, never the control surface).
    #[must_use]
    pub const fn writable_by(&self, requester: &Actor) -> bool {
        match self {
            // The policy is configuration- and admin-owned; Core Host enforces it.
            Self::SecurityPolicy => matches!(requester, Actor::Human | Actor::System),
            // The `aiprj` control surface — read-only to every Inference (the runtime face;
            // Core Host's own writers are the aiprj convention's, not a skill's).
            Self::AiprjRules => matches!(requester, Actor::Human | Actor::System),
            // Skill material is the Loader's placement (wiring), instructed by the Catalog.
            Self::SkillInstructions => matches!(requester, Actor::System),
            // The task prompt rides the message channel's body (Agent-facing).
            Self::TaskPrompt => true,
        }
    }
}

/// A layered instruction: which layers' texts compose, revealed in order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InstructionText {
    /// The layer's text (empty = the layer holds no text).
    pub security_policy: String,
    /// The `aiprj` rules' operative text (as the control surface presented them).
    pub aiprj_rules: String,
    /// A skill's text.
    pub skill_instructions: String,
    /// The task prompt's text.
    pub task_prompt: String,
}

/// Compose the layered instruction: order by the layer precedence, refuse any
/// lower-layer text claiming to rule a higher one (`ATH-SKILL-027`'s refusal face).
///
/// # Errors
/// [`PrecedenceFault::LayerClaim`] naming the claimer and the claimed-over layer.
pub fn compose(text: &InstructionText) -> Result<PrecedenceOrder, PrecedenceFault> {
    let claims = [
        (
            Layer::SkillInstructions,
            Layer::AiprjRules,
            &text.skill_instructions,
        ),
        (Layer::TaskPrompt, Layer::AiprjRules, &text.task_prompt),
        (
            Layer::SkillInstructions,
            Layer::SecurityPolicy,
            &text.skill_instructions,
        ),
        (Layer::TaskPrompt, Layer::SecurityPolicy, &text.task_prompt),
    ];
    for (claiming, nullified, content) in claims {
        if attempts_nullification(content) {
            return Err(PrecedenceFault::LayerClaim {
                claiming,
                nullified,
            });
        }
    }
    Ok(PrecedenceOrder)
}

/// Whether a text *attempts* the nullification a lower layer is structurally denied
/// (`ATH-SKILL-027`). The scan names the attempt; the placement itself is the
/// structural barrier (workspace bytes never reach the control surface).
fn attempts_nullification(text: &str) -> bool {
    let lowered = text.to_lowercase();
    [
        // Claims naming the rules' surface itself.
        "ignore the aiprj",
        "ignore aiprj",
        "disregard the aiprj",
        "supersede the aiprj",
        "override the aiprj",
        "your rules do not apply",
        "the project rules are void",
        "nullified the project rules",
    ]
    .iter()
    .any(|marker| lowered.contains(marker))
}

/// The composed order: a marker-type whose existence finishes the composition (no
/// conflict claimed) — the paths may then assemble the run's instruction stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrecedenceOrder;

impl PrecedenceOrder {
    /// The stack the wired Agent executes with: the layers in binding order.
    #[must_use]
    pub const fn binding_stack() -> [Layer; 4] {
        [
            Layer::SecurityPolicy,
            Layer::AiprjRules,
            Layer::SkillInstructions,
            Layer::TaskPrompt,
        ]
    }
}

#[cfg(test)]
mod precedence_tests;
