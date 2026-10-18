//! The Hierarchy Manager's and the Runtime Manager's persistence: `HierarchyRepository`
//! and `RuntimeInstanceRepository` implemented on `SqliteStore` over the §27.3
//! `hierarchies` / `hierarchy_nodes` / `runtime_instances` tables (P1-05's remaining
//! aggregates — R-PRS-01 keeps the store authoritative for every §27.1 entity).

use crate::error::{PersistenceError, Result as PersResult};
use crate::repository::{HierarchyRepository, RuntimeInstanceRepository};
use athena_types::{
    AgentId, AgentInstanceState, AgentLevel, Hierarchy, HierarchyId, RuntimeInstance,
    RuntimeInstanceId, RuntimeType, ScopeRef, SideEffects, StopReason, TaskId,
};
use rusqlite::params;
use serde_json as json;

const HIERARCHY_COLUMNS: &str = "id, task_id, revision, created_at";
const NODE_COLUMNS: &str = "agent_id, level, superior_agent_id, scope_ref_json, sibling_acl_json";
const RUNTIME_COLUMNS: &str = "id, runtime_type, instance_id, state, pid, last_heartbeat_at, \
started_at, stopped_at, stop_reason, exit_code, side_effects";

/// Decode a `TEXT` column into an enum by its wire form — a stored token that no longer
/// parses is an internal inconsistency (the store's decode discipline, never a guessed enum).
macro_rules! decode_wire {
    ($entity:literal, $column:literal, $raw:expr, $enum_ty:ident) => {{
        $enum_ty::ALL
            .iter()
            .copied()
            .find(|value| value.as_str() == $raw)
            .ok_or_else(|| PersistenceError::Decode {
                entity: $entity,
                column: $column,
                detail: format!("unknown value {:?}", $raw),
            })?
    }};
}

// `AgentId` is a slug, not a ULID — it rehydrates verbatim; the ULID kinds validate
// their prefix via `parse`.
fn parse_hierarchy_id(raw: &str) -> PersResult<HierarchyId> {
    HierarchyId::parse(raw).ok_or_else(|| PersistenceError::Decode {
        entity: "hierarchy",
        column: "id",
        detail: format!("malformed identifier {raw:?}"),
    })
}

fn decode_stop_reason(raw: Option<String>) -> PersResult<Option<StopReason>> {
    raw.map(|text| {
        StopReason::ALL
            .iter()
            .copied()
            .find(|value| value.as_str() == text)
            .ok_or_else(|| PersistenceError::Decode {
                entity: "runtime_instance",
                column: "stop_reason",
                detail: format!("unknown value {text:?}"),
            })
    })
    .transpose()
}

fn row_to_hierarchy(row: &rusqlite::Row<'_>) -> rusqlite::Result<PersResult<Hierarchy>> {
    let id_raw: String = row.get(0)?;
    let task_raw: String = row.get(1)?;
    let revision: i64 = row.get(2)?;
    let created_at: String = row.get(3)?;

    Ok((|| {
        Ok(Hierarchy {
            id: HierarchyId::parse(id_raw.clone()).ok_or_else(|| PersistenceError::Decode {
                entity: "hierarchy",
                column: "id",
                detail: format!("malformed identifier {id_raw:?}"),
            })?,
            task_id: TaskId::parse(task_raw.clone()).ok_or_else(|| PersistenceError::Decode {
                entity: "hierarchy",
                column: "task_id",
                detail: format!("malformed identifier {task_raw:?}"),
            })?,
            revision: revision.max(0) as u64,
            // The node set is fetched separately (`get` re-reads it).
            nodes: Vec::new(),
            created_at,
        })
    })())
}

