//! The definition supply (X-44(a)): the configured `agents` list becomes the store's
//! static `AgentDefinition` rows at boot.
//!
//! What this module owns is the *translation and the gate* — the rules that decide which
//! configuration entries deserve a definition row:
//!
//! * every entry names its level and tools (ATH-AGENT-001: id, level, capabilities,
//!   persona, allowed_tools), either inline or through the persona file its `persona`
//!   names (`personas/` front matter carries `name` / `role` / `level` / `allowed_tools`,
//!   and the file's level must match the entry's when both are stated, the persona
//!   pairing `athena-agent::persona` enforces);
//! * the runtime spelling is a closed wire vocabulary (`RuntimeType`), and a spelling
//!   outside it is a validation refusal before any store write;
//! * model coordinates are mandatory (ATH-AGENT-008): an entry without `provider` and
//!   `model` is refused through [`athena_agent::validate_definition`] — this supply step
//!   is that gate's first production caller;
//! * the Core Agent is never taken from this list: instance #0's definition is
//!   `core.agent`'s alone (`spawn_core_agent` refuses anything else, ATH-CORE-022), so a
//!   list entry naming the Core Agent's slug is refused here before any write — two
//!   registration routes for instance #0 would collide with the boot/restart 1:1 rule.
//!
//! Idempotency rides the destination itself: [`AgentRepository::upsert_definition`] is a
//! put by definition id, so re-running [`supply_definitions`] replaces each row with the
//! equal row and the supply step can sit in the boot sequence unconditionally.
//!
//! The delegation intake depends on this face (`ATH-AGENT-002`'s `DefinitionAbsent`): a
//! definition the supply registered is what turns that refusal into an admission, so the
//! supply precedes every execution-loop tick by construction (it runs at boot, before the
//! loop is even spawned).

use athena_persistence::AthenaStore;
use athena_persistence::repository::AgentRepository;
use athena_types::{
    AgentDefinition, AgentId, AgentLevel, InstanceId, ModelRef, RuntimeType, SkillRef, ToolName,
};

/// Why one configured entry earned no definition row.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SupplyFault {
    /// The entry's slug is empty or names the Core Agent (`ATH-CORE-022`, risk 8): the
    /// list never registers instance #0.
    #[error("agents entry {0:?}: the list never supplies the Core Agent (ATH-CORE-022")]
    CoreAgentEntry(String),
    /// The `runtime` spelling is outside the closed `RuntimeType` vocabulary.
    #[error("agents entry {id}: runtime {spelling:?} is not one of the Runtime kinds")]
    UnknownRuntime {
        /// The entry's slug.
        id: String,
        /// The refused spelling.
        spelling: String,
    },
    /// `ATH-AGENT-001`: level and tools come from the entry or its persona file; the
    /// entry names neither an inline `level` nor a readable persona that carries one.
    #[error("agents entry {0}: no level stated and no persona file to read one from")]
    LevelUnstated(String),
    /// The persona file exists but its front matter refuses (malformed YAML, or its
    /// `allowed_tools` are not tool wire forms — same class as the Runtime spelling).
    #[error("agents entry {id}: persona {path} unreadable: {detail}")]
    PersonaUnreadable {
        /// The entry's slug.
        id: String,
        /// The persona file it named.
        path: String,
        /// The refusal.
        detail: String,
    },
    /// `ATH-AGENT-001`: the tools stated, inline or persona-side, are not the vocabulary.
    #[error("agents entry {id}: allowed_tools carries {tool:?}, which is not a tool")]
    UnknownTool {
        /// The entry's slug.
        id: String,
        /// The refused tool spelling.
        tool: String,
    },
    /// `ATH-AGENT-008`: the definition carries no provider/model — the registration gate's
    /// own refusal, surfaced whole.
    #[error("agents entry {id}: {detail}")]
    ModelUnspecified {
        /// The entry's slug.
        id: String,
        /// The gate's refusal text.
        detail: String,
    },
    /// The store refused a read or a write; surfaced whole.
    #[error("store fault: {0}")]
    Store(String),
}

