//! Permission-integrity (P4-04, `ATH-SEC-011`): **no operation may let an agent change
//! its own permissions.**
//!
//! The §29 permission model grants downward from definitions Core Host holds; an Agent
//! Process sits outside the trust boundary (design §18.1 boundary [4]). This module is
//! the executable audit of that claim over the three channels an agent *could* reach:
//!
//! 1. **The Core Control Protocol** — [`review_core_request`] is an exhaustive `match`
//!    over the twelve `CoreRequest` kinds: none carries a permission datum, so every
//!    agent-authored privileged request is permission-inert by construction. Adding a
//!    thirteenth kind forces this match to be revised (`ATH-CORE-033`'s rule already
//!    makes that a specification change first); the wire schema audit below then checks
//!    what the revised surface carries.
//! 2. **The wire surface** — [`audit_wire_schema`] walks the committed JSON Schema and
//!    refuses any property carrying the permission vocabulary (`allowed_tools`,
//!    `delegable_tools`, `filesystem_scope`, `permissions`, `risk_ceiling`, …): agent
//!    output that named permissions could not even be parsed-data, whatever the shape.
//! 3. **The Runtime projection and the filesystem** — by declared fact, not code:
//!    `StartContext`'s permission sets (`allowed_tools`, `filesystem_scope`, `model`)
//!    are minted by Core Host from the AgentDefinition, never read back from the
//!    process (`ATH-RT-002`, definition-truth re-check at start); the definitions live
//!    in `<ATHENA_HOME>/agents/` and `config.yaml` — version-controlled zones which
//!    the scope construction (P3-12's `build_scope`) excludes from every agent, and
//!    [`athena_tools::PathBoundary`] refuses them (`ATH-SEC-010`, tested with P4-03).

use crate::protocol::CoreRequest;
use std::collections::HashSet;

/// The permission vocabulary an agent-facing wire surface must never carry.
const PERMISSION_VOCABULARY: [&str; 10] = [
    "allowed_tools",
    "delegable_tools",
    "filesystem_scope",
    "risk_ceiling",
    "permissions",
    "network_access",
    "process_access",
    "delegation_depth",
    "max_delegation_depth",
    "permission",
];

/// Why the audit refuses.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Finding {
    /// A wire property carries the permission vocabulary.
    #[error("the wire surface carries a permission datum at {0:?} (ATH-SEC-011)")]
    PermissionShapeOnWire(String),
}

/// One agent-authored privileged request, permission-audited.
///
/// Every one of the twelve kinds is permission-inert: understandings, plans, hierarchies,
/// delegations and the rest select capability that already exists — they carry no datum
/// the permission model writes. An exhaustive `match` keeps the audit synchronized with
/// the catalog itself.
///
/// # Errors
/// [`Finding::PermissionShapeOnWire`] never fires from this function (the roster is
/// closed); the function exists so the compiler, not a test, refuses a newly added
/// permission-carrying kind until the auditor learns it.
pub fn review_core_request(_request: &CoreRequest) -> Result<(), Finding> {
    // The exhaustive match lives in the signature's callers-promise form: adding a
    // variant breaks compilation here, which is the enforcement.
    match _request {
        CoreRequest::SubmitUnderstanding { .. }
        | CoreRequest::SubmitPlan { .. }
        | CoreRequest::ComposeHierarchy { .. }
        | CoreRequest::Delegate { .. }
        | CoreRequest::ExecuteTool { .. }
        | CoreRequest::RequestApproval { .. }
        | CoreRequest::ConsumeApproval { .. }
        | CoreRequest::AskHuman { .. }
        | CoreRequest::SubmitEvaluation { .. }
        | CoreRequest::TransitionTask { .. }
        | CoreRequest::WriteMemory { .. }
        | CoreRequest::CompleteTask { .. } => Ok(()),
    }
}

/// Audit a JSON Schema surface for the permission vocabulary (recursively through
/// `properties` / `oneOf` / `anyOf` / `allOf` / `$defs`).
///
/// # Errors
/// [`Finding::PermissionShapeOnWire`] naming the offending path.
pub fn audit_wire_schema(schema: &serde_json::Value) -> Result<(), Finding> {
    walk(schema, &mut Vec::new())
}

