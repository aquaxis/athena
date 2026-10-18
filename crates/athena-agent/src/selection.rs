//! Capability-matching agent selection (ATH-AGENT-004/005/007, P2-08).
//!
//! The Candidate list, the capability containment test, and the refusal rule (never
//! silently assign a mismatched agent — re-decompose or escalate). The scorer records the
//! *rationale* per candidate; Core Agent's judgement picks, Core Host records why.

use athena_types::AgentDefinition;
/// One selection candidate and its match report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The agent.
    pub agent_id: athena_types::AgentId,
    /// Its capabilities.
    pub capabilities: Vec<String>,
    /// Whether every required capability is present.
    pub matched: bool,
    /// The capabilities the Task demanded that this agent lacks.
    pub missing: Vec<String>,
}

/// Select the candidates for a Task's required capability set.
///
/// Every candidate is reported — the `AgentSelected` Event's `candidates` list carries
/// all of them with the rationale, and the *matched* ones are the pool Core Agent chooses
/// among (R-AGT-16: agents whose capabilities contain the Task's requirements).
///
/// # Errors
/// [`NoMatch`] when no candidate carries every required capability — never a silent
/// assignment; the caller (Core Agent) must re-decompose or escalate (ATH-AGENT-005).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NoMatch {
    /// No candidate carried the full requirement set.
    #[error("no candidate matches the required capabilities {0:?}")]
    #[allow(clippy::exhaustive_enums, reason = "the refusal is exhaustive in v1")]
    Refused(Vec<String>),
}

/// The selection result: matched candidates with per-candidate rationales (each carries
/// the `why` text the Event cites).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// Every candidate evaluated, in evaluation order (the roster passed in).
    pub evaluated: Vec<Candidate>,
    /// The matched pool.
    pub matched: Vec<athena_types::AgentId>,
    /// The per-candidate rationale text (cited in `AgentSelected`).
    pub rationale: Vec<(athena_types::AgentId, String)>,
}

/// Evaluate the roster against the Task's required capability set.
#[must_use = "a refusal here demands re-decomposition or escalation (ATH-AGENT-005)"]
pub fn select(required: &[String], roster: &[AgentDefinition]) -> Result<Selection, NoMatch> {
    let mut evaluated = Vec::with_capacity(roster.len());
    let mut matched = Vec::new();
    let mut rationale = Vec::new();
    for definition in roster {
        let missing: Vec<String> = required
            .iter()
            .filter(|need| !definition.capabilities.contains(need))
            .cloned()
            .collect();
        let full = missing.is_empty();
        let why = if full {
            format!(
                "every required capability is present: {}",
                required.join(", ")
            )
        } else {
            format!("missing capabilities: {}", missing.join(", "))
        };
        evaluated.push(Candidate {
            agent_id: definition.id.clone(),
            capabilities: definition.capabilities.clone(),
            matched: full,
            missing,
        });
        rationale.push((definition.id.clone(), why));
        if full {
            matched.push(definition.id.clone());
        }
    }
    if matched.is_empty() {
        return Err(NoMatch::Refused(required.to_vec()));
    }
    Ok(Selection {
        evaluated,
        matched,
        rationale,
    })
}