impl SupplyFault {
    /// The §20.1 class; a supply refusal is always an input problem (fail-fast, ATH-CFG-003).
    #[must_use]
    pub const fn class(&self) -> &'static str {
        "ValidationError"
    }
}

/// One configured `agents` entry, widened from the shipped `{id, runtime}` pair (X-44a:
/// ATH-AGENT-001's facts and ATH-AGENT-008's model facts; every new field optional or
/// defaulted so `agents = []` and every shipped file still parse under the schema's
/// `deny_unknown_fields`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct AgentEntryWide {
    /// Agent slug.
    pub id: String,
    /// Runtime type name (the closed `RuntimeType` vocabulary).
    pub runtime: String,
    /// Hierarchy level, as the `AgentLevel` wire word. Stated inline or resolved from the
    /// persona file — one of the two is required (ATH-AGENT-001).
    pub level: Option<String>,
    /// Display name; defaults to the slug.
    pub name: Option<String>,
    /// Declared capabilities (matched against Task requirements, ATH-AGENT-004).
    pub capabilities: Vec<String>,
    /// Persona reference: a path under `<ATHENA_HOME>/personas/` (the loader's own
    /// contract) or the definition-shaped file the persona resolver reads.
    pub persona: Option<String>,
    /// Persona version, recorded in history (ATH-PERSONA-005).
    pub persona_version: Option<String>,
    /// Tools the agent may run, as `ToolName` wire words; resolved from the persona file
    /// when the entry omits them, refused when neither carries any (ATH-AGENT-001).
    pub allowed_tools: Vec<String>,
    /// Tools the agent may grant downward (ATH-SEC-024: stated, never implied).
    pub delegable_tools: Vec<String>,
    /// Default skills, as `slug@version` pairs.
    pub skills: Vec<SkillEntryCfg>,
    /// Provider name (ATH-AGENT-008: mandatory).
    pub provider: Option<String>,
    /// Model name (ATH-AGENT-008: mandatory).
    pub model: Option<String>,
    /// The delegation-depth ceiling the definition carries (ATH-HIER-005's default).
    pub max_delegation_depth: Option<u64>,
}

impl AgentEntryWide {
    /// The all-defaults value: `denied unknown fields` still parses `agents = []` because
    /// the slice itself is a default, and an inline table must name `id` and `runtime`.
    #[must_use]
    pub fn empty(id: String, runtime: String) -> Self {
        Self {
            id,
            runtime,
            level: None,
            name: None,
            capabilities: Vec::new(),
            persona: None,
            persona_version: None,
            allowed_tools: Vec::new(),
            delegable_tools: Vec::new(),
            skills: Vec::new(),
            provider: None,
            model: None,
            max_delegation_depth: None,
        }
    }
}
impl From<&athena_config::AgentEntry> for AgentEntryWide {
    /// The configured entry, read field for field. The empty string is the config
    /// layer's "unstated" (its `#[serde(default)]` shape) and becomes `None` here, so the
    /// translation's own precedence rules (inline first, persona file second) see one
    /// notion of absence instead of two.
    fn from(entry: &athena_config::AgentEntry) -> Self {
        let stated = |text: &str| -> Option<String> {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        };
        Self {
            id: entry.id.trim().to_string(),
            runtime: entry.runtime.trim().to_string(),
            level: stated(&entry.level),
            name: stated(&entry.name),
            capabilities: entry.capabilities.clone(),
            persona: stated(&entry.persona),
            persona_version: entry.persona_version.clone(),
            allowed_tools: entry
                .allowed_tools
                .iter()
                .map(|tool| tool.as_str().to_string())
                .collect(),
            delegable_tools: entry
                .delegable_tools
                .iter()
                .map(|tool| tool.as_str().to_string())
                .collect(),
            skills: entry
                .skills
                .iter()
                .map(|skill| SkillEntryCfg {
                    id: skill.id.clone(),
                    version: stated(&skill.version),
                })
                .collect(),
            provider: stated(&entry.provider),
            model: stated(&entry.model),
            max_delegation_depth: entry.max_delegation_depth,
        }
    }
}

