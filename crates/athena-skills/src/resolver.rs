//! The Skill Resolver (P5-06; spec §12.7 / `ATH-SKILL-028` … `ATH-SKILL-033`):
//! from one requested Skill to the exact set an Instance runs:
//!
//! 1. **Transitive closure** (`ATH-SKILL-028`) over `dependencies`, visiting each Skill's
//!    row in the store.
//! 2. **Max satisfying version** (`ATH-SKILL-029`): every dependency edges declares a
//!    SemVer *range*; among the range-satisfying versions of that id, the greatest wins.
//! 3. **Conflict ⇒ `SkillError`** (`ATH-SKILL-030` / `ATH-SKILL-031`): when two closure
//!    members demand ranges of one id that resolve to different versions, the resolution
//!    is *refused* — a silent pick for either side is never made.
//! 4. **Permission union** (`ATH-SKILL-031`, `ATH-SKILL-023`, `ATH-SEC-012`): the union
//!    of `required_permissions` over the closure must be contained in the Agent's
//!    permissions.
//! 5. **All-`ENABLED`** (`ATH-STATE-008`): every closure member must be in the
//!    selectable state — a disabled or failed dependency vetoes the set.
//! 6. **Tool check** (`ATH-SKILL-032`/`033`): every member's `required_tools` must be in
//!    the Agent's `allowed_tools` **and** in the Runtime `capabilities()` set — a tool
//!    the platform cannot honor fails the set even when the policy allows it.

use crate::registry::SkillStore;
use athena_types::{Permission, Skill, SkillRef, SkillState, ToolName};

/// Why the resolution was refused (the Core Agent's facts for the `SkillError` class).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolverFault {
    /// No enabled version of the named dependency (nothing satisfies at all).
    #[error("no ENABLED version of {id} satisfies anything")]
    NothingSatisfies {
        /// The missing skill.
        id: String,
    },
    /// Two members demand ranges of one id and they resolve differently ('never pick
    /// one silently').
    #[error("conflicting ranges on {id}: {first} vs {second}")]
    ConflictingRanges {
        /// The contested skill.
        id: String,
        /// The first resolved version.
        first: String,
        /// The second resolved version.
        second: String,
    },
    /// A closure member is not ENABLED (its state and id@version are named).
    #[error("{id}@{version} is {state}, not ENABLED")]
    NotEnableable {
        /// The skill.
        id: String,
        /// Its version.
        version: String,
        /// Its state.
        state: SkillState,
    },
    /// The permission union exceeds the Agent's own grant (`PermissionError`-class).
    #[error("the permission union exceeds the agent's grant: {0:?}")]
    PermissionExceeded(Permission),
    /// A required tool is outside the agent's grant.
    #[error("tool {0:?} is outside the agent's allowed_tools")]
    ToolNotGranted(String),
    /// A required tool is outside the Runtime's declared capabilities.
    #[error("tool {0:?} is outside the runtime capabilities()")]
    ToolNotCapable(String),
}

/// The agent-side facts the check runs against.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentFace {
    /// The Agent definition's `allowed_tools` (§11.1-calibrated grant).
    pub allowed_tools: Vec<ToolName>,
    /// The Agent definition's `permissions` (the §26.1 shape the check inflates).
    pub permissions: Vec<Permission>,
}

/// The resolution: the closure in deterministic (id, version) order, plus the union
/// facts the Loader/Frozen set carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    /// Every closure member (root first), each pinned to its resolved version.
    pub members: Vec<Skill>,
    /// The permission union over the members.
    pub permission_union: Vec<Permission>,
    /// The tool union over the members (the SKILL.md placement manifest input).
    pub tool_union: Vec<ToolName>,
}

// --- SemVer satisfaction (ranges own the constraint) --------------------------------------

/// One relation, parsed from the range shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bound {
    /// `^X.Y.Z`: same major.
    Caret([u64; 3]),
    /// `~X.Y.Z`: same major.minor.
    Tilde([u64; 3]),
    /// `>=X.Y.Z`.
    AtLeast([u64; 3]),
    /// `=X.Y.Z` (or a bare version).
    Exact([u64; 3]),
}

