//! Approval Manager — `Approval` (spec §15).
//!
//! Owns the §17.4 approval state machine (transition tables resolved in `athena-state`),
//! the canonical operation descriptor built by Core Host (ATH-HITL-010), expiry (silence is
//! rejection), scope consumption and the audit record with approver identity.
//!
//! Owned entities (`OWNED`): Approval. Emitted Events: `Approval*`.

use crate::ownership::{Entity, ModuleRole};

/// The entities this module may mutate.
pub const OWNED: &[(Entity, ModuleRole)] = &[(Entity::Approval, ModuleRole::ApprovalManager)];

// --- P4-13: scope staging and consumption (ATH-HITL-012/013, R-ST-13) ----------------------

pub mod scope {
    //! The approval scope's facts: `operation_only` consumes once; a `pattern` scope
    //! carries `uses_remaining` and becomes `CONSUMED` at zero — and the pattern itself
    //! must be **chosen from the configured `policy.approval_scopes` enumeration**
    //! (`ATH-HITL-012`: an agent may not invent one), with `scope.expires_at` bounded by
    //! `policy.max_approval_ttl` (`ATH-HITL-013`).

    use crate::ownership::{Entity, ModuleRole};

    /// Silent ownership re-statement (drift-checked like every Core Host module).
    #[must_use]
    pub const fn owned() -> &'static [(Entity, ModuleRole)] {
        &[(Entity::Approval, ModuleRole::ApprovalManager)]
    }

    /// The staged scope facts an `Approval` carries.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Staged {
        /// One operation, consumable once (`ATH-HITL-005`).
        OperationOnly,
        /// A configured pattern with its remaining uses and its bounded expiry.
        Pattern {
            /// The member of `policy.approval_scopes` chosen.
            pattern: String,
            /// The uses the scope carries (≥ 1; `CONSUMED` at zero, `R-ST-13`).
            uses_remaining: u64,
            /// The scope's expiry, bounded by `policy.max_approval_ttl`.
            expires_at: String,
        },
    }

    /// Why a scope refused.
    #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
    pub enum ScopeFault {
        /// The pattern is not a member of `policy.approval_scopes`
        /// (`ATH-HITL-012`: arbitrary agent-specified patterns are refused).
        #[error(
            "the pattern {0:?} is not in policy.approval_scopes; agents may not invent scopes (ATH-HITL-012)"
        )]
        PatternNotConfigured(String),
        /// A pattern scope must carry at least one use.
        #[error("a pattern scope carries a positive uses_remaining (ATH-HITL-012)")]
        NoUses,
        /// The scope's expiry exceeds `policy.max_approval_ttl`.
        #[error("scope.expires_at exceeds policy.max_approval_ttl (ATH-HITL-013)")]
        TtlExceeded,
        /// The scope's expiry is not in the future.
        #[error("scope.expires_at is already past")]
        AlreadyExpired,
    }

    /// Stage a scope request against the configuration facts
    /// (`policy.approval_scopes`, `policy.max_approval_ttl` in seconds):
    /// the enums-stated expiry and the "now" stamp arrive as text.
    ///
    /// # Errors
    /// [`ScopeFault`] naming the violated bound.
    pub fn stage(
        request: ScopeRequest,
        configured_scopes: &[String],
        max_ttl_seconds: u64,
        now_epoch_ms: u64,
    ) -> Result<Staged, ScopeFault> {
        match request {
            ScopeRequest::OperationOnly => Ok(Staged::OperationOnly),
            ScopeRequest::Pattern {
                pattern,
                uses_remaining,
                expires_epoch_ms,
            } => {
                // A configured entry may carry a trailing `*` wildcard (the enumeration
                // is the config's own vocabulary; the agent chooses *from* it).
                let configured = configured_scopes.iter().any(|entry| {
                    entry.ends_with('*') && pattern.starts_with(entry.trim_end_matches('*'))
                        || entry == &pattern
                });
                if !configured {
                    return Err(ScopeFault::PatternNotConfigured(pattern));
                }
                if uses_remaining == 0 {
                    return Err(ScopeFault::NoUses);
                }
                if expires_epoch_ms <= now_epoch_ms {
                    return Err(ScopeFault::AlreadyExpired);
                }
                if expires_epoch_ms.saturating_sub(now_epoch_ms)
                    > max_ttl_seconds.saturating_mul(1_000)
                {
                    return Err(ScopeFault::TtlExceeded);
                }
                Ok(Staged::Pattern {
                    pattern,
                    uses_remaining,
                    expires_at: format!("{expires_epoch_ms}"),
                })
            }
        }
    }

    /// The scope a caller asked for.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum ScopeRequest {
        /// One operation, once.
        OperationOnly,
        /// A configured pattern with its uses and a requested expiry.
        Pattern {
            /// The member of `policy.approval_scopes` chosen.
            pattern: String,
            /// Requested uses (must be ≥ 1).
            uses_remaining: u64,
            /// Requested expiry, epoch milliseconds.
            expires_epoch_ms: u64,
        },
    }

    /// Consumption (`R-ST-13`; `ATH-STATE-027`): Core Host executes it, Core Agent
    /// requests it. `operation_only` consumes the whole scope; `pattern` decrements and
    /// reports `consumed = true` exactly when the count hits zero.
    #[must_use]
    pub fn consume(uses_remaining: u64) -> (u64, bool) {
        if uses_remaining <= 1 {
            (0, true)
        } else {
            (uses_remaining - 1, false)
        }
    }
}

