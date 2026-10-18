//! The hierarchy's composition checks (design §3.3's Hierarchy Manager surface).
//!
//! The tree is *logical* (ATH-HIER-010): unique parentage, acyclic (ATH-HIER-002/003),
//! depth-limited (ATH-HIER-005, default 4), immediate-child-only (ATH-HIER-004; a Core
//! Agent-granted level skip is the recorded widening), level ↔ work-unit mapped
//! (ATH-HIER-012), and permission-contained (ATH-HIER-006 / ATH-SEC-009:
//! `allowed_tools(child) ⊆ delegable_tools(parent)`).
//!
//! Composition runs on Core Agent's drafts; these functions run before anything
//! persists, and the Events carry the settled tree (ATH-HIER-009's `HierarchyComposed`).

use athena_types::{AgentId, AgentLevel, HierarchyNodeDraft};
use thiserror::Error;

/// Why composition refused. The depth ceiling is a `PolicyError`; everything else is a
/// `ValidationError` (§20.1).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CompositionError {
    /// Two nodes claim the same agent (ATH-HIER-002's unique parentage).
    #[error("agent {0:?} appears more than once")]
    DuplicateAgent(AgentId),
    /// A node names a superior that is not in the composition.
    #[error("node {0:?} names an absent superior")]
    AbsentSuperior(AgentId),
    /// The parent's level is not the immediate parent of the child's, without a recorded
    /// Core-Agent grant (ATH-HIER-004).
    #[error("{child:?} ({child_level}) cannot sit directly under {parent:?} ({parent_level})")]
    LevelSkip {
        /// The child.
        child: AgentId,
        /// The child's level.
        child_level: AgentLevel,
        /// The named parent.
        parent: AgentId,
        /// The parent's level.
        parent_level: AgentLevel,
    },
    /// The chain to the root exceeds the depth ceiling (ATH-HIER-005, default 4).
    #[error("the chain through {0:?} exceeds depth {1}")]
    DepthCeiling(AgentId, u64),
    /// The composition closes a loop (ATH-HIER-003).
    #[error("the composition contains a cycle through {0:?}")]
    Cycle(AgentId),
    /// The child's `allowed_tools` exceed the superior's `delegable_tools`
    /// (ATH-HIER-006, ATH-SEC-009).
    #[error("agent {agent:?} requests tools outside its superior's delegable_tools: {outside:?}")]
    PermissionEscalation {
        /// The child.
        agent: AgentId,
        /// The refused tools.
        outside: Vec<athena_types::ToolName>,
    },
}

impl CompositionError {
    /// The §20.1 class wire value (`PolicyError` only for the depth ceiling).
    #[must_use]
    pub const fn class(&self) -> &'static str {
        if self.is_policy() {
            "PolicyError"
        } else {
            "ValidationError"
        }
    }

    /// Whether the depth ceiling produced this error.
    #[must_use]
    pub const fn is_policy(&self) -> bool {
        matches!(self, Self::DepthCeiling(..))
    }

    /// A stable machine code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::DuplicateAgent(_) => "HIER_DUPLICATE_AGENT",
            Self::AbsentSuperior(_) => "HIER_ABSENT_SUPERIOR",
            Self::LevelSkip { .. } => "HIER_LEVEL_SKIP",
            Self::DepthCeiling(..) => "HIER_DEPTH_CEILING",
            Self::Cycle(_) => "HIER_CYCLE",
            Self::PermissionEscalation { .. } => "HIER_PERMISSION_ESCALATION",
        }
    }
}

/// One composed node after the checks: the agent, its level, its unique superior, the
/// *effective* tool set — `allowed_tools(child) ∩ delegable_tools(parent)` per the
/// containment rule, carried downward so every descendant sees the intersection — and the
/// recorded sibling-communication permission list (ATH-MSG-018): the peers this node may
/// address directly, exactly as Core Agent's `compose_hierarchy` request listed them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettledNode {
    /// The agent at this node.
    pub agent_id: AgentId,
    /// Its level.
    pub level: AgentLevel,
    /// Its unique superior; `None` only for the Producer root.
    pub superior: Option<AgentId>,
    /// The effective tools after containment.
    pub effective_tools: Vec<athena_types::ToolName>,
    /// The sibling peers this node may address directly, carried verbatim from the draft
    /// (ATH-MSG-018 — recorded at composition, checked by Core Host at delivery).
    pub sibling_acl: Vec<AgentId>,
}

/// One submitted definition's facts (level and the two permission sets), keyed by agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionFacts {
    /// The agent.
    pub agent_id: AgentId,
    /// Its level.
    pub level: AgentLevel,
    /// What it may run itself.
    pub allowed_tools: Vec<athena_types::ToolName>,
    /// What it may grant downward.
    pub delegable_tools: Vec<athena_types::ToolName>,
}

/// The parent's level for a child at `child_level` (one step up the chain).
#[must_use]
pub const fn parent_level(child_level: AgentLevel) -> Option<AgentLevel> {
    match child_level {
        AgentLevel::Producer => None,
        AgentLevel::Director => Some(AgentLevel::Producer),
        AgentLevel::Lead => Some(AgentLevel::Director),
        AgentLevel::Worker => Some(AgentLevel::Lead),
    }
}