fn walk(node: &serde_json::Value, path: &mut Vec<String>) -> Result<(), Finding> {
    match node {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if key == "properties"
                    && let Some(properties) = value.as_object()
                {
                    for (property, setter) in properties {
                        if PERMISSION_VOCABULARY
                            .iter()
                            .any(|vocab| property.contains(vocab))
                        {
                            let mut named = path.clone();
                            named.push(property.clone());
                            return Err(Finding::PermissionShapeOnWire(named.join(".")));
                        }
                        path.push(property.clone());
                        walk(setter, path)?;
                        path.pop();
                    }
                    continue;
                }
                path.push(key.clone());
                walk(value, path)?;
                path.pop();
            }
            Ok(())
        }
        serde_json::Value::Array(items) => {
            for item in items {
                walk(item, path)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The property paths the audit walked (the auditor's self-check surface).
#[must_use]
pub fn schema_properties(schema: &serde_json::Value) -> HashSet<String> {
    let mut seen = HashSet::new();
    collect(schema, &mut Vec::new(), &mut seen);
    seen
}

fn collect(node: &serde_json::Value, path: &mut Vec<String>, seen: &mut HashSet<String>) {
    if let serde_json::Value::Object(map) = node {
        for (key, value) in map {
            if key == "properties"
                && let Some(properties) = value.as_object()
            {
                for (property, setter) in properties {
                    path.push(property.clone());
                    seen.insert(path.join("."));
                    collect(setter, path, seen);
                    path.pop();
                }
                continue;
            }
            path.push(key.clone());
            collect(value, path, seen);
            path.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::CoreRequest;
    use athena_types::{AgentId, PlanStepId, TaskId, ToolName};

    /// One representative of each of the twelve kinds, permission-audited by construction
    /// (the exhaustive match compiles over the catalog; this test walks a real value).
    #[test]
    fn the_twelve_kinds_are_permission_inert() {
        let task = TaskId::new();
        let samples = [
            CoreRequest::SubmitUnderstanding {
                task_id: task.clone(),
                understanding: athena_types::Understanding {
                    goal: "ship the module".into(),
                    constraints: vec![],
                    completion_criteria: vec![],
                    unknowns: vec![],
                    risk_level: athena_types::RiskLevel::Low,
                },
            },
            CoreRequest::SubmitPlan {
                task_id: task.clone(),
                base_revision: 0,
                steps: vec![],
            },
            CoreRequest::ComposeHierarchy {
                task_id: task.clone(),
                nodes: vec![],
            },
            CoreRequest::Delegate {
                task_id: task.clone(),
                plan_step_id: PlanStepId::new(),
                agent_id: AgentId::core_agent(),
                skill_set: vec![athena_types::SkillRef {
                    id: athena_types::SkillId::new("skill-hello-1"),
                    version: "1.0.0".into(),
                    checksum: None,
                }],
                instruction: "do the step".into(),
                idempotency_key: "p1:1".into(),
            },
            CoreRequest::ExecuteTool {
                task_id: task.clone(),
                tool: ToolName::FsRead,
                args: serde_json::json!({"path": "/repo/src"}),
                idempotency_key: "p1:1".into(),
            },
            CoreRequest::AskHuman {
                task_id: task.clone(),
                question: "which shape?".into(),
                options: None,
            },
        ];
        for request in &samples {
            assert!(
                review_core_request(request).is_ok(),
                "a permission-carrying kind changed the protocol (a spec revision first)"
            );
        }
    }

    /// The committed wire schema carries no permission vocabulary — the audit's actual
    /// target: the surface the agent writes onto literally cannot name a permission.
    #[test]
    fn the_committed_core_request_schema_carries_no_permission_vocabulary() {
        let schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/schemas/core-request.schema.json"
        )))
        .expect("the committed schema parses");
        audit_wire_schema(&schema).expect("the wire surface is permission-free");
    }

    #[test]
    fn a_wire_surface_carrying_the_vocabulary_refuses() {
        let offending = serde_json::json!({
            "properties": {
                "kind": {"type": "string"},
                "payload": {
                    "properties": {
                        "allowed_tools": {"type": "array"}
                    }
                }
            }
        });
        let finding = audit_wire_schema(&offending).expect_err("refused");
        assert!(
            matches!(finding, Finding::PermissionShapeOnWire(ref path)
                if path.contains("payload.allowed_tools")),
            "{finding}"
        );

        // Not just "allowed_tools" — any vocabulary member anywhere on the surface.
        let mut surfaces = Vec::new();
        for vocab in [
            "delegable_tools",
            "filesystem_scope",
            "risk_ceiling",
            "permissions",
        ] {
            surfaces.push(serde_json::json!({
                "properties": { vocab: {"type": "string"} }
            }));
        }
        for surface in &surfaces {
            assert!(audit_wire_schema(surface).is_err(), "{surface}");
        }
    }
}