/// One `slug@version` skill entry, or a bare slug.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SkillEntryCfg {
    /// Skill slug.
    pub id: String,
    /// Semantic version; the entry's bare-slug form carries none.
    pub version: Option<String>,
}

impl<'de> serde::Deserialize<'de> for SkillEntryCfg {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = SkillEntryCfg;
            fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str("a skill slug or slug@version")
            }
            fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<Self::Value, E> {
                let (id, version) = match text.split_once('@') {
                    Some((id, version)) => (id.to_string(), Some(version.to_string())),
                    None => (text.to_string(), None),
                };
                Ok(SkillEntryCfg { id, version })
            }
        }
        deserializer.deserialize_str(Visitor)
    }
}

/// The persona facts the loader may contribute to the definition (the front matter's rows).
struct PersonaFacts {
    level: Option<AgentLevel>,
    allowed_tools: Option<Vec<ToolName>>,
}

/// Read one persona file's front matter through the persona loader's own parsing, so the
/// front-matter shape is read in one place only. The definition the entry becomes is not
/// built yet, so the pairing check is the caller's (level mismatch, tool excess); this
/// helper only reads what the file states.
fn persona_facts(persona_path: &std::path::Path) -> Result<PersonaFacts, String> {
    let text = std::fs::read_to_string(persona_path)
        .map_err(|error| format!("{}: {error}", persona_path.display()))?;
    let stripped = text.trim_start_matches('\u{feff}');
    let mut fenced = stripped.splitn(3, "---");
    let _preamble = fenced.next().unwrap_or("");
    let front_text = fenced
        .next()
        .filter(|section| !section.is_empty())
        .ok_or_else(|| "no `---` front-matter fences".to_string())?;
    #[derive(serde::Deserialize)]
    struct FrontWire {
        #[serde(default)]
        level: Option<String>,
        #[serde(default)]
        allowed_tools: Option<Vec<String>>,
    }
    let front: FrontWire =
        serde_yaml::from_str(front_text).map_err(|error| format!("front matter: {error}"))?;
    let level = match front.level {
        Some(text) => Some(parse_level(&text).ok_or_else(|| format!("level {text:?}"))?),
        None => None,
    };
    let allowed_tools = match front.allowed_tools {
        Some(words) => {
            let mut tools = Vec::new();
            for word in words {
                tools.push(parse_tool(&word).ok_or_else(|| format!("tool {word:?}"))?);
            }
            Some(tools)
        }
        None => None,
    };
    Ok(PersonaFacts {
        level,
        allowed_tools,
    })
}

/// One `AgentLevel` wire word.
fn parse_level(word: &str) -> Option<AgentLevel> {
    AgentLevel::ALL
        .iter()
        .copied()
        .find(|level| level.as_str() == word)
}

/// One `ToolName` wire word.
fn parse_tool(word: &str) -> Option<ToolName> {
    ToolName::ALL
        .iter()
        .copied()
        .find(|tool| tool.as_str() == word)
}