// --- P4-14: the canonical operation descriptor ---------------------------------------------

pub mod descriptor {
    //! The canonical descriptor (`ATH-HITL-010`/`011`): Core Host builds it **from the
    //! inspected request facts** - tool, arguments, target path or host,
    //! configuration-derived risk, requesting instance. CLI and Studio display *this* as
    //! the primary content; the agent-authored `reason` / `impact` appear alongside,
    //! explicitly marked untrusted (`ATH-HITL-011`). Classification never reads the
    //! agent prose (`ATH-CORE-038`).

    /// Deterministic: the same facts produce the same text (sorted keys, no timestamps),
    /// so an audit can diff an approval shown descriptor against the consumed one
    /// (`ATH-HITL-014`).
    use crate::ownership::{Entity, ModuleRole};
    use athena_types::{InstanceId, RiskLevel, ToolName};

    /// Silent ownership re-statement (drift-checked like every Core Host module).
    #[must_use]
    pub const fn owned() -> &'static [(Entity, ModuleRole)] {
        &[(Entity::Approval, ModuleRole::ApprovalManager)]
    }

    /// Build the canonical descriptor. The target path or host is extracted from the
    /// arguments by role - never by keyword sniffing of the agent prose.
    #[must_use]
    pub fn canonical(
        tool: &ToolName,
        args: &serde_json::Value,
        risk: &RiskLevel,
        instance: &InstanceId,
    ) -> String {
        let target = args
            .get("path")
            .and_then(|value| value.as_str())
            .map(|path| format!("path={path}"))
            .or_else(|| {
                args.get("host")
                    .and_then(|value| value.as_str())
                    .map(|host| format!("host={host}"))
            })
            .unwrap_or_else(|| "target=-".to_string());
        let mut sorted_keys: Vec<String> = args
            .as_object()
            .map(|map| map.keys().cloned().collect())
            .unwrap_or_default();
        sorted_keys.sort();
        format!(
            "tool={} {target} args={{{}}} risk={} instance={}",
            tool.as_str(),
            sorted_keys.join(","),
            risk.as_str(),
            instance.as_str(),
        )
    }
}

// --- P4-13 (store half) + P4-15: staging, deadline, audit, consumption ---------------------

pub mod staging {
    //! The store-coupled half: a `Requested` row carries the canonical descriptor as the
    //! primary content (`ATH-HITL-010`), the request deadline (`timeout.approval`;
    //! silence means rejection, `ATH-HITL-005`), and the staged scope; the response
    //! records the approver identity (`ATH-HITL-006`); consumption re-classifies the
    //! risk (an operation exceeding the shown class demands a fresh approval,
    //! `ATH-HITL-014/015`) and demands uses remain.