/// Compose a tree from Core Agent's drafts, running every check in §8.2's order:
/// duplicate agents, absent superiors, immediate-child levels (widened only by a recorded
/// Core-Agent grant), cycles, the depth ceiling (`max_depth`, ATH-HIER-005's default 4),
/// and permission containment downward.
///
/// `facts` reads the *definitions'* levels and permission sets (a draft names an agent;
/// its permission reality is the definition's). `granted_skips` carries the recorded
/// `(child agent, granted parent level)` pairs (ATH-HIER-004's explicit widening). The
/// drafts' `sibling_acl` lists ride the settled nodes untouched (ATH-MSG-018): composition
/// records the grant; Core Host checks it at delivery — composing never adjudicates
/// siblings.
///
/// # Errors
/// [`CompositionError`] variants, each carrying the offending agent(s).
pub fn compose(
    drafts: &[HierarchyNodeDraft],
    facts: &[DefinitionFacts],
    max_depth: u64,
    granted_skips: &[(AgentId, AgentLevel)],
) -> Result<Vec<SettledNode>, CompositionError> {
    use std::collections::{BTreeMap, BTreeSet};

    // Duplicate agents first (unique parentage, ATH-HIER-002).
    let mut seen: BTreeSet<&AgentId> = BTreeSet::new();
    for draft in drafts {
        let fresh = seen.insert(&draft.agent_id);
        if !fresh {
            return Err(CompositionError::DuplicateAgent(draft.agent_id.clone()));
        }
    }

    // Every named superior must exist in this composition.
    let present: BTreeSet<&AgentId> = drafts.iter().map(|draft| &draft.agent_id).collect();
    for draft in drafts {
        if let Some(superior) = &draft.superior_agent_id
            && !present.contains(superior)
        {
            return Err(CompositionError::AbsentSuperior(superior.clone()));
        }
    }

    // Cycle check over superior-agent edges (ATH-HIER-003): follow each node upward; a
    // node seen twice on one walk is a loop.
    for draft in drafts {
        let mut walked: BTreeSet<&AgentId> = BTreeSet::new();
        let mut current = Some(&draft.agent_id);
        while let Some(node) = current {
            if !walked.insert(node) {
                return Err(CompositionError::Cycle(node.clone()));
            }
            current = drafts
                .iter()
                .find(|seek| &seek.agent_id == node)
                .and_then(|seek| seek.superior_agent_id.as_ref());
        }
    }

    // The facts per agent (level + permission sets).
    let lookup: BTreeMap<&AgentId, &DefinitionFacts> =
        facts.iter().map(|fact| (&fact.agent_id, fact)).collect();

    // Depth + immediate-child level + containment, walking the tree downward from roots.
    let mut settled: Vec<SettledNode> = Vec::with_capacity(drafts.len());
    for draft in drafts {
        // The superior's level must be the child level's immediate parent, unless a
        // recorded grant widened it (ATH-HIER-004).
        if let Some(superior) = &draft.superior_agent_id {
            let child_level = draft.level;
            let parent = drafts
                .iter()
                .find(|seek| &seek.agent_id == superior)
                .map(|seek| seek.level)
                .ok_or_else(|| CompositionError::AbsentSuperior(superior.clone()))?;
            let granted = granted_skips
                .iter()
                .any(|(agent, level)| agent == &draft.agent_id && *level == parent);
            if !granted && parent_level(child_level) != Some(parent) {
                // The containment check running *before* a level verdict would hide the
                // genuine problem: level-skip is itself the first refusal — but with a
                // recorded grant the pair is legal and the containment check proceeds.
                return Err(CompositionError::LevelSkip {
                    child: draft.agent_id.clone(),
                    child_level,
                    parent: superior.clone(),
                    parent_level: parent,
                });
            }
        }

        // Depth: the chain of superiors to the producer root, capped by ATH-HIER-005.
        let mut chain = 0_u64;
        let mut current = draft.superior_agent_id.as_ref();
        while let Some(node) = current {
            chain += 1;
            if chain > max_depth {
                return Err(CompositionError::DepthCeiling(
                    draft.agent_id.clone(),
                    max_depth,
                ));
            }
            current = drafts
                .iter()
                .find(|seek| &seek.agent_id == node)
                .and_then(|seek| seek.superior_agent_id.as_ref());
        }

        // Permission containment (ATH-HIER-006): the child's tools must sit inside the
        // superior's delegable set. A node without registered facts holds no tools —
        // which trivially satisfies containment (permission reality is the definition's;
        // the Runtime start-path re-checks against the store).
        let empty: Vec<athena_types::ToolName> = Vec::new();
        let child_tools: &Vec<athena_types::ToolName> = lookup
            .get(&draft.agent_id)
            .map_or(&empty, |fact| &fact.allowed_tools);
        let effective: Vec<athena_types::ToolName> = match &draft.superior_agent_id {
            None => child_tools.clone(),
            Some(superior) => {
                let parent_delegable = lookup
                    .get(superior)
                    .map_or(&empty, |fact| &fact.delegable_tools);
                let outside: Vec<athena_types::ToolName> = child_tools
                    .iter()
                    .filter(|tool| !parent_delegable.contains(tool))
                    .cloned()
                    .collect();
                if !outside.is_empty() {
                    return Err(CompositionError::PermissionEscalation {
                        agent: draft.agent_id.clone(),
                        outside,
                    });
                }
                child_tools.clone()
            }
        };

        settled.push(SettledNode {
            agent_id: draft.agent_id.clone(),
            level: draft.level,
            superior: draft.superior_agent_id.clone(),
            effective_tools: effective,
            sibling_acl: draft.sibling_acl.clone(),
        });
    }

    Ok(settled)
}
