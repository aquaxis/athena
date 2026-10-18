//! The Skill Registry's persistence: `SkillRepository` implemented on `SqliteStore`
//! over the §27.3 `skills` table (P5-13's supporting layer).

use crate::error::{PersistenceError, Result as PersResult};
use crate::repository::SkillRepository;
use athena_types::{RiskLevel, Skill, SkillId, SkillState, ToolName};
use rusqlite::params;
use serde_json as json;

const COLUMNS: &str = "id, name, version, description, capabilities_json, required_tools_json, \
required_permissions_json, supported_agents_json, dependencies_json, runtime_provider, source, \
checksum, publisher, risk_level, state, hot_reload_safe, registered_at, updated_at";

fn row_to_skill(row: &rusqlite::Row<'_>) -> rusqlite::Result<Skill> {
    let risk_raw: String = row.get(13)?;
    let state_raw: String = row.get(14)?;
    Ok(Skill {
        id: SkillId::new(row.get::<_, String>(0)?),
        name: row.get(1)?,
        version: row.get(2)?,
        description: row.get(3)?,
        capabilities: json::from_str(&row.get::<_, String>(4)?).unwrap_or_default(),
        required_tools: json::from_str(&row.get::<_, String>(5)?).unwrap_or_default(),
        required_permissions: json::from_str(&row.get::<_, String>(6)?).unwrap_or_default(),
        supported_agents: json::from_str(&row.get::<_, String>(7)?).unwrap_or_default(),
        dependencies: json::from_str(&row.get::<_, String>(8)?).unwrap_or_default(),
        runtime_provider: row.get(9)?,
        source: row.get(10)?,
        checksum: row.get(11)?,
        publisher: row.get(12)?,
        risk_level: decode_risk(&risk_raw),
        state: decode_state(&state_raw),
        hot_reload_safe: row.get::<_, i64>(15)? != 0,
        registered_at: row.get(16)?,
        updated_at: row.get(17)?,
    })
}

fn decode_risk(text: &str) -> RiskLevel {
    RiskLevel::ALL
        .iter()
        .copied()
        .find(|risk| risk.as_str() == text)
        .unwrap_or(RiskLevel::Medium)
}

fn decode_state(text: &str) -> SkillState {
    SkillState::ALL
        .iter()
        .copied()
        .find(|state| state.as_str() == text)
        .unwrap_or(SkillState::Removed)
}

fn encode_tools(values: &[ToolName]) -> String {
    json::to_string(&values.iter().map(ToolName::as_str).collect::<Vec<_>>()).unwrap_or_default()
}

fn encode_agents(values: &[athena_types::AgentId]) -> String {
    json::to_string(&values.iter().map(|id| id.as_str()).collect::<Vec<_>>()).unwrap_or_default()
}

impl SkillRepository for crate::SqliteStore {
    fn upsert(&mut self, skill: &Skill) -> PersResult<()> {
        self
            .database()
            .connection()
            .execute(
                &format!(
                    "INSERT OR REPLACE INTO skills ({COLUMNS}) VALUES \
                     (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)"
                ),
                params![
                    skill.id.as_str(),
                    skill.name,
                    skill.version,
                    skill.description,
                    json::to_string(&skill.capabilities).unwrap_or_default(),
                    encode_tools(&skill.required_tools),
                    json::to_string(&skill.required_permissions).unwrap_or_default(),
                    encode_agents(&skill.supported_agents),
                    json::to_string(&skill.dependencies).unwrap_or_default(),
                    skill.runtime_provider,
                    skill.source,
                    skill.checksum,
                    skill.publisher,
                    skill.risk_level.as_str(),
                    skill.state.as_str(),
                    i64::from(skill.hot_reload_safe),
                    skill.registered_at,
                    skill.updated_at,
                ],
            )
            .map_err(|error| PersistenceError::Plainly(error.to_string()))?;
        Ok(())
    }

    fn get(&self, id: &SkillId, version: &str) -> PersResult<Option<Skill>> {
        let text = format!("SELECT {COLUMNS} FROM skills WHERE id = ?1 AND version = ?2");
        use rusqlite::OptionalExtension;
        let row = self
            .database()
            .connection()
            .query_row(&text, params![id.as_str(), version], row_to_skill)
            .optional()
            .map_err(|error| PersistenceError::Plainly(error.to_string()))?;
        Ok(row)
    }

    fn list_versions(&self, id: &SkillId) -> PersResult<Vec<Skill>> {
        let text = format!("SELECT {COLUMNS} FROM skills WHERE id = ?1 ORDER BY version");
        let mut statement = self
            .database()
            .connection()
            .prepare(&text)
            .map_err(|error| PersistenceError::Plainly(error.to_string()))?;
        let rows = statement
            .query_map([id.as_str()], row_to_skill)
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<Skill>>>())
            .map_err(|error| PersistenceError::Plainly(error.to_string()))?;
        Ok(rows)
    }

    fn list_by_state(&self, state: SkillState) -> PersResult<Vec<Skill>> {
        let text = format!("SELECT {COLUMNS} FROM skills WHERE state = ?1 ORDER BY id, version");
        let mut statement = self
            .database()
            .connection()
            .prepare(&text)
            .map_err(|error| PersistenceError::Plainly(error.to_string()))?;
        let rows = statement
            .query_map([state.as_str()], row_to_skill)
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<Skill>>>())
            .map_err(|error| PersistenceError::Plainly(error.to_string()))?;
        Ok(rows)
    }

    fn search_by_capability(&self, capability: &str) -> PersResult<Vec<Skill>> {
        // The JSON array renders with quotes; an exact-text LIKE honors the exact capability.
        let text = format!(
            "SELECT {COLUMNS} FROM skills WHERE state = ?1 AND capabilities_json LIKE ?2 \
             ORDER BY id, version"
        );
        let pattern = format!("%\"{capability}\"%");
        let mut statement = self
            .database()
            .connection()
            .prepare(&text)
            .map_err(|error| PersistenceError::Plainly(error.to_string()))?;
        let rows = statement
            .query_map(params![SkillState::Enabled.as_str(), pattern], row_to_skill)
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<Skill>>>())
            .map_err(|error| PersistenceError::Plainly(error.to_string()))?;
        Ok(rows)
    }

    fn delete(&mut self, id: &SkillId, version: &str) -> PersResult<()> {
        let touched = self
            .database()
            .connection()
            .execute(
                "DELETE FROM skills WHERE id = ?1 AND version = ?2",
                params![id.as_str(), version],
            )
            .map_err(|error| PersistenceError::Plainly(error.to_string()))?;
        if touched == 0 {
            return Err(PersistenceError::NotFound {
                entity: "skill",
                id: format!("{}@{version}", id.as_str()),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "skill_store_tests.rs"]
mod skill_store_tests;
