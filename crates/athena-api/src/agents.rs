//! The Agent surface (spec §22.5), the Runtime surface (§22.6) and the §22.9 `GET
//! /events` list (X-31; `R-API-09`): the read half of the endpoint sets the first survey
//! round named absent (`report_next2.md`, repo root) — every row the store already keeps
//! (`AgentRepository`, `HierarchyRepository`, `ToolExecutionRepository`,
//! `ResultRepository`, `RuntimeInstanceRepository`, `MessageRepository`,
//! `EventRepository`) projected through the §22 field maps, cursor-paginated where the
//! spec's route row says a list (`ATH-API-005`). The write half on this surface is only
//! the one spec row that exists — `POST /api/v1/instances/{id}/stop`, the `admin`
//! emergency stop (`ATH-API-024`: an Instance is never *started* over the API).

use crate::errors::ApiError;
use crate::tasks::SharedStore;
use athena_core::{
    AgentRepository, EventRepository, HierarchyRepository, InstanceTrigger, MessageRepository,
    PlanRepository, ResultRepository, RuntimeInstanceRepository, ToolExecutionRepository,
    instance_transition,
};
use athena_types::{AgentId, AgentInstanceState, InstanceId, TaskId};
use axum::{Json, http::StatusCode, response::IntoResponse, response::Response};
use serde_json::{Value, json};

// --- §22.5 Agent definitions ---------------------------------------------------------------

/// One definition serialized per §22.5's field map (`ATH-API-013`: definitions, not
/// instances — the two never conflate).
#[must_use]
pub fn body_definition(definition: &athena_types::AgentDefinition) -> Value {
    json!({
        "id": definition.id.as_str(),
        "name": definition.name,
        "level": definition.level,
        "description": definition.description,
        "capabilities": definition.capabilities,
        "persona_ref": definition.persona_ref,
        "persona_version": definition.persona_version,
        "allowed_tools": definition.allowed_tools,
        "delegable_tools": definition.delegable_tools,
        "default_skills": definition.default_skills,
        "runtime": definition.runtime,
        "model": definition.model,
        "max_delegation_depth": definition.max_delegation_depth,
        "dynamic": definition.dynamic,
    })
}

/// List agent definitions, cursor-paginated (`GET /agents`, viewer).
///
/// # Errors
/// Store faults ⇒ 500.
pub fn list_definitions(
    store: &SharedStore,
    page: &crate::pagination::Query,
) -> Result<Response, ApiError> {
    let guard = store.lock().expect("agent store poisoned");
    let rows =
        AgentRepository::list_definitions(&*guard).map_err(|e| crate::tasks::store_fault(e, ""))?;
    let page = crate::pagination::slice(&rows, page);
    let items: Vec<Value> = page.items.iter().map(body_definition).collect();
    Ok((
        StatusCode::OK,
        Json(json!({ "items": items, "next_cursor": page.next_cursor })),
    )
        .into_response())
}

/// Read one definition (`GET /agents/{id}`, viewer): 404 when absent.
///
/// # Errors
/// Malformed id or absent row ⇒ 404; store faults ⇒ 500.
pub fn read_definition(store: &SharedStore, agent: &str) -> Result<Response, ApiError> {
    let id = definition_id_of(agent)?;
    let guard = store.lock().expect("agent store poisoned");
    let found = AgentRepository::get_definition(&*guard, &id)
        .map_err(|e| crate::tasks::store_fault(e, ""))?;
    match found {
        Some(row) => Ok((StatusCode::OK, Json(body_definition(&row))).into_response()),
        None => Err(absent_agent(id.as_str())),
    }
}

/// One instance serialized per §22.5's field map.
#[must_use]
pub fn body_instance(instance: &athena_types::AgentInstance) -> Value {
    json!({
        "id": instance.id.as_str(),
        "agent_id": instance.agent_id.as_str(),
        "task_id": instance.task_id.as_ref().map(TaskId::as_str),
        "plan_step_id": instance.plan_step_id.as_ref().map(|s| s.as_str()),
        "superior_instance_id": instance.superior_instance_id.as_ref().map(|i| i.as_str()),
        "delegation_depth": instance.delegation_depth,
        "state": instance.state,
        "runtime_instance_id": instance.runtime_instance_id.as_ref().map(|i| i.as_str()),
        "workspace": instance.workspace,
    })
}