fn parse_bound(range: &str) -> Option<Bound> {
    let range = range.trim();
    let (bound, rest) = if let Some(rest) = range.strip_prefix("^") {
        (BoundKind::Caret, rest)
    } else if let Some(rest) = range.strip_prefix("~") {
        (BoundKind::Tilde, rest)
    } else if let Some(rest) = range.strip_prefix(">=") {
        (BoundKind::AtLeast, rest)
    } else if let Some(rest) = range.strip_prefix("=") {
        (BoundKind::Exact, rest)
    } else {
        (BoundKind::Exact, range)
    };
    let head = rest.split(['-', '+']).next().unwrap_or(rest);
    let parts: Option<Vec<u64>> = head
        .split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect();
    let values = parts?;
    match values.len() {
        2 => {
            let mut triple = [values[0], values[1], 0];
            let _ = &mut triple;
            Some(match bound {
                BoundKind::Caret => Bound::Caret(triple),
                BoundKind::Tilde => Bound::Tilde(triple),
                BoundKind::AtLeast => Bound::AtLeast(triple),
                BoundKind::Exact => Bound::Exact(triple),
            })
        }
        3 => Some(match bound {
            BoundKind::Caret => Bound::Caret([values[0], values[1], values[2]]),
            BoundKind::Tilde => Bound::Tilde([values[0], values[1], values[2]]),
            BoundKind::AtLeast => Bound::AtLeast([values[0], values[1], values[2]]),
            BoundKind::Exact => Bound::Exact([values[0], values[1], values[2]]),
        }),
        _ => None,
    }
}

/// Kind marker for the parse above.
enum BoundKind {
    Caret,
    Tilde,
    AtLeast,
    Exact,
}

/// `MAJOR.MINOR.PATCH` head of a literal version, as numbers.
fn version_triple(version: &str) -> Option<[u64; 3]> {
    let head = version.split(['-', '+']).next().unwrap_or(version);
    let parts: Option<Vec<u64>> = head
        .split('.')
        .map(|part| part.trim().parse::<u64>().ok())
        .collect();
    let mut values = parts?;
    while values.len() < 3 {
        values.push(0);
    }
    Some([values[0], values[1], values[2]])
}

/// Whether `version` satisfies `range` (the §12.3 shapes: `^1.0.0`, `~2.3`, `>=0.9.0`,
/// `=1.2.3`, and a bare version).
#[must_use]
pub fn satisfies(version: &str, range: &str) -> bool {
    let Some(candidate) = version_triple(version) else {
        return false;
    };
    let Some(bound) = parse_bound(range) else {
        return false;
    };
    match bound {
        Bound::Caret(base) => candidate >= base && candidate[0] == base[0],
        Bound::Tilde(base) => {
            candidate >= base && candidate[0] == base[0] && candidate[1] == base[1]
        }
        Bound::AtLeast(base) => candidate >= base,
        Bound::Exact(base) => candidate == base,
    }
}

/// The greater of two versions, by `MAJOR.MINOR.PATCH`.
fn greater(a: &str, b: &str) -> Option<String> {
    let (Some(left), Some(right)) = (version_triple(a), version_triple(b)) else {
        return None;
    };
    Some(if left >= right {
        a.to_string()
    } else {
        b.to_string()
    })
}

// --- The resolution ---------------------------------------------------------------------

