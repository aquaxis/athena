//! The frozen Skill Set (P5-09; `ATH-SKILL-010`, `ATH-SKILL-020`, `ATH-SKILL-021`,
//! `ATH-STATE-009`): what an Agent Instance runs with is fixed **at start** — the
//! resolver's pinned members — and never changes with the Registry's lifecycle moves
//! (`UPDATING` / `REMOVED` on a member do not retract it mid-run). The frozen record:
//! the execution-history shape of §12.8 (`agent_id` / `instance_id` / the skills'
//! `id / version / checksum` rows) — what yields the JSON block the record's
//! `to_history_json` renders.

use crate::resolver::Resolution;
use athena_types::{Actor, AgentId, InstanceId, SkillState};
use std::path::PathBuf;

/// Why a freeze was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FreezeFault {
    /// A resolver member is not in the selectable state — freeze is built from the
    /// resolver, which already refused; a record built elsewhere may not sneak in.
    #[error("{id}@{version} is {state}; a frozen set freezes only ENABLED members")]
    MemberNotEnabled {
        /// The skill.
        id: String,
        /// The version.
        version: String,
        /// The offending state.
        state: SkillState,
    },
}

/// The frozen Skill Set: fixed at start, immune to lifecycle moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenSkillSet {
    /// The owning Agent.
    pub agent_id: AgentId,
    /// The Instance it froze for.
    pub instance_id: InstanceId,
    /// The members, pinned (id / version / checksum), in resolution order.
    pub members: Vec<FrozenMember>,
    /// The frozen-at stamp.
    pub frozen_at: String,
}

/// One pinned member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenMember {
    /// The Skill slug.
    pub id: String,
    /// The pinned version (`ATH-SKILL-020`).
    pub version: String,
    /// The content checksum at freeze (`ATH-SKILL-021`).
    pub checksum: String,
}

impl FrozenSkillSet {
    /// Freeze the resolution the start path computed
    /// (`ATH-SKILL-010` — the fixed contents).
    ///
    /// # Errors
    /// [`FreezeFault::MemberNotEnabled`] when a member record rides a non-selectable
    /// state (an internal inconsistency — the resolver refuses those; this is the
    /// record-side guard against a caller assembling the set by hand).
    pub fn freeze(
        agent_id: AgentId,
        instance_id: InstanceId,
        resolution: &Resolution,
        frozen_at: &str,
    ) -> Result<Self, FreezeFault> {
        let mut members = Vec::with_capacity(resolution.members.len());
        for member in &resolution.members {
            if !member.state.is_selectable() {
                return Err(FreezeFault::MemberNotEnabled {
                    id: member.id.as_str().to_string(),
                    version: member.version.clone(),
                    state: member.state,
                });
            }
            members.push(FrozenMember {
                id: member.id.as_str().to_string(),
                version: member.version.clone(),
                checksum: member.checksum.clone(),
            });
        }
        Ok(Self {
            agent_id,
            instance_id,
            members,
            frozen_at: frozen_at.to_string(),
        })
    }

    /// Immunity from the Registry's lifecycle moves (`ATH-STATE-009` — a mid-run
    /// `UPDATING` / `REMOVED` on a member must not retract it). The check the
    /// Loader's re-arming path consults before believing a lifecycle Event implies
    /// anything about a running Instance: lifecycle moves affect the **Catalog**,
    /// never the members of a frozen set.
    ///
    /// # Errors
    /// Never — the operation is total. It deliberately carries no mutation.
    #[must_use]
    pub fn member_is_pinned(&self, id: &str) -> bool {
        self.members.iter().any(|member| member.id == id)
    }

    /// Whether a Registry Event naming one member is absorbed by the freeze
    /// (i.e. the member stays pinned through it, `ATH-SKILL-010`'s face):
    /// `true` for both `UPDATING` and `REMOVED` and every lifecycle state —
    /// the freeze does not track lifecycle states at all.
    #[must_use]
    pub const fn absorbs_lifecycle_moves(&self) -> bool {
        // Const-fact: the frozen set holds id/version/checksum facts; the Registry's
        // states are the Catalog's, not the frozen set's. No member is ever unpinned by
        // a lifecycle move — the CLI records updates as the *next launch's* resolution.
        true
    }

    /// The execution-history JSON of §12.8 (`ATH-SKILL-021`): the serialized face the
    /// `plan_step_attempts` record and the Agent's AI_LOG cite.
    ///
    /// # Errors
    /// Serialization faults (never expected for these shapes; surfaced whole).
    pub fn to_history_json(&self) -> Result<HistoryJson, FreezeFault> {
        let members = self
            .members
            .iter()
            .map(|member| {
                format!(
                    r#"{{ "id": "{}", "version": "{}", "checksum": "{}" }}"#,
                    member.id, member.version, member.checksum
                )
            })
            .collect::<Vec<_>>()
            .join(",\n    ");
        Ok(HistoryJson {
            body: format!(
                "{{\n  \"agent_id\": \"{}\",\n  \"instance_id\": \"{}\",\n  \"skills\": [\n    {}\n  ]\n}}",
                self.agent_id.as_str(),
                self.instance_id.as_str(),
                members
            ),
            path_hint: PathBuf::from(".aiprj/AI_LOG"),
        })
    }
}

/// The §12.8 JSON face, plus the location the record rides (`ATH-OBS-010`'s pairing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryJson {
    /// The JSON body (`skills[:] = {id, version, checksum}`).
    pub body: String,
    /// The location the record pairs with (the Instance's aiprj log ref).
    pub path_hint: PathBuf,
}

/// Whether the actor may request a lifecycle move (`ATH-STATE-025` re-statement on
/// the freeze side: Core Agent may only *select*, never transition the Catalog).
#[must_use]
pub const fn may_move_lifecycle(actor: &Actor) -> bool {
    !matches!(actor, Actor::CoreAgent)
}

#[cfg(test)]
mod freeze_tests;