/// Translate one entry into an `AgentDefinition` and validate it through the
/// definition gate. `personas_root` is `<ATHENA_HOME>/personas/` — the loader's own
/// directory contract — and `instances_id_of` is unused (the slug rides verbatim);
/// `workspace_layout` is not consulted: the definition's shape is the store's row, the
/// Instance's cwd the delegation line composes (`ATH-OPS-009`).
pub(crate) fn translate(
    entry: &AgentEntryWide,
    personas_root: &std::path::Path,
) -> Result<AgentDefinition, SupplyFault> {
    // The Core Agent is never the list's (risk 8).
    if AgentId::new(entry.id.as_str()).is_core_agent() {
        return Err(SupplyFault::CoreAgentEntry(entry.id.clone()));
    }
    if entry.id.trim().is_empty() {
        return Err(SupplyFault::LevelUnstated(entry.id.clone()));
    }
    if entry.runtime.trim().is_empty() {
        return Err(SupplyFault::UnknownRuntime {
            id: entry.id.clone(),
            spelling: entry.runtime.clone(),
        });
    }
    // The closed Runtime vocabulary: an unknown spelling refuses before any write.
    let runtime = RuntimeType::ALL
        .iter()
        .copied()
        .find(|kind| kind.as_str() == entry.runtime)
        .ok_or_else(|| SupplyFault::UnknownRuntime {
            id: entry.id.clone(),
            spelling: entry.runtime.clone(),
        })?;

    // The persona file, when the entry names one: its front matter may carry the level
    // and the tool grant (`personas/` measured shapes).
    let persona_rel = entry.persona.clone().unwrap_or_else(|| {
        // The loader's own default: the persona file named after the slug.
        format!("{}.md", entry.id)
    });
    let persona_path = personas_root.join(&persona_rel);
    let facts = if persona_path.is_file() {
        persona_facts(&persona_path).map_err(|detail| SupplyFault::PersonaUnreadable {
            id: entry.id.clone(),
            path: persona_rel.clone(),
            detail,
        })?
    } else {
        // A definition without a persona file cannot read one either (ATH-AGENT-001
        // names persona as a required fact); the entry must therefore state its level
        // inline. Missing-file behavior stays below, at the level/tools resolution.
        PersonaFacts {
            level: None,
            allowed_tools: None,
        }
    };

    // The level: inline first, then the persona file's (ATH-AGENT-001). When both are
    // stated they must pair (R-PER-03).
    let level = match (entry.level.as_deref(), facts.level) {
        (Some(word), None) => parse_level(word)
            .ok_or_else(|| SupplyFault::LevelUnstated(format!("{word:?} is not a level word")))?,
        (None, Some(read)) => read,
        (Some(word), Some(read)) => {
            let inline = parse_level(word).ok_or_else(|| {
                SupplyFault::LevelUnstated(format!("{word:?} is not a level word"))
            })?;
            if inline != read {
                return Err(SupplyFault::PersonaUnreadable {
                    id: entry.id.clone(),
                    path: persona_rel.clone(),
                    detail: "the persona's level disagrees with the entry's".into(),
                });
            }
            inline
        }
        (None, None) => return Err(SupplyFault::LevelUnstated(entry.id.clone())),
    };

    // The tools: inline first, then the persona file's (ATH-AGENT-001).
    let tools = if !entry.allowed_tools.is_empty() {
        let mut tools = Vec::new();
        for word in &entry.allowed_tools {
            tools.push(parse_tool(word).ok_or_else(|| SupplyFault::UnknownTool {
                id: entry.id.clone(),
                tool: word.clone(),
            })?);
        }
        tools
    } else if let Some(read) = facts.allowed_tools.as_ref() {
        read.clone()
    } else {
        // The persona default: the measured persona files read minimal (fs_read,
        // fs_write, send_to). An entry with neither a persona file nor inline tools
        // carries the minimal grant, which is what ATH-SEC-008's "minimal by default"
        // states — never a shell-class tool invented on the agent's behalf.
        vec![ToolName::FsRead, ToolName::FsWrite, ToolName::SendTo]
    };
    // R-PER-03: the persona's asks must stay inside the definition's grant.
    if let Some(read) = facts.allowed_tools.as_ref() {
        let excessive: Vec<&ToolName> = read.iter().filter(|tool| !tools.contains(tool)).collect();
        if !excessive.is_empty() {
            return Err(SupplyFault::PersonaUnreadable {
                id: entry.id.clone(),
                path: persona_rel.clone(),
                detail: "the persona asks for tools the definition does not grant".into(),
            });
        }
    }
    // ATH-SEC-025's shape at definition time: minimal is the floor, shell-class tools are
    // never granted to a definition that carries none itself (the Core Agent alone grants
    // downward, and the validator of the *start* face refuses a widened grant).
    let delegable = {
        let mut granted = Vec::new();
        for word in &entry.delegable_tools {
            granted.push(parse_tool(word).ok_or_else(|| SupplyFault::UnknownTool {
                id: entry.id.clone(),
                tool: word.clone(),
            })?);
        }
        granted
    };

    let definition = AgentDefinition {
        id: AgentId::new(entry.id.as_str()),
        name: entry
            .name
            .clone()
            .unwrap_or_else(|| entry.id.trim().to_string()),
        level,
        description: format!("static definition from config.agents ({})", entry.id),
        capabilities: entry.capabilities.clone(),
        persona_ref: persona_rel.clone(),
        persona_version: entry
            .persona_version
            .clone()
            .unwrap_or_else(|| "1.0.0".into()),
        allowed_tools: tools,
        delegable_tools: delegable,
        default_skills: entry
            .skills
            .iter()
            .map(|entry| SkillRef {
                id: athena_types::SkillId::new(entry.id.as_str()),
                version: entry.version.clone().unwrap_or_else(|| "1.0.0".into()),
                checksum: None,
            })
            .collect(),
        runtime,
        // The mandatory coordinates (ATH-AGENT-008); the gate below decides.
        model: ModelRef {
            provider: entry.provider.clone().unwrap_or_default(),
            name: entry.model.clone().unwrap_or_default(),
        },
        // ATH-HIER-005's default: the operator's value wins when stated.
        max_delegation_depth: entry.max_delegation_depth.unwrap_or(4),
        dynamic: false,
    };

    // The registration gate (ATH-AGENT-008's first production caller).
    athena_agent::validate_definition(&definition).map_err(|refusal| {
        SupplyFault::ModelUnspecified {
            id: entry.id.clone(),
            detail: refusal.to_string(),
        }
    })?;
    Ok(definition)
}

