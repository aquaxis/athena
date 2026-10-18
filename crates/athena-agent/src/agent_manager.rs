//! Definition registration and validation (spec §8.1, ATH-AGENT-001/008, R-AGT-01).
//!
//! Two validations carry the *no default* rule: registration and start both refuse
//! definitions whose `model.provider` / `model.name` are missing (ATH-AGENT-008), and the
//! dynamic-generation path (ATH-HIER-018) may only recombine *existing* personas,
//! capabilities, and permission subsets.

use athena_types::AgentDefinition;
use thiserror::Error;

/// The permission label the dynamic-generation ceiling checks against (the agents' own
/// `ToolName`'s wire form, spelled locally to avoid a config-shaped dependency).
pub type ToolLabel = String;

/// Why a definition or instance was refused (pre-registration).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Refusal {
    /// The definition lacks model coordinates (ATH-AGENT-008: both must be stated; ATHENA
    /// has no default provider or model).
    #[error("the agent definition must specify model.provider and model.name")]
    ModelUnspecified,
    /// A dynamic definition named a capability no existing definition carries
    /// (ATH-HIER-018's ceiling).
    #[error("unknown capability {0:?} — dynamic definitions recombine existing ones only")]
    UnknownCapability(String),
    /// A dynamic definition's permission set widened past every known definition
    /// (ATH-HIER-018: subsets only).
    #[error("tool {0:?} is not granted to any existing agent definition")]
    UnknownPermission(ToolLabel),
    /// The dynamic definition's persona does not exist (R-PER-05 / ATH-HIER-018).
    #[error("persona {0:?} is unknown")]
    UnknownPersona(String),
}

impl Refusal {
    /// The §20.1 class; registration refusals are input problems.
    #[must_use]
    pub const fn class(&self) -> &'static str {
        "ValidationError"
    }

    /// A stable machine code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ModelUnspecified => "AGENT_MODEL_UNSPECIFIED",
            Self::UnknownCapability(_) => "AGENT_UNKNOWN_CAPABILITY",
            Self::UnknownPermission(_) => "AGENT_UNKNOWN_PERMISSION",
            Self::UnknownPersona(_) => "AGENT_UNKNOWN_PERSONA",
        }
    }
}

/// Validate a *static* definition at registration (and again at start — same check,
/// ATH-AGENT-008).
///
/// # Errors
/// [`Refusal::ModelUnspecified`] when the definition carries no provider/model.
pub fn validate_definition(definition: &AgentDefinition) -> Result<(), Refusal> {
    if definition.model.provider.trim().is_empty() || definition.model.name.trim().is_empty() {
        return Err(Refusal::ModelUnspecified);
    }
    Ok(())
}

/// The facts the dynamic-generation ceiling checks against
/// (ATH-HIER-018: the existing definitions are the ceiling).
#[derive(Debug, Clone, Default)]
pub struct KnownUniverse {
    /// Every persona the existing definitions carry.
    pub personas: Vec<String>,
    /// Every capability at least one definition carries.
    pub capabilities: Vec<String>,
    /// Every tool at least one definition grants.
    pub tools: Vec<ToolLabel>,
}

impl KnownUniverse {
    /// Collect the universe from the live definitions.
    #[must_use]
    pub fn from_definitions(definitions: &[AgentDefinition]) -> Self {
        let mut personas = Vec::new();
        let mut capabilities = Vec::new();
        let mut tools = Vec::new();
        for definition in definitions {
            if !personas.contains(&definition.persona_ref) {
                personas.push(definition.persona_ref.clone());
            }
            for capability in &definition.capabilities {
                if !capabilities.contains(capability) {
                    capabilities.push(capability.clone());
                }
            }
            for tool in &definition.allowed_tools {
                let label = tool.as_str().to_owned();
                if !tools.contains(&label) {
                    tools.push(label);
                }
            }
        }
        Self {
            personas,
            capabilities,
            tools,
        }
    }
}

/// Validate a *dynamically generated* definition (ATH-HIER-018: combinations of existing
/// personas, capabilities, and permission subsets only — no persona prose, no new
/// permissions).
///
/// `universe` is read from the live definitions at generation time; a widening attempt
/// refuses with the specific field.
///
/// # Errors
/// [`Refusal::UnknownPersona`], [`Refusal::UnknownCapability`], or
/// [`Refusal::UnknownPermission`] — each cites the offending value.
pub fn validate_dynamic(
    definition: &AgentDefinition,
    universe: &KnownUniverse,
) -> Result<(), Refusal> {
    validate_definition(definition)?;
    if !universe.personas.contains(&definition.persona_ref) {
        return Err(Refusal::UnknownPersona(definition.persona_ref.clone()));
    }
    for capability in &definition.capabilities {
        if !universe.capabilities.contains(capability) {
            return Err(Refusal::UnknownCapability(capability.clone()));
        }
    }
    for tool in &definition.allowed_tools {
        let label = tool.as_str().to_owned();
        if !universe.tools.contains(&label) {
            return Err(Refusal::UnknownPermission(label));
        }
    }
    Ok(())
}