/// Resolve the closure for one requested `(id, version)` Skill Ref.
///
/// # Errors
/// [`ResolverFault`] — the first failing rule, in spec order.
pub fn resolve(
    requested: &SkillRef,
    agent: &AgentFace,
    runtime_tools: &[ToolName],
    store: &dyn SkillStore,
) -> Result<Resolution, ResolverFault> {
    use std::collections::BTreeMap;
    use std::collections::BTreeSet;

    // 1 + 2: the closure. `members: id -> pinned row`; `ranges: id -> the set of ranges
    // every member declared on it` — one per requring member.
    let mut members: BTreeMap<String, Skill> = BTreeMap::new();
    let mut ranges: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();

    // The root row: exactly the version requested.
    let root = SkillStore::read(store, &requested.id, &requested.version)
        .map_err(seam_fault)?
        .ok_or_else(|| ResolverFault::NothingSatisfies {
            id: requested.id.as_str().to_string(),
        })?;
    members.insert(root.id.as_str().to_string(), root.clone());

    // Work the closure: members discovered in the walk carry their own dependencies with
    // ranges that pick among all enabled rows of that id.
    let mut frontier: Vec<Skill> = vec![root.clone()];
    while let Some(member) = frontier.pop() {
        for edge in &member.dependencies {
            let id = edge.id.clone();
            let range = edge.version.clone();
            // Every enabled row of the id — the candidate set.
            let all_rows: Vec<Skill> = SkillStore::list_all(store).map_err(seam_fault)?;
            let candidates: Vec<Skill> = all_rows.into_iter().filter(|row| row.id == id).collect();
            let satisfying: Vec<Skill> = candidates
                .into_iter()
                .filter(|row| satisfies(&row.version, &range))
                .collect();
            // 5 holds at the edge too: the pick runs only over ENABLED rows; when the
            // range is satisfied but only in a non-selectable state, the fact is the
            // row's state, not "nothing".
            let enabled: Vec<Skill> = satisfying
                .iter()
                .filter(|row| row.state.is_selectable())
                .cloned()
                .collect();
            if enabled.is_empty() {
                return match satisfying.first() {
                    Some(row) => Err(ResolverFault::NotEnableable {
                        id: row.id.as_str().to_string(),
                        version: row.version.clone(),
                        state: row.state,
                    }),
                    None => Err(ResolverFault::NothingSatisfies {
                        id: id.as_str().to_string(),
                    }),
                };
            }
            let best: Option<String> = enabled.iter().fold(None::<String>, |acc, row| match acc {
                None => Some(row.version.clone()),
                Some(acc) => greater(&acc, &row.version).or(Some(acc)),
            });
            let best_ref: &str = &best.clone().unwrap_or_default();
            let Some(best) = best else {
                return Err(ResolverFault::NothingSatisfies {
                    id: id.as_str().to_string(),
                });
            };
            // 3: an id demanded by more than one member must resolve to the *same* max.
            ranges
                .entry(id.as_str().to_string())
                .or_default()
                .insert(member.id.as_str().to_string(), best.clone());
            let pinned = enabled
                .into_iter()
                .find(|row| row.version == best_ref)
                .expect("the max enabled row exists");
            let existing = members.get(id.as_str());
            if let Some(existing) = existing
                && existing.version != pinned.version
            {
                return Err(ResolverFault::ConflictingRanges {
                    id: id.as_str().to_string(),
                    first: existing.version.clone(),
                    second: pinned.version.clone(),
                });
            }
            if !members.contains_key(id.as_str()) {
                members.insert(id.as_str().to_string(), pinned.clone());
                frontier.push(pinned.clone());
            }
        }
    }

    // 3 again: the ranges map — the same id demanded with different satisfied versions
    // by different members is a conflict (the per-member walk cannot see it all).
    for (id, per_member) in &ranges {
        let pins: BTreeSet<&String> = per_member.values().collect();
        if pins.len() > 1 {
            let mut pins = pins.into_iter().cloned().collect::<Vec<_>>();
            pins.sort();
            return Err(ResolverFault::ConflictingRanges {
                id: id.clone(),
                first: pins[0].clone(),
                second: pins[1].clone(),
            });
        }
    }

    let members: Vec<Skill> = members.into_values().collect();

    // 5: every closure member must be ENABLED (`ATH-STATE-008`).
    for member in &members {
        if !member.state.is_selectable() {
            return Err(ResolverFault::NotEnableable {
                id: member.id.as_str().to_string(),
                version: member.version.clone(),
                state: member.state,
            });
        }
    }

    // 4: the permission union ⊆ permissions(Agent) (`ATH-SKILL-031`).
    let mut union: Vec<Permission> = members
        .iter()
        .flat_map(|member| member.required_permissions.clone())
        .collect();
    union = sort_permissions(union);
    for required in &union {
        if !agent.permissions.contains(required) {
            return Err(ResolverFault::PermissionExceeded(required.clone()));
        }
    }

    // 6: the tool union must sit inside both the agent grant and the platform face
    // (`ATH-SKILL-032/033`).
    let mut tools: Vec<ToolName> = members
        .iter()
        .flat_map(|member| member.required_tools.iter().copied())
        .collect();
    tools.sort();
    tools.dedup();
    for required in &tools {
        if !agent.allowed_tools.contains(required) {
            return Err(ResolverFault::ToolNotGranted(required.as_str().to_string()));
        }
        if !runtime_tools.contains(required) {
            return Err(ResolverFault::ToolNotCapable(required.as_str().to_string()));
        }
    }

    Ok(Resolution {
        permission_union: union,
        tool_union: tools,
        members,
    })
}

/// A deterministic permission-union ordering (the §26.1 shapes have no Ord; the
/// wire form orders stably instead).
fn sort_permissions(mut union: Vec<Permission>) -> Vec<Permission> {
    union.sort_by_key(form);
    union.dedup_by(|a, b| form(a) == form(b));
    union
}

/// A stable wire-form text of one permission (the §26.1 enum carries serde names).
fn form(permission: &Permission) -> String {
    let name = permission_to_key(permission);
    let detail = match permission {
        Permission::Tool { tool } => tool.as_str().to_string(),
        Permission::Fs { mode, scope } => format!("{:?}:{}", mode, scope),
        Permission::Network { mode, hosts } => {
            format!("{:?}:{}", mode, hosts.clone().unwrap_or_default().join(","))
        }
        Permission::Process => "".to_string(),
    };
    format!("{name}={detail}")
}

/// The discriminator key of a permission shape.
fn permission_to_key(permission: &Permission) -> &'static str {
    match permission {
        Permission::Tool { .. } => "tool",
        Permission::Fs { .. } => "fs",
        Permission::Network { .. } => "network",
        Permission::Process => "process",
    }
}

fn seam_fault<E: std::fmt::Display>(error: E) -> ResolverFault {
    // A store failure is not a resolution fact; it surfaces Internal-class through the
    // wiring's caller — this fault set keeps the Core Agent's facts pure.
    ResolverFault::ToolNotGranted(format!("store failed: {error}"))
}

#[cfg(test)]
mod resolver_tests;