/// List agent instances, cursor-paginated (`GET /instances`, viewer).
///
/// # Errors
/// Store faults ⇒ 500.
pub fn list_instances(
    store: &SharedStore,
    page: &crate::pagination::Query,
) -> Result<Response, ApiError> {
    let guard = store.lock().expect("agent store poisoned");
    let rows = AgentRepository::list_all_instances(&*guard)
        .map_err(|e| crate::tasks::store_fault(e, ""))?;
    let page = crate::pagination::slice(&rows, page);
    let items: Vec<Value> = page.items.iter().map(body_instance).collect();
    Ok((
        StatusCode::OK,
        Json(json!({ "items": items, "next_cursor": page.next_cursor })),
    )
        .into_response())
}

/// Read one instance (`GET /instances/{id}`, viewer): 404 when absent.
///
/// # Errors
/// Malformed id or absent row ⇒ 404; store faults ⇒ 500.
pub fn read_instance(store: &SharedStore, instance: &str) -> Result<Response, ApiError> {
    let id = instance_id_of(instance)?;
    let guard = store.lock().expect("agent store poisoned");
    let found = AgentRepository::get_instance(&*guard, &id)
        .map_err(|e| crate::tasks::store_fault(e, ""))?;
    match found {
        Some(row) => Ok((StatusCode::OK, Json(body_instance(&row))).into_response()),
        None => Err(absent_instance(id.as_str())),
    }
}

/// Messages addressed to one instance (`GET /instances/{id}/messages`, viewer): the
/// §26.6 `MessageRow`s of the instance's own delivery face, cursor-paginated. A
/// well-formed but absent instance is 404 — the same face the definition row gives.
///
/// # Errors
/// Malformed id or absent row ⇒ 404; store faults ⇒ 500.
pub fn list_instance_messages(
    store: &SharedStore,
    instance: &str,
    page: &crate::pagination::Query,
) -> Result<Response, ApiError> {
    let id = instance_id_of(instance)?;
    let guard = store.lock().expect("agent store poisoned");
    AgentRepository::get_instance(&*guard, &id)
        .map_err(|e| crate::tasks::store_fault(e, ""))?
        .ok_or_else(|| absent_instance(id.as_str()))?;
    // The instance's messages ride the thread key the Broker addressed it by
    // (`ATH-MSG-004`/`013`): the instance row's own message scan is the thread face —
    // the store's message table keyed `to_instance_id` — walked here through the
    // thread string the instances mint. Until a dedicated instance scan exists, the
    // honest face is the thread named by the instance id.
    let thread = id.as_str().to_string();
    let rows = MessageRepository::list_for_thread(&*guard, &thread)
        .map_err(|e| crate::tasks::store_fault(e, ""))?;
    let page = crate::pagination::slice(&rows, page);
    let items: Vec<Value> = page
        .items
        .iter()
        .map(|message| {
            json!({
                "message_id": message.message_id.as_str(),
                "message_type": message.message_type,
                "from_agent": message.from_agent.as_str(),
                "to_agent": message.to_agent.as_str(),
                "from_instance_id": message.from_instance_id.as_str(),
                "task_id": message.task_id.as_ref().map(TaskId::as_str),
                "thread_id": message.thread_id,
                "priority": message.priority,
                "body": message.body,
                "created_at": message.created_at,
            })
        })
        .collect();
    Ok((
        StatusCode::OK,
        Json(json!({ "items": items, "next_cursor": page.next_cursor })),
    )
        .into_response())
}