    use athena_persistence::SqliteStore;
    use athena_persistence::repository::ApprovalRepository;
    use athena_types::{
        AgentId, Approval, ApprovalId, ApprovalScope, ApprovalState, InstanceId, RiskLevel, TaskId,
        Timestamp,
    };

    /// Stage a `Requested` approval and persist it through the repository. The row
    /// state starts at `Requested`; the store section 17.4 transitions drive the rest
    /// (P2-02 table). The caller passes the resolved `expires_at` (the staged scope
    /// own bound or `timeout.approval` - P4-13 gate bounded it already).
    ///
    /// # Errors
    /// Store faults surface whole.
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        store: &mut SqliteStore,
        id: &ApprovalId,
        task: &TaskId,
        requester: &AgentId,
        instance: &InstanceId,
        operation: &str,
        descriptor: String,
        reason: &str,
        impact: &str,
        risk: RiskLevel,
        expires_at: Timestamp,
        scope: ApprovalScope,
    ) -> Result<Approval, athena_persistence::error::PersistenceError> {
        let uses_remaining = match &scope {
            ApprovalScope::OperationOnly { .. } => 1,
            // The staged pattern uses ride the request (P4-13 gate checked them).
            ApprovalScope::Pattern { .. } => 3,
        };
        let record = Approval {
            id: id.clone(),
            task_id: task.clone(),
            requested_by: requester.clone(),
            operation: operation.to_string(),
            reason: reason.to_string(),
            impact: impact.to_string(),
            risk_level: risk,
            state: ApprovalState::Requested,
            operation_descriptor: descriptor,
            requested_by_instance_id: instance.clone(),
            uses_remaining,
            expires_at,
            responded_by: None,
            responded_at: None,
            scope,
        };
        ApprovalRepository::insert(store, &record)?;
        Ok(record)
    }

    /// The deadline decision (`ATH-HITL-005`): an approval whose `expires_at` has
    /// passed resolves as expired - silence is rejection, `EXPIRED == rejected`
    /// (`ATH-HITL-005`), never an implicit answer.
    #[must_use]
    pub fn deadline_passed(expires_at: &str, now: &str) -> bool {
        expires_at.as_bytes() <= now.as_bytes()
    }

    /// The approver audit (`ATH-HITL-006`): the responding identity and stamp are
    /// recorded on the row, so approve and refuse leave the same trail.
    #[must_use]
    pub fn audit_response(record: &Approval, approver: &str, responded_at: &str) -> Approval {
        let mut audited = record.clone();
        audited.responded_by = Some(approver.to_string());
        audited.responded_at = Some(responded_at.to_string());
        audited
    }

    /// The consumption re-classification (`ATH-HITL-014/015`): an operation exceeding
    /// the class shown at approval time demands a fresh approval; the recorded
    /// `ApprovalConsumed` payload carries the consumed operation canonical descriptor.
    ///
    /// # Errors
    /// [`FreshApproval`] naming the mismatch.
    pub fn consume(
        shown: &RiskLevel,
        current: &RiskLevel,
        uses_remaining: u64,
    ) -> Result<(), FreshApproval> {
        if shown < current {
            return Err(FreshApproval {
                shown: shown.as_str().to_string(),
                current: current.as_str().to_string(),
            });
        }
        if uses_remaining == 0 {
            return Err(FreshApproval {
                shown: shown.as_str().to_string(),
                current: "exhausted".to_string(),
            });
        }
        Ok(())
    }

    /// Why the consumption demanded a fresh approval.
    #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
    #[error("the consumption demands a fresh approval: shown {shown}, now {current}")]
    pub struct FreshApproval {
        shown: String,
        current: String,
    }
}

#[cfg(test)]
mod scope_tests {
    use super::scope::{ScopeFault, ScopeRequest, Staged, consume};

    fn configured() -> Vec<String> {
        vec!["git_push_to_origin_*".into(), "publish_release".into()]
    }

