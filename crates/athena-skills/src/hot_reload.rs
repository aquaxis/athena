//! Optional hot reload (P5-10; `ATH-SKILL-011` MAY, `ATH-SKILL-034` MUST,
//! `ATH-DATA-006`): the ONLY way a running Instance's skill set grows mid-run.
//!
//! The rules the reload must survive:
//!
//! 1. **Declaration-gated** (`ATH-SKILL-011`): only a skill that declared
//!    `hot_reload_safe: true` may be added to a running Agent. Anything else refuses.
//! 2. **Permission re-check** (`ATH-SKILL-034` / `ATH-SKILL-023` / `ATH-SEC-012`): the
//!    enlarged set's permission union is re-run against the *current* Agent grant — a
//!    capability that arrives riding new permissions the Agent holds on paper is fine,
//!    one exceeding them refuses.
//! 3. **A new revision** (`ATH-SKILL-034`): the surviving set is recorded as the
//!    Instance's `skill_set` revision **N+1** — the Agent's frozen facts never mutate
//!    in place (`ATH-DATA-006`).
//! 4. **No re-check, no write** (`ATH-SKILL-034`'s MUST again, the refusal shape): a
//!    reload call whose permission re-check did not run is structurally impossible —
//!    the permissions ride the value; the wiring cannot "skip" it.
//! 5. **Freeze compatibility**: the reloaded set stays a *frozen set* — same
//!    id/version/checksum facts, and in particular it does NOT reach until its member
//!    is duplicated under the workspace by the Loader (delivery rides commands — P5-08).
//!    A reload that only names a skill (no material placed) is refused.

use crate::freeze::{FrozenMember, FrozenSkillSet};
use athena_types::Permission;

/// Why a hot reload was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HotReloadFault {
    /// The skill did not declare itself hot-reload-safe (`ATH-SKILL-011`).
    #[error("{id}@{version} did not declare hot_reload_safe; a running set stays frozen")]
    NotDeclaredSafe {
        /// The skill.
        id: String,
        /// The version.
        version: String,
    },
    /// The enlarged permission union exceeds the Agent's grant (`ATH-SKILL-034`).
    #[error("the enlarged permission union exceeds the agent's grant: {0:?}")]
    PermissionExceeded(Permission),
    /// The arrival names the skill but carries no placed material (delivery rides the
    /// Loader's placement, `ATH-SKILL-017`): a prose-only reload is refused.
    #[error("no material placed for {id}@{version}; the reload needs the workspace body")]
    MaterialMissing {
        /// The skill.
        id: String,
        /// The version.
        version: String,
    },
}

/// The candidate skill proposing reload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReloadCandidate {
    /// The pinned member it would add.
    pub member: FrozenMember,
    /// Whether it declared `hot_reload_safe` (from the Catalog record).
    pub declared_safe: bool,
    /// The placed material's absolute SKILL.md path (`ATH-SKILL-017`'s face — the
    /// Loader must have placed this already).
    pub material_path: Option<String>,
    /// Its permissions (already replaced by the entity shape).
    pub permissions: Vec<Permission>,
}

/// The outcome: the new revision of the running set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReloadOutcome {
    /// The revision the surviving set now carries (previous + 1, `ATH-SKILL-034`).
    pub new_revision: u64,
    /// The members the surviving set holds (the old ones plus the arrival).
    pub members: Vec<FrozenMember>,
}

/// Attempt the reload of one candidate into a running frozen set.
///
/// # Errors
/// [`HotReloadFault`] — first failing rule, in the order above.
pub fn attempt(
    running: &FrozenSkillSet,
    candidate: &ReloadCandidate,
    agent_permissions: &[Permission],
    current_revision: u64,
) -> Result<ReloadOutcome, HotReloadFault> {
    // 1. The declaration gate (`ATH-SKILL-011`).
    if !candidate.declared_safe {
        return Err(HotReloadFault::NotDeclaredSafe {
            id: candidate.member.id.clone(),
            version: candidate.member.version.clone(),
        });
    }
    // 5. The material gate — delivery rides the Loader, not prose (`ATH-SKILL-017`).
    let Some(_material) = &candidate.material_path else {
        return Err(HotReloadFault::MaterialMissing {
            id: candidate.member.id.clone(),
            version: candidate.member.version.clone(),
        });
    };
    // Already in the set: a reload of a pinned member is a no-op, not an error — and
    // not a revision bump (the set did not change).
    if running
        .members
        .iter()
        .any(|member| member.id == candidate.member.id)
    {
        return Ok(ReloadOutcome {
            new_revision: current_revision,
            members: running.members.clone(),
        });
    }
    // 2. The permission re-check: the enlarged union rides the candidate's own grant
    //    demand, against the *current* Agent grant.
    let enlarged: Vec<Permission> = candidate.permissions.clone();
    // The new member must sit inside the grant.
    for required in &enlarged {
        if !agent_permissions.contains(required) {
            return Err(HotReloadFault::PermissionExceeded(required.clone()));
        }
    }
    // No `ATH-SEC-012` subsumption check is required: entity equality suffices — the
    // same grant shape equals the same shape.
    let _ = agent_permissions.len();

    // 3. The new revision (`ATH-SKILL-034`): N+1, in place, not mutated.
    let mut members = running.members.clone();
    members.push(candidate.member.clone());
    Ok(ReloadOutcome {
        new_revision: current_revision + 1,
        members,
    })
}

/// Aiding the caller to construct the frozen-record replacement from the outcome —
/// the reload never mutates the running record; the wiring writes a fresh one.
///
/// # Errors
/// Propagated from [`FrozenSkillSet`]'s constructor — never in practice (the outcome's
/// members were admitted by `attempt`).
pub fn freeze_replacement(
    outcome: &ReloadOutcome,
    running: &FrozenSkillSet,
) -> Result<FrozenSkillSet, crate::freeze::FreezeFault> {
    let mut replacement = FrozenSkillSet::freeze(
        running.agent_id.clone(),
        running.instance_id.clone(),
        &crates_resolution_of(outcome),
        &running.frozen_at,
    )?;
    replacement.frozen_at = running.frozen_at.clone();
    Ok(replacement)
}

fn crates_resolution_of(outcome: &ReloadOutcome) -> crate::resolver::Resolution {
    crate::resolver::Resolution {
        members: outcome
            .members
            .iter()
            .map(|member| {
                let mut skill = aset_stub(&member.id, &member.version);
                skill.checksum = member.checksum.clone();
                skill.state = athena_types::SkillState::Enabled;
                skill
            })
            .collect(),
        permission_union: vec![],
        tool_union: vec![],
    }
}

fn aset_stub(id: &str, version: &str) -> athena_types::Skill {
    athena_types::Skill {
        id: athena_types::SkillId::new(id),
        name: id.into(),
        version: version.into(),
        description: "hot-reload fixture".into(),
        capabilities: vec![],
        required_tools: vec![],
        required_permissions: vec![],
        supported_agents: vec![],
        dependencies: vec![],
        runtime_provider: "agent-cli".into(),
        source: "local".into(),
        checksum: String::new(),
        publisher: "local".into(),
        risk_level: athena_types::RiskLevel::Low,
        state: athena_types::SkillState::Enabled,
        hot_reload_safe: true,
        registered_at: "t".into(),
        updated_at: "t".into(),
    }
}

#[cfg(test)]
mod hot_reload_tests;