/// The `admin` emergency stop (`POST /instances/{id}/stop`): `StopRequested` through the
/// §17.2 instance table (`Stopping`, event `AgentStopping`) — never a fabricated
/// terminal state; the Runtime's own exit path records `AgentStopped` afterwards. The
/// spec's only write face on this group (`ATH-API-024`).
///
/// # Errors
/// Malformed id ⇒ 404; absent row ⇒ 404; a state the table refuses ⇒ 409
/// (`ValidationError`); store faults ⇒ 500.
pub fn stop_instance(
    store: &SharedStore,
    instance: &str,
    now: &str,
    correlation_id: &str,
) -> Result<Response, ApiError> {
    let id = instance_id_of(instance)?;
    let mut guard = store.lock().expect("agent store poisoned");
    let row = AgentRepository::get_instance(&*guard, &id)
        .map_err(|e| crate::tasks::store_fault(e, correlation_id))?
        .ok_or_else(|| absent_instance(id.as_str()))?;
    if matches!(
        row.state,
        AgentInstanceState::Stopped | AgentInstanceState::Failed
    ) {
        return Err(ApiError::of_status(
            athena_core::protocol::ErrorClass::ValidationError,
            StatusCode::CONFLICT,
            "INSTANCE_TERMINAL",
            format!("the instance is already {}", row.state),
            json!({}),
            correlation_id.to_string(),
        ));
    }
    let event = athena_core::NewEvent {
        event_id: athena_types::EventId::new(),
        schema_version: 1,
        event_type: "AgentStopping".into(),
        occurred_at: now.to_string(),
        task_id: row.task_id.clone(),
        plan_step_id: row.plan_step_id.clone(),
        agent_id: Some(row.agent_id.clone()),
        instance_id: Some(id.clone()),
        skill_id: None,
        correlation_id: correlation_id.to_string(),
        causation_id: None,
        actor: "human".into(),
        payload_json: json!({
            "instance_id": id.as_str(),
            "reason": "emergency stop (ATH-API-024)",
        })
        .to_string(),
    };
    let resolved = instance_transition(row.state, InstanceTrigger::StopRequested);
    match resolved {
        Some(to) => {
            AgentRepository::update_instance_state(&mut *guard, &id, to.to)
                .map_err(|e| crate::tasks::store_fault(e, correlation_id))?;
            EventRepository::append(&mut *guard, &event)
                .map_err(|e| crate::tasks::store_fault(e, correlation_id))?;
        }
        None => {
            return Err(ApiError::of_status(
                athena_core::protocol::ErrorClass::ValidationError,
                StatusCode::CONFLICT,
                "INSTANCE_TRANSITION_REFUSED",
                format!("state {} does not accept StopRequested", row.state),
                json!({}),
                correlation_id.to_string(),
            ));
        }
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "id": id.as_str(),
            "state": AgentInstanceState::Stopping,
        })),
    )
        .into_response())
}

// --- §22.5 Hierarchy / Tool executions / Evaluations ----------------------------------------