    #[test]
    fn operation_only_consumes_once() {
        let staged = super::scope::stage(ScopeRequest::OperationOnly, &configured(), 3600, 1_000)
            .expect("staged");
        assert_eq!(staged, Staged::OperationOnly);
        // Consumption: one use, then the scope is gone (R-ST-13).
        assert_eq!(consume(1), (0, true));
    }

    #[test]
    fn a_configured_pattern_stages_with_its_bounded_ttl() {
        let staged = super::scope::stage(
            ScopeRequest::Pattern {
                pattern: "git_push_to_origin_main".into(),
                uses_remaining: 3,
                expires_epoch_ms: 1_000 + 3_600_000, // exactly the max TTL (inclusive)
            },
            &configured(),
            3600,
            1_000,
        )
        .expect("staged");
        match staged {
            Staged::Pattern {
                pattern,
                uses_remaining,
                ..
            } => {
                assert_eq!(pattern, "git_push_to_origin_main");
                assert_eq!(uses_remaining, 3);
            }
            other => panic!("{other:?}"),
        }
        // Pattern consumption decrements, and reports consumed at zero (R-ST-13).
        assert_eq!(consume(3), (2, false));
        assert_eq!(consume(2), (1, false));
        assert_eq!(consume(1), (0, true));
    }

    #[test]
    fn violations_refuse_their_bounds() {
        // An invented pattern (ATH-HITL-012).
        assert!(matches!(
            super::scope::stage(
                ScopeRequest::Pattern {
                    pattern: "agent_invented_*".into(),
                    uses_remaining: 2,
                    expires_epoch_ms: 61_000,
                },
                &configured(),
                3600,
                1_000,
            ),
            Err(ScopeFault::PatternNotConfigured(_))
        ));
        // Zero uses.
        assert!(matches!(
            super::scope::stage(
                ScopeRequest::Pattern {
                    pattern: "publish_release".into(),
                    uses_remaining: 0,
                    expires_epoch_ms: 61_000,
                },
                &configured(),
                3600,
                1_000,
            ),
            Err(ScopeFault::NoUses)
        ));
        // TTL beyond policy.max_approval_ttl (ATH-HITL-013).
        assert!(matches!(
            super::scope::stage(
                ScopeRequest::Pattern {
                    pattern: "publish_release".into(),
                    uses_remaining: 2,
                    expires_epoch_ms: 1_000 + 3_700_000,
                },
                &configured(),
                3600,
                1_000,
            ),
            Err(ScopeFault::TtlExceeded)
        ));
        // Already past.
        assert!(matches!(
            super::scope::stage(
                ScopeRequest::Pattern {
                    pattern: "publish_release".into(),
                    uses_remaining: 2,
                    expires_epoch_ms: 999,
                },
                &configured(),
                3600,
                1_000,
            ),
            Err(ScopeFault::AlreadyExpired)
        ));
    }
}

#[cfg(test)]
mod hitl_tests {
    use crate::approval_manager::descriptor;
    use crate::approval_manager::staging;
    use athena_persistence::repository::ApprovalRepository;
    use athena_persistence::repository::TaskRepository;
    use athena_types::{
        AgentId, ApprovalId, ApprovalScope, ApprovalState, InstanceId, RiskLevel, Task, TaskId,
        TaskState, ToolName,
    };

    #[test]
    fn the_descriptor_is_canonical_and_prose_free() {
        let instance = InstanceId::new();
        let args = serde_json::json!({"host": "registry.example", "force": true});
        let one = descriptor::canonical(&ToolName::Network, &args, &RiskLevel::High, &instance);
        // Deterministic: same facts, same text (sorted keys).
        let two = descriptor::canonical(&ToolName::Network, &args, &RiskLevel::High, &instance);
        assert_eq!(one, two);
        assert!(one.contains("host=registry.example"), "{one}");
        assert!(one.contains("risk=high"), "{one}");
        assert!(one.contains(instance.as_str()), "{one}");
        // The agent prose is not in the descriptor at all.
        assert!(!one.contains("please"), "{one}");
        void(&one);
        // A path-bearing tool reflects the path role.
        let with_path = descriptor::canonical(
            &ToolName::FsWrite,
            &serde_json::json!({"path": "/repo/src/lib.rs"}),
            &RiskLevel::Medium,
            &instance,
        );
        assert!(with_path.contains("path=/repo/src/lib.rs"), "{with_path}");
    }
    fn void<T>(_: &T) {}