fn row_to_runtime_instance(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<PersResult<RuntimeInstance>> {
    let id_raw: String = row.get(0)?;
    let runtime_raw: String = row.get(1)?;
    let instance_raw: String = row.get(2)?;
    let state_raw: String = row.get(3)?;
    let pid: Option<i64> = row.get(4)?;
    let heartbeat: Option<String> = row.get(5)?;
    let started_at: String = row.get(6)?;
    let stopped_at: Option<String> = row.get(7)?;
    let stop_reason_raw: Option<String> = row.get(8)?;
    let exit_code: Option<i32> = row.get(9)?;
    let side_raw: String = row.get(10)?;

    Ok((|| {
        Ok(RuntimeInstance {
            id: RuntimeInstanceId::parse(id_raw.clone()).ok_or_else(|| {
                PersistenceError::Decode {
                    entity: "runtime_instance",
                    column: "id",
                    detail: format!("malformed identifier {id_raw:?}"),
                }
            })?,
            runtime_type: decode_wire!(
                "runtime_instance",
                "runtime_type",
                &runtime_raw,
                RuntimeType
            ),
            instance_id: athena_types::InstanceId::parse(instance_raw.clone()).ok_or_else(
                || PersistenceError::Decode {
                    entity: "runtime_instance",
                    column: "instance_id",
                    detail: format!("malformed identifier {instance_raw:?}"),
                },
            )?,
            state: decode_wire!("runtime_instance", "state", &state_raw, AgentInstanceState),
            pid: pid.map(|value| value.max(0) as u32),
            last_heartbeat_at: heartbeat,
            started_at,
            stopped_at,
            stop_reason: decode_stop_reason(stop_reason_raw)?,
            exit_code,
            side_effects: decode_wire!("runtime_instance", "side_effects", &side_raw, SideEffects),
        })
    })())
}

impl HierarchyRepository for crate::SqliteStore {
    fn insert(&mut self, hierarchy: &Hierarchy) -> PersResult<()> {
        let connection = self.database().connection();
        let tx = connection
            .unchecked_transaction()
            .map_err(|error| PersistenceError::Plainly(error.to_string()))?;
        tx.execute(
            "INSERT OR REPLACE INTO hierarchies (id, task_id, revision, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                hierarchy.id.as_str(),
                hierarchy.task_id.as_str(),
                hierarchy.revision as i64,
                hierarchy.created_at,
            ],
        )
        .map_err(|error| PersistenceError::Plainly(error.to_string()))?;
        tx.execute(
            "DELETE FROM hierarchy_nodes WHERE hierarchy_id = ?1",
            params![hierarchy.id.as_str()],
        )
        .map_err(|error| PersistenceError::Plainly(error.to_string()))?;
        for node in &hierarchy.nodes {
            tx.execute(
                "INSERT INTO hierarchy_nodes (hierarchy_id, agent_id, level, superior_agent_id, \
                 scope_ref_json, sibling_acl_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    hierarchy.id.as_str(),
                    node.agent_id.as_str(),
                    node.level.as_str(),
                    node.superior_agent_id.as_ref().map(|id| id.as_str()),
                    node.scope_ref
                        .as_ref()
                        .map(|scope| json::to_string(scope).unwrap_or_else(|_| "null".into())),
                    json::to_string(
                        &node
                            .sibling_acl
                            .iter()
                            .map(|id| id.as_str().to_owned())
                            .collect::<Vec<_>>()
                    )
                    .unwrap_or_else(|_| "[]".into()),
                ],
            )
            .map(|_| ())
            .map_err(|error| PersistenceError::Plainly(error.to_string()))?;
        }
        tx.commit()
            .map_err(|error| PersistenceError::Plainly(error.to_string()))
    }

    fn get(&self, id: &HierarchyId) -> PersResult<Option<Hierarchy>> {
        use rusqlite::OptionalExtension;
        let base = self
            .database()
            .connection()
            .query_row(
                &format!("SELECT {HIERARCHY_COLUMNS} FROM hierarchies WHERE id = ?1"),
                params![id.as_str()],
                row_to_hierarchy,
            )
            .optional()
            .map_err(|error| PersistenceError::Plainly(error.to_string()))?
            .transpose()?;
        let Some(mut hierarchy) = base else {
            return Ok(None);
        };
        let mut statement = self
            .database()
            .connection()
            .prepare(&format!(
                "SELECT {NODE_COLUMNS} FROM hierarchy_nodes WHERE hierarchy_id = ?1 ORDER BY \
                 agent_id"
            ))
            .map_err(|error| PersistenceError::Plainly(error.to_string()))?;
        let rows = statement
            .query_map([id.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .map_err(|error| PersistenceError::Plainly(error.to_string()))?;
        for (agent_raw, level_raw, superior_raw, scope_json, acl_json) in rows {
            let scope_ref: Option<ScopeRef> = scope_json
                .as_deref()
                .filter(|text| *text != "null")
                .map(|text| {
                    json::from_str(text).map_err(|e| PersistenceError::Decode {
                        entity: "hierarchy_node",
                        column: "scope_ref_json",
                        detail: e.to_string(),
                    })
                })
                .transpose()?;
            let sibling_acl: Vec<AgentId> = json::from_str::<Vec<String>>(&acl_json)
                .map_err(|e| PersistenceError::Decode {
                    entity: "hierarchy_node",
                    column: "sibling_acl_json",
                    detail: e.to_string(),
                })?
                .into_iter()
                .map(AgentId::new)
                .collect::<Vec<_>>();
            hierarchy.nodes.push(athena_types::HierarchyNode {
                agent_id: AgentId::new(agent_raw),
                level: decode_wire!("hierarchy_node", "level", &level_raw, AgentLevel),
                superior_agent_id: superior_raw.map(AgentId::new),
                scope_ref,
                sibling_acl,
            });
        }
        Ok(Some(hierarchy))
    }

    fn latest_for_task(&self, task: &TaskId) -> PersResult<Option<Hierarchy>> {
        use rusqlite::OptionalExtension;
        let id_raw: Option<String> = self
            .database()
            .connection()
            .query_row(
                "SELECT id FROM hierarchies WHERE task_id = ?1 ORDER BY revision DESC LIMIT 1",
                params![task.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| PersistenceError::Plainly(error.to_string()))?;
        let Some(id_raw) = id_raw else {
            return Ok(None);
        };
        let id = parse_hierarchy_id(&id_raw)?;
        HierarchyRepository::get(self, &id)
    }
}

impl RuntimeInstanceRepository for crate::SqliteStore {
    fn insert(&mut self, instance: &RuntimeInstance) -> PersResult<()> {
        self.database()
            .connection()
            .execute(
                &format!(
                    "INSERT OR REPLACE INTO runtime_instances ({RUNTIME_COLUMNS})
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"
                ),
                params![
                    instance.id.as_str(),
                    instance.runtime_type.as_str(),
                    instance.instance_id.as_str(),
                    instance.state.as_str(),
                    instance.pid.map(i64::from),
                    instance.last_heartbeat_at,
                    instance.started_at,
                    instance.stopped_at,
                    instance.stop_reason.map(|reason| reason.as_str()),
                    instance.exit_code,
                    instance.side_effects.as_str(),
                ],
            )
            .map(|_| ())
            .map_err(|error| PersistenceError::Plainly(error.to_string()))
    }

    fn get(&self, id: &RuntimeInstanceId) -> PersResult<Option<RuntimeInstance>> {
        use rusqlite::OptionalExtension;
        let row = self
            .database()
            .connection()
            .query_row(
                &format!("SELECT {RUNTIME_COLUMNS} FROM runtime_instances WHERE id = ?1"),
                params![id.as_str()],
                row_to_runtime_instance,
            )
            .optional()
            .map_err(|error| PersistenceError::Plainly(error.to_string()))?;
        row.transpose()
    }

    fn record_heartbeat(&mut self, id: &RuntimeInstanceId, at: &str) -> PersResult<()> {
        let touched = self
            .database()
            .connection()
            .execute(
                "UPDATE runtime_instances SET last_heartbeat_at = ?2 WHERE id = ?1 AND \
                 stopped_at IS NULL",
                params![id.as_str(), at],
            )
            .map_err(|error| PersistenceError::Plainly(error.to_string()))?;
        if touched == 0 {
            return Err(PersistenceError::NotFound {
                entity: "runtime_instance",
                id: id.as_str().to_owned(),
            });
        }
        Ok(())
    }

    fn list_stale(&self, cutoff_rfc3339: &str) -> PersResult<Vec<RuntimeInstance>> {
        let mut statement = self
            .database()
            .connection()
            .prepare(&format!(
                "SELECT {RUNTIME_COLUMNS} FROM runtime_instances
                 WHERE stopped_at IS NULL
                   AND (last_heartbeat_at IS NULL OR last_heartbeat_at < ?1)
                 ORDER BY started_at"
            ))
            .map_err(|error| PersistenceError::Plainly(error.to_string()))?;
        let rows = statement
            .query_map([cutoff_rfc3339], row_to_runtime_instance)
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .map_err(|error| PersistenceError::Plainly(error.to_string()))?;
        rows.into_iter().collect()
    }

    fn mark_stopped(
        &mut self,
        id: &RuntimeInstanceId,
        reason: StopReason,
        exit_code: Option<i32>,
        side_effects: SideEffects,
    ) -> PersResult<()> {
        let touched = self
            .database()
            .connection()
            .execute(
                "UPDATE runtime_instances SET state = ?2, stopped_at = ?3, stop_reason = ?4, \
                 exit_code = ?5, side_effects = ?6
                 WHERE id = ?1 AND stopped_at IS NULL",
                params![
                    id.as_str(),
                    AgentInstanceState::Stopped.as_str(),
                    crate::now_rfc3339(),
                    reason.as_str(),
                    exit_code,
                    side_effects.as_str(),
                ],
            )
            .map_err(|error| PersistenceError::Plainly(error.to_string()))?;
        if touched == 0 {
            return Err(PersistenceError::NotFound {
                entity: "runtime_instance",
                id: id.as_str().to_owned(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "hierarchy_runtime_store_tests.rs"]
mod hierarchy_runtime_store_tests;
