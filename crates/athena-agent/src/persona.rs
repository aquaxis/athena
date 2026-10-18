//! The persona loader (P2-18…P2-21; spec §9).
//!
//! Front matter carries `name` / `role` / `level` / `allowed_tools`; the body is prose:
//! *which* prose the Core Agent reads by absolute path under the Instance's Workspace
//! (ATH-AIPRJ-028), and never as a slash command (ATH-ACLI-016). The loader's contract:
//!
//! - Front matter parses (selection: `name`, `role`, `level`, `allowed_tools`,
//!   R-PER-02); the loader refuses personas whose `allowed_tools` exceed the agent
//!   definition's (R-PER-03 / ATH-PERSONA-003).
//! - The persona's *level* must match the definition's (`ATH-PERSONA-003`'s `level`
//!   pairing), which is also what P2-18's patch validation keeps.
//! - Personas are version-controlled files; the execution history records the identifier
//!   and version (R-PER-05 / ATH-PERSONA-005) — this module carries the `PersonaLedger`
//!   helpers the History record cites.

use athena_types::{AgentDefinition, AgentId, AgentLevel, ToolName};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The front matter schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontMatter {
    /// The persona name.
    pub name: String,
    /// The role prose name.
    pub role: String,
    /// The level this persona serves.
    pub level: AgentLevel,
    /// The tools the persona asks for.
    pub allowed_tools: Vec<ToolName>,
}

/// What the loader produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadedPersona {
    /// The validated front matter.
    pub front_matter: FrontMatter,
    /// The body text (verbatim; disallowed prose is a schema condition, the enforcement
    /// is the content-review gate the publication path carries, R-PER-04).
    pub body: String,
    /// The agent identity the persona binds to.
    pub agent_id: AgentId,
}

/// Why a persona was refused (R-PER-03 / R-PER-01's front-matter shape).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PersonaFault {
    /// The file did not carry `---` front-matter fences with a `name` and `level`.
    #[error("the persona's front matter is missing or malformed: {0}")]
    MalformedFrontMatter(String),
    /// The persona's level disagrees with the definition (R-PER-03's level pairing).
    #[error("the persona level {front:?} does not match the definition level {defined:?}")]
    LevelMismatched {
        /// The persona's level.
        front: AgentLevel,
        /// The definition's.
        defined: AgentLevel,
    },
    /// The persona asks for tools the definition does not grant.
    #[error("persona requests tools outside the definition's allowed_tools: {0:?}")]
    PermissionExceeded(Vec<ToolName>),
}

impl PersonaFault {
    /// The §20.1 class (`ValidationError` everywhere here).
    #[must_use]
    pub const fn class(&self) -> &'static str {
        "ValidationError"
    }

    /// A stable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::MalformedFrontMatter(_) => "PERSONA_FRONT_MATTER",
            Self::LevelMismatched { .. } => "PERSONA_LEVEL_MISMATCH",
            Self::PermissionExceeded(_) => "PERSONA_PERMISSION_EXCEEDED",
        }
    }
}

/// Parse the text, split the front matter, verify against the definition's grants
/// (R-PER-01..03).
///
/// # Errors
/// [`PersonaFault`], whole.
pub fn load_persona_from_text(
    text: &str,
    agent_id: &AgentId,
    definition: &AgentDefinition,
) -> Result<LoadedPersona, PersonaFault> {
    let stripped = text.trim_start_matches('\u{feff}');
    let mut fenced = stripped.splitn(3, "---");
    let head = fenced.next().unwrap_or("").trim();
    let _ = head; // empty preamble between file start and the first `---`
    let front_text = fenced
        .next()
        .filter(|section| !section.is_empty())
        .ok_or_else(|| PersonaFault::MalformedFrontMatter("no `---` fences".into()))?;
    let body = fenced.next().unwrap_or("").to_string();

    let front_matter: FrontMatter = serde_yaml::from_str(front_text)
        .map_err(|err| PersonaFault::MalformedFrontMatter(err.to_string()))?;

    // The persona's level pairs the definition's (R-PER-03).
    if front_matter.level != definition.level {
        return Err(PersonaFault::LevelMismatched {
            front: front_matter.level,
            defined: definition.level,
        });
    }

    // The persona's tool asks stay inside the definition's grant (R-PER-03).
    let excessive: Vec<ToolName> = front_matter
        .allowed_tools
        .iter()
        .filter(|tool| !definition.allowed_tools.contains(tool))
        .cloned()
        .collect();
    if !excessive.is_empty() {
        return Err(PersonaFault::PermissionExceeded(excessive));
    }

    Ok(LoadedPersona {
        front_matter,
        body,
        agent_id: agent_id.clone(),
    })
}

/// The front matter teaser callers use for history records (the agent slug and level).
#[must_use]
pub fn front_matter(previous: &LoadedPersona) -> (AgentId, AgentLevel) {
    (previous.agent_id.clone(), previous.front_matter.level)
}

/// The ledger that records the persona identifier/version-history rows (R-PER-05).
#[derive(Debug, Clone, Default)]
pub struct PersonaLedger {
    records: Vec<(AgentId, String, String)>,
}

impl PersonaLedger {
    /// Record one use: agent, persona ref, persona version (R-PER-05).
    pub fn record(&mut self, agent: &AgentId, persona_ref: &str, version: &str) {
        self.records
            .push((agent.clone(), persona_ref.to_string(), version.to_string()));
    }

    /// The uses recorded so far (most recent last — a render-order answer).
    #[must_use]
    pub fn history(&self, agent: &AgentId) -> Vec<(String, String)> {
        self.records
            .iter()
            .filter(|(owner, _, _)| owner == agent)
            .map(|(_, own, version)| (own.clone(), version.clone()))
            .collect()
    }
}