/// The Task's latest composed hierarchy (`GET /hierarchies/{task_id}`, viewer): the
/// §26.6 rows (agent, level, superior, scope) — 404 when the Task never composed one.
///
/// # Errors
/// Malformed id ⇒ 404; absent Task or absent hierarchy ⇒ 404; store faults ⇒ 500.
pub fn read_hierarchy(store: &SharedStore, task: &str) -> Result<Response, ApiError> {
    let task_id = task_id_of(task)?;
    let guard = store.lock().expect("agent store poisoned");
    task_present(&guard, &task_id)?;
    let found = HierarchyRepository::latest_for_task(&*guard, &task_id)
        .map_err(|e| crate::tasks::store_fault(e, ""))?;
    let Some(hierarchy) = found else {
        return Err(ApiError::of_status(
            athena_core::protocol::ErrorClass::ValidationError,
            StatusCode::NOT_FOUND,
            "HIERARCHY_NOT_FOUND",
            format!("task {} has no composed hierarchy", task_id.as_str()),
            json!({}),
            String::new(),
        ));
    };
    let nodes: Vec<Value> = hierarchy
        .nodes
        .iter()
        .map(|node| {
            json!({
                "agent_id": node.agent_id.as_str(),
                "level": node.level,
                "superior_agent_id": node.superior_agent_id.as_ref().map(AgentId::as_str),
                "scope_ref": node.scope_ref,
                "sibling_acl": node.sibling_acl.iter().map(AgentId::as_str).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok((
        StatusCode::OK,
        Json(json!({
            "id": hierarchy.id.as_str(),
            "task_id": hierarchy.task_id.as_str(),
            "revision": hierarchy.revision,
            "created_at": hierarchy.created_at,
            "nodes": nodes,
        })),
    )
        .into_response())
}

/// Tool execution records (`GET /tool-executions`, viewer), cursor-paginated — newest
/// first, the insert order the table keeps.
///
/// # Errors
/// Store faults ⇒ 500.
pub fn list_tool_executions(
    store: &SharedStore,
    page: &crate::pagination::Query,
) -> Result<Response, ApiError> {
    let guard = store.lock().expect("agent store poisoned");
    let rows = tool_rows(&guard)?;
    let page = crate::pagination::slice(&rows, page);
    let items: Vec<Value> = page
        .items
        .iter()
        .map(|row| {
            json!({
                "id": row.id.as_str(),
                "instance_id": row.instance_id.as_str(),
                "task_id": row.task_id.as_ref().map(TaskId::as_str),
                "tool": row.tool,
                "operation_descriptor": row.operation_descriptor,
                "risk_level": row.risk_level,
                "approval_id": row.approval_id.as_ref().map(|a| a.as_str()),
                "status": row.status,
                "started_at": row.started_at,
                "finished_at": row.finished_at,
            })
        })
        .collect();
    Ok((
        StatusCode::OK,
        Json(json!({ "items": items, "next_cursor": page.next_cursor })),
    )
        .into_response())
}

/// Evaluation records (`GET /evaluations`, viewer), cursor-paginated — every recorded
/// verdict over every Result (`ATH-EVAL-003`'s records are read here).
///
/// # Errors
/// Store faults ⇒ 500.
pub fn list_evaluations(
    store: &SharedStore,
    page: &crate::pagination::Query,
) -> Result<Response, ApiError> {
    let guard = store.lock().expect("agent store poisoned");
    let rows = evaluation_rows(&guard)?;
    let page = crate::pagination::slice(&rows, page);
    let items: Vec<Value> = page
        .items
        .iter()
        .map(|row| {
            json!({
                "id": row.id.as_str(),
                "task_id": row.task_id.as_str(),
                "result_id": row.result_id.as_str(),
                "evaluator": row.evaluator,
                "method": row.method,
                "verdict": row.verdict,
                "rationale": row.rationale,
                "created_at": row.created_at,
            })
        })
        .collect();
    Ok((
        StatusCode::OK,
        Json(json!({ "items": items, "next_cursor": page.next_cursor })),
    )
        .into_response())
}

// --- §22.6 Runtime instances ----------------------------------------------------------------

/// One runtime instance serialized per §22.6's field map.
#[must_use]
pub fn body_runtime(instance: &athena_types::RuntimeInstance) -> Value {
    json!({
        "id": instance.id.as_str(),
        "runtime_type": instance.runtime_type,
        "instance_id": instance.instance_id.as_str(),
        "state": instance.state,
        "pid": instance.pid,
        "last_heartbeat_at": instance.last_heartbeat_at,
        "started_at": instance.started_at,
        "stopped_at": instance.stopped_at,
        "stop_reason": instance.stop_reason,
        "exit_code": instance.exit_code,
        "side_effects": instance.side_effects,
    })
}

/// List runtime instances (`GET /runtimes`, viewer): the §14.5 rows (the process face
/// every Agent Instance rides one-to-one, `ATH-RT-012`), cursor-paginated.
///
/// # Errors
/// Store faults ⇒ 500.
pub fn list_runtimes(
    store: &SharedStore,
    page: &crate::pagination::Query,
) -> Result<Response, ApiError> {
    let guard = store.lock().expect("runtime store poisoned");
    let rows = runtime_rows(&guard)?;
    let page = crate::pagination::slice(&rows, page);
    let items: Vec<Value> = page.items.iter().map(body_runtime).collect();
    Ok((
        StatusCode::OK,
        Json(json!({ "items": items, "next_cursor": page.next_cursor })),
    )
        .into_response())
}

/// Read one runtime instance (`GET /runtimes/{id}`, viewer): 404 when absent.
///
/// # Errors
/// Malformed id ⇒ 404; absent row ⇒ 404; store faults ⇒ 500.
pub fn read_runtime(store: &SharedStore, runtime: &str) -> Result<Response, ApiError> {
    let guard = store.lock().expect("runtime store poisoned");
    let rows = runtime_rows(&guard)?;
    let found = rows.into_iter().find(|row| row.id.as_str() == runtime);
    match found {
        Some(row) => Ok((StatusCode::OK, Json(body_runtime(&row))).into_response()),
        None => Err(ApiError::of_status(
            athena_core::protocol::ErrorClass::ValidationError,
            StatusCode::NOT_FOUND,
            "RUNTIME_NOT_FOUND",
            format!("no runtime instance {runtime:?}"),
            json!({}),
            String::new(),
        )),
    }
}

// --- §22.9 Events list ----------------------------------------------------------------------

/// List Events (`GET /events`, viewer): the §23 rows newest first, cursor-paginated;
/// `?task_id=` narrows to one Task's replay, `?type=` (one name) and `?since=` (the
/// stream's gap anchor, `ATH-EVENT-020`, in list form) narrow the rows per
/// §22.9 (X-38: the narrowing keys are read by `athena_core::event_filter`). Payload
/// contents are never rendered (`ATH-EVENT-007`): names and stamps only.
///
/// # Errors
/// Unknown `task_id` shape ⇒ 400; store faults ⇒ 500.
pub fn list_events(
    store: &SharedStore,
    page: &crate::pagination::Query,
    task_id: Option<&str>,
    types: Option<&str>,
    since: Option<u64>,
) -> Result<Response, ApiError> {
    let guard = store.lock().expect("event store poisoned");
    let filter_parts: Vec<(String, String)> = task_id
        .map(|raw| vec![(String::from("task_id"), raw.to_string())])
        .unwrap_or_default();
    let mut filter_parts = filter_parts;
    if let Some(wire) = types {
        filter_parts.push((String::from("types"), wire.to_string()));
    }
    if let Some(anchor) = since {
        // `?since=` rides the filter itself (the sequence anchor, `ATH-EVENT-020`);
        // the row walk then answers `sequence > since`, list-wise.
        filter_parts.push((String::from("since"), anchor.to_string()));
    }
    let filter = athena_core::event_filter::EventFilter::parse(&filter_parts);
    let rows = match task_id {
        Some(raw) => {
            let task_id = TaskId::parse(raw).ok_or_else(|| {
                ApiError::of_status(
                    athena_core::protocol::ErrorClass::ValidationError,
                    StatusCode::BAD_REQUEST,
                    "TASK_NOT_FOUND",
                    "malformed task id on the filter",
                    json!({}),
                    String::new(),
                )
            })?;
            task_present(&guard, &task_id)?;
            EventRepository::list_for_task(&*guard, &task_id)
                .map_err(|e| crate::tasks::store_fault(e, ""))?
        }
        None => {
            // The stream's own semantics, list form: `?since=` (§22.9) anchors the gap
            // (ATH-EVENT-020), the cursor pages the filtered answer; without either the
            // whole append-only history walks oldest → newest under the cursor.
            let since = filter.since.or(page.cursor).unwrap_or(0);
            EventRepository::list_since(&*guard, since, u64::MAX)
                .map_err(|e| crate::tasks::store_fault(e, ""))?
        }
    };
    // The §22.9 narrowing keys apply to the row set before the page slice (X-38);
    // the cursor indexes the *filtered* order, so a walk that changes the filter
    // changes what the cursors mean — the response always says what to ask next.
    let rows: Vec<_> = rows
        .into_iter()
        .filter(|event| filter.matches(event))
        .collect();
    // Newest first, matching every other list face; the cursor indexes into that order.
    let mut rows = rows;
    rows.reverse();
    let page = crate::pagination::slice(&rows, page);
    let items: Vec<Value> = page
        .items
        .iter()
        .map(|event| {
            json!({
                "event_id": event.event_id.as_str(),
                "type": event.event_type,
                "occurred_at": event.occurred_at,
                "sequence": event.sequence,
                "task_id": event.task_id.as_ref().map(TaskId::as_str),
                "correlation_id": event.correlation_id,
            })
        })
        .collect();
    Ok((
        StatusCode::OK,
        Json(json!({ "items": items, "next_cursor": page.next_cursor })),
    )
        .into_response())
}

// --- §22.3 Task-side read faces (plan / results / messages) ----------------------------------

/// The Task's latest Plan (`GET /tasks/{id}/plan`, viewer): the steps and their
/// dependencies (`?revision=` names an earlier revision; `ATH-API-013`: read-only).
///
/// # Errors
/// Malformed id / absent Task ⇒ 404; absent Plan (none submitted) ⇒ 404; store faults
/// ⇒ 500.
pub fn read_task_plan(
    store: &SharedStore,
    task: &str,
    revision: Option<u64>,
) -> Result<Response, ApiError> {
    let task_id = task_id_of(task)?;
    let guard = store.lock().expect("plan store poisoned");
    task_present(&guard, &task_id)?;
    let found = match revision {
        Some(number) => PlanRepository::revision_for_task(&*guard, &task_id, number)
            .map_err(|e| crate::tasks::store_fault(e, ""))?,
        None => PlanRepository::latest_for_task(&*guard, &task_id)
            .map_err(|e| crate::tasks::store_fault(e, ""))?,
    };
    let Some(plan) = found else {
        return Err(ApiError::of_status(
            athena_core::protocol::ErrorClass::ValidationError,
            StatusCode::NOT_FOUND,
            "PLAN_NOT_FOUND",
            format!("task {} has no submitted plan", task_id.as_str()),
            json!({}),
            String::new(),
        ));
    };
    let steps: Vec<Value> = plan
        .steps
        .iter()
        .map(|step| {
            json!({
                "id": step.id.as_str(),
                "intent": step.intent,
                "state": step.state,
                "depends_on": step.depends_on.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                "assigned_agent": step.assigned_agent.as_ref().map(|a| a.as_str()),
                "skill_set": step.skill_set,
            })
        })
        .collect();
    Ok((
        StatusCode::OK,
        Json(json!({
            "id": plan.id.as_str(),
            "task_id": plan.task_id.as_str(),
            "revision": plan.revision,
            "goal": plan.goal,
            "steps": steps,
        })),
    )
        .into_response())
}

/// The Task's Results (`GET /tasks/{id}/results`, viewer): the immutable §19 rows and
/// their artifact summaries, cursor-paginated.
///
/// # Errors
/// Malformed id / absent Task ⇒ 404; store faults ⇒ 500.
pub fn list_task_results(
    store: &SharedStore,
    task: &str,
    page: &crate::pagination::Query,
) -> Result<Response, ApiError> {
    let task_id = task_id_of(task)?;
    let guard = store.lock().expect("result store poisoned");
    task_present(&guard, &task_id)?;
    let rows = ResultRepository::list_for_task(&*guard, &task_id)
        .map_err(|e| crate::tasks::store_fault(e, ""))?;
    let page = crate::pagination::slice(&rows, page);
    let items: Vec<Value> = page
        .items
        .iter()
        .map(|row: &athena_types::ExecutionResult| {
            json!({
                "id": row.id.as_str(),
                "task_id": row.task_id.as_str(),
                "status": row.status,
                "output": row.output,
                "started_at": row.started_at,
                "finished_at": row.finished_at,
                "execution_time_ms": row.execution_time_ms,
                "artifacts": row.artifacts.iter().map(|a| a.path.clone()).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok((
        StatusCode::OK,
        Json(json!({ "items": items, "next_cursor": page.next_cursor })),
    )
        .into_response())
}

/// The Task's conversation utterances (`GET /tasks/{id}/messages`, viewer): the human ↔
/// Core Agent dialogue rides the §10 conversation rows (`ATH-API-021`), cursor-paginated.
///
/// # Errors
/// Malformed id / absent Task ⇒ 404; store faults ⇒ 500.
pub fn list_task_messages(
    store: &SharedStore,
    task: &str,
    page: &crate::pagination::Query,
) -> Result<Response, ApiError> {
    let task_id = task_id_of(task)?;
    let guard = store.lock().expect("message store poisoned");
    task_present(&guard, &task_id)?;
    // The delegation thread is one whole Task life (`ATH-MSG-004`); the Task's messages
    // are the ones whose thread carries the Task's id-shaped key the loop minted.
    let thread = athena_core::core_loop::core_thread(&task_id);
    let rows = MessageRepository::list_for_thread(&*guard, &thread)
        .map_err(|e| crate::tasks::store_fault(e, ""))?;
    let page = crate::pagination::slice(&rows, page);
    let items: Vec<Value> = page
        .items
        .iter()
        .map(|message| {
            json!({
                "message_id": message.message_id.as_str(),
                "message_type": message.message_type,
                "from_agent": message.from_agent.as_str(),
                "to_agent": message.to_agent.as_str(),
                "task_id": message.task_id.as_ref().map(TaskId::as_str),
                "thread_id": message.thread_id,
                "body": message.body,
                "created_at": message.created_at,
            })
        })
        .collect();
    Ok((
        StatusCode::OK,
        Json(json!({ "items": items, "next_cursor": page.next_cursor })),
    )
        .into_response())
}

// --- row helpers -------------------------------------------------------------------------------

/// Task existence under an already-held store guard — the same 404 face
/// [`crate::tasks::read_task`] gives, without re-locking the shared `Mutex` (std
/// `Mutex` is not reentrant: locking it again over the same thread deadlocks the
/// whole runtime's accept loop, the freeze the first live probe recorded).
pub(crate) fn task_present(
    store: &athena_core::AthenaStore,
    task_id: &TaskId,
) -> Result<(), ApiError> {
    match athena_core::TaskRepository::get(store, task_id)
        .map_err(|e| crate::tasks::store_fault(e, ""))?
    {
        Some(_) => Ok(()),
        None => Err(ApiError::of_status(
            athena_core::protocol::ErrorClass::ValidationError,
            StatusCode::NOT_FOUND,
            "TASK_NOT_FOUND",
            format!("no Task {task_id:?}"),
            json!({}),
            String::new(),
        )),
    }
}

fn tool_rows(
    store: &athena_core::AthenaStore,
) -> Result<Vec<athena_types::ToolExecution>, ApiError> {
    // The store keeps insert order; the full scan is the list face for a table the Task
    // lifecycle writes rarely (the §11 execution records).
    let mut rows: Vec<athena_types::ToolExecution> = Vec::new();
    for instance in
        AgentRepository::list_all_instances(store).map_err(|e| crate::tasks::store_fault(e, ""))?
    {
        for execution in ToolExecutionRepository::list_for_instance(store, &instance.id)
            .map_err(|e| crate::tasks::store_fault(e, ""))?
        {
            rows.push(execution);
        }
    }
    // Newest first: started_at descending, then id for stability.
    rows.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then(a.id.as_str().cmp(b.id.as_str()))
    });
    Ok(rows)
}

fn evaluation_rows(
    store: &athena_core::AthenaStore,
) -> Result<Vec<athena_types::Evaluation>, ApiError> {
    let mut rows: Vec<athena_types::Evaluation> = Vec::new();
    let tasks = athena_core::TaskRepository::list_all(store)
        .map_err(|e| crate::tasks::store_fault(e, ""))?;
    for task in tasks {
        for result in ResultRepository::list_for_task(store, &task.id)
            .map_err(|e| crate::tasks::store_fault(e, ""))?
        {
            rows.extend(
                ResultRepository::list_evaluations_for_result(store, &result.id)
                    .map_err(|e| crate::tasks::store_fault(e, ""))?,
            );
        }
    }
    // Newest first: created_at descending, then id for stability.
    rows.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then(a.id.as_str().cmp(b.id.as_str()))
    });
    Ok(rows)
}

fn runtime_rows(
    store: &athena_core::AthenaStore,
) -> Result<Vec<athena_types::RuntimeInstance>, ApiError> {
    let mut rows: Vec<athena_types::RuntimeInstance> = Vec::new();
    for instance in
        AgentRepository::list_all_instances(store).map_err(|e| crate::tasks::store_fault(e, ""))?
    {
        if let Some(runtime_id) = &instance.runtime_instance_id
            && let Some(runtime) = RuntimeInstanceRepository::get(store, runtime_id)
                .map_err(|e| crate::tasks::store_fault(e, ""))?
        {
            rows.push(runtime);
        }
    }
    Ok(rows)
}

// --- id helpers ---------------------------------------------------------------------------------

fn definition_id_of(agent: &str) -> Result<AgentId, ApiError> {
    // The definition id is a persona-derived slug, not a ULID (§26.1): the row's own
    // spelling is the accepted form.
    Ok(AgentId::new(agent))
}

fn instance_id_of(instance: &str) -> Result<InstanceId, ApiError> {
    InstanceId::parse(instance).ok_or_else(|| absent_instance(instance))
}

fn task_id_of(task: &str) -> Result<TaskId, ApiError> {
    TaskId::parse(task).ok_or_else(|| {
        ApiError::of_status(
            athena_core::protocol::ErrorClass::ValidationError,
            StatusCode::NOT_FOUND,
            "TASK_NOT_FOUND",
            "malformed task id on the path",
            json!({}),
            String::new(),
        )
    })
}

fn absent_agent(id: &str) -> ApiError {
    ApiError::of_status(
        athena_core::protocol::ErrorClass::ValidationError,
        StatusCode::NOT_FOUND,
        "AGENT_NOT_FOUND",
        format!("no agent definition {id:?}"),
        json!({}),
        String::new(),
    )
}

fn absent_instance(id: &str) -> ApiError {
    ApiError::of_status(
        athena_core::protocol::ErrorClass::ValidationError,
        StatusCode::NOT_FOUND,
        "INSTANCE_NOT_FOUND",
        format!("no agent instance {id:?}"),
        json!({}),
        String::new(),
    )
}