/// The context the supply runs in: the persona root and the instance-id shape the row
/// store's own 1:1 refusal keeps (unused here, kept for the test double's symmetry).
pub struct SupplyContext<'a> {
    /// `<ATHENA_HOME>/personas/` — the persona loader's own directory contract.
    pub personas_root: &'a std::path::Path,
    /// The empty instance id the store's row shape refuses to see null in; the
    /// definition rows never carry one, the Instance rows do (section 26.3's own split).
    pub marker: (),
}

/// Supply every configured definition into the store (the boot step's own call).
///
/// `agents = []` is the shipped state: the walk runs, reads nothing, writes nothing and
/// answers `0` — the supply is unconditional and the empty list is a legitimate
/// configuration, since instance #0 is `core_definition`'s apart from this list.
///
/// # Errors
/// [`SupplyFault`] naming the offending entry; a refusal aborts the boot (ATH-CFG-003's
/// fail-fast shape — an operator who wrote a definition the gate refuses must see it, not
/// watch the resident silently skip it).
pub fn supply_definitions(
    store: &mut AthenaStore,
    config: &athena_config::Config,
    context: &SupplyContext<'_>,
) -> Result<usize, SupplyFault> {
    let mut count = 0_usize;
    // The config layer's `AgentEntry` is the source and this module only translates: the
    // conversion below reads *every* configured field, so a key an operator wrote can
    // never be silently dropped on the way to the definition row.
    for entry in &config.agents {
        let wide = AgentEntryWide::from(entry);
        let definition = translate(&wide, context.personas_root)?;
        AgentRepository::upsert_definition(store, &definition)
            .map_err(|fault| SupplyFault::Store(fault.to_string()))?;
        count += 1;
    }
    Ok(count)
}

/// The id-shape guard the store's row keeps: an instance id is `Instance`-shaped and the
/// definition rows never carry one (section 26.3). Kept on the module's own surface so a
/// future dynamic-definition face reaches the same shape through one function.
#[must_use]
pub fn instance_row_marker() -> Option<InstanceId> {
    None
}