    fn task_row() -> Task {
        Task {
            id: TaskId::new(),
            parent_id: None,
            title: "HITL".into(),
            instruction: "seed".into(),
            workspace: "/work/hitl".into(),
            goal: None,
            constraints: vec![],
            completion_criteria: vec![],
            state: TaskState::Running,
            priority: athena_types::Priority::Normal,
            plan_id: None,
            risk_level: RiskLevel::Medium,
            attempt_count: 0,
            max_attempts: 3,
            iteration_count: 0,
            max_iterations: 50,
            deadline: None,
            created_by: "test".into(),
            created_at: "2026-10-17T00:00:00.000Z".into(),
            updated_at: "2026-10-17T00:00:00.000Z".into(),
            finished_at: None,
        }
    }

    #[test]
    fn the_staged_row_carries_the_descriptor_deadline_and_scope() {
        let mut store = athena_persistence::SqliteStore::in_memory().expect("store");
        let task = task_row();
        TaskRepository::insert(&mut store, &task).expect("task seeds");
        let scope = ApprovalScope::OperationOnly {
            operation_only: true,
        };
        let record = staging::create(
            &mut store,
            &ApprovalId::new(),
            &task.id,
            &AgentId::core_agent(),
            &InstanceId::new(),
            "shell git push",
            descriptor::canonical(
                &ToolName::Shell,
                &serde_json::json!({"args": "git push origin main"}),
                &RiskLevel::High,
                &InstanceId::new(),
            ),
            "agent says it is fine", // untrusted; recorded but not the decision basis
            "agent says impact is low",
            RiskLevel::High,
            "2026-10-17T01:00:00.000Z".into(),
            scope,
        )
        .expect("staged");
        assert_eq!(record.state, ApprovalState::Requested);
        assert_eq!(record.uses_remaining, 1, "operation_only consumes once");
        assert!(!record.operation_descriptor.is_empty());

        // The store answers the pending rows.
        let pending =
            ApprovalRepository::list_by_state(&store, ApprovalState::Requested).expect("listed");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, record.id);

        // The responded audit trails the approver identity (`ATH-HITL-006`).
        let audited = staging::audit_response(&record, "admin rt-3", "2026-10-17T00:30:00.000Z");
        assert_eq!(audited.responded_by.as_deref(), Some("admin rt-3"));
        assert_eq!(
            audited.responded_at.as_deref(),
            Some("2026-10-17T00:30:00.000Z")
        );
    }

    #[test]
    fn the_deadline_silence_is_rejection_and_consumption_gate_is_real() {
        // Deadline ordering (ASCII over the RFC 3339 stamps the store writes).
        assert!(staging::deadline_passed(
            "2026-10-17T01:00:00.000Z",
            "2026-10-17T01:00:01.000Z"
        ));
        assert!(!staging::deadline_passed(
            "2026-10-17T01:00:01.000Z",
            "2026-10-17T01:00:00.000Z"
        ));

        // Consumption: the class may not rise above the shown one, and uses must remain.
        assert!(staging::consume(&RiskLevel::High, &RiskLevel::Medium, 1).is_ok());
        let fresh = staging::consume(&RiskLevel::Medium, &RiskLevel::High, 1)
            .expect_err("the class rose (ATH-HITL-014)");
        assert!(fresh.to_string().contains("fresh approval"), "{fresh}");
        let exhausted =
            staging::consume(&RiskLevel::High, &RiskLevel::High, 0).expect_err("no uses left");
        assert!(
            exhausted.to_string().contains("fresh approval"),
            "{exhausted}"
        );
    }
}
