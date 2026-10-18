//! Memory tier policy (P4-05, `ATH-MEM-001`/`002`): the three tiers' separation facts,
//! their size ceilings, and their eviction policies as pure, store-agnostic policy —
//! the store-owning callers (Core Host's Memory Manager, `ATH-MEM-012`) plumb these into
//! the `MemoryRepository`.
//!
//! | Tier | Scope | Lifetime | Ceiling | Eviction (`ATH-MEM-002`) |
//! |---|---|---|---|---|
//! | Short | Agent Instance | until instance end | `short_term_max_tokens`, counted with `memory.tokenizer` (`ATH-MEM-009`: never an implicit provider default) | summarize oldest first — delegated to the `memory.summarizer` agent (`ATH-MEM-008`, P4-06) |
//! | Working | Task | until terminal state | `memory.working_max_records` | summarize completed PlanSteps — same delegation; the P1-36 fallback (oldest out, references kept) runs until the summarizer lands |
//! | Long | Project | indefinite until explicit deletion | `memory.long_term.max_records` (active only when `long_term.enabled`) | **explicit removal** — deletion is a deliberate Core Agent judgement or human instruction, recorded as an Event (`ATH-MEM-003`, `ATH-MEM-006`) |
//!
//! **[design decision]** Long Term's eviction policy is *explicit deliberation*, not a
//! summarizer: the tier must outlive summarization cycles (its records are the durable
//! project memory), so pressure at the ceiling refuses further writes rather than
//! silently digesting history. The caller surfaces that as an escalation, keeping the
//! "explicit judgement or human instruction" rule (`ATH-MEM-003`) enforced.

use athena_types::MemoryTier;

/// Why a policy refused its own inputs.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyFault {
    /// Tokenizer not stated — `ATH-MEM-009` forbids relying on provider defaults.
    #[error("memory.tokenizer must be stated (ATH-MEM-009); got empty")]
    TokenizerUnstated,
    /// Summarizer not stated — summarizing is delegated, never implicit (`ATH-MEM-008`).
    #[error("memory.summarizer must name the agent that summarizes (ATH-MEM-008); got empty")]
    SummarizerUnstated,
    /// A ceiling of zero accepts nothing at all — a misconfiguration, not a policy.
    #[error("the memory ceiling for {0:?} must be positive (ATH-MEM-002)")]
    NonPositiveCeiling(MemoryTier),
}

/// The tier policy set, built from the resolved `memory` configuration values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryPolicy {
    /// Short-term token ceiling.
    pub short_term_max_tokens: u64,
    /// The tokenizer the token count uses (`ATH-MEM-009`).
    pub tokenizer: String,
    /// Working record ceiling.
    pub working_max_records: u64,
    /// Long-term activation gate (`memory.long_term.enabled`).
    pub long_term_enabled: bool,
    /// Long-term record ceiling.
    pub long_term_max_records: u64,
    /// The agent definition that performs eviction summarization (`ATH-MEM-008`).
    pub summarizer: String,
}

impl MemoryPolicy {
    /// Build the policy from the configured values, refusing an unusable one
    /// (a zero ceiling or an unstated tokenizer would silently make a tier dead).
    ///
    /// # Errors
    /// [`PolicyFault`] with the violated value.
    pub fn from_values(
        short_term_max_tokens: u64,
        tokenizer: &str,
        working_max_records: u64,
        long_term_enabled: bool,
        long_term_max_records: u64,
        summarizer: &str,
    ) -> Result<Self, PolicyFault> {
        if tokenizer.trim().is_empty() {
            return Err(PolicyFault::TokenizerUnstated);
        }
        if summarizer.trim().is_empty() {
            return Err(PolicyFault::SummarizerUnstated);
        }
        for (tier, ceiling) in [
            (MemoryTier::Short, short_term_max_tokens),
            (MemoryTier::Working, working_max_records),
        ] {
            if ceiling == 0 {
                return Err(PolicyFault::NonPositiveCeiling(tier));
            }
        }
        if long_term_enabled && long_term_max_records == 0 {
            return Err(PolicyFault::NonPositiveCeiling(MemoryTier::Long));
        }
        Ok(Self {
            short_term_max_tokens,
            tokenizer: tokenizer.to_string(),
            working_max_records,
            long_term_enabled,
            long_term_max_records,
            summarizer: summarizer.to_string(),
        })
    }

    /// One tier's policy view: its ceiling kind and its eviction rule.
    ///
    /// # Errors
    /// [`PolicyFault::NonPositiveCeiling`] when the tier's ceiling is a misconfiguration
    /// and the long-term gate is disabled while the tier is asked about
    /// (an inactive tier has no policy).
    #[must_use]
    pub fn tier(&self, tier: MemoryTier) -> Option<TierPolicy> {
        match tier {
            MemoryTier::Short => Some(TierPolicy {
                tier,
                ceiling: Ceiling::Tokens(self.short_term_max_tokens),
                eviction: Eviction::SummarizeOldestFirst(self.summarizer.clone()),
            }),
            MemoryTier::Working => Some(TierPolicy {
                tier,
                ceiling: Ceiling::Records(self.working_max_records),
                eviction: Eviction::SummarizeCompletedSteps(self.summarizer.clone()),
            }),
            MemoryTier::Long if self.long_term_enabled => Some(TierPolicy {
                tier,
                ceiling: Ceiling::Records(self.long_term_max_records),
                eviction: Eviction::ExplicitDeliberation,
            }),
            MemoryTier::Long => None,
        }
    }

    /// The short-term budget the Runtime-facing side needs (`ATH-MEM-010`, `ATH-ACLI-010`):
    /// the Adapter mirrors the ceiling into `agent-cli`'s `[history] max_context_tokens`
    /// and ATHENA snapshots only at `SUSPENDED` (`ATH-MEM-010`).
    #[must_use]
    pub fn short_term_budget(&self) -> ShortTermBudget {
        ShortTermBudget {
            max_tokens: self.short_term_max_tokens,
            tokenizer: self.tokenizer.clone(),
        }
    }
}

/// One tier's ceiling kind and eviction rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierPolicy {
    /// The tier.
    pub tier: MemoryTier,
    /// Its ceiling, in the unit the tier counts.
    pub ceiling: Ceiling,
    /// Its eviction rule (`ATH-MEM-002`).
    pub eviction: Eviction,
}

/// The counting unit a tier's ceiling measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ceiling {
    /// Tokens (short term; the count uses [`MemoryPolicy::tokenizer`]).
    Tokens(u64),
    /// Record rows (working, long).
    Records(u64),
}

/// The eviction rule a tier applies at its ceiling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Eviction {
    /// Summarize the oldest records first — delegated to the named summarizer agent
    /// (`ATH-MEM-008`); the specific agent is carried in the policy.
    SummarizeOldestFirst(String),
    /// Working tier's rule: summarize completed PlanSteps, same delegation.
    SummarizeCompletedSteps(String),
    /// Long-term rule: no automatic removal — the ceiling pressure is surfaced to the
    /// decision-makers (explicit Core Agent judgement or human instruction, `ATH-MEM-003`).
    ExplicitDeliberation,
}

/// The AIPRJ surface note for the eviction summarizer's workspace context: the
/// summarizer agent (like every agent) works under the aiprj convention.
#[must_use]
pub fn summarizer_agent_id(policy: &MemoryPolicy, project_root: &std::path::Path) -> String {
    format!(
        "{}@{}",
        policy.summarizer,
        project_root.join(".aiprj").display()
    )
}

/// The short-term limit-management surface (`ATH-MEM-010`): taken from the `agent-cli`
/// `[history]` accounting, `ATHENA` performs only limit management and snapshotting —
/// the *content* lives in the Agent Process (never mirrored; `R-MEM-09`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortTermBudget {
    /// The token ceiling (`memory.short_term_max_tokens`).
    pub max_tokens: u64,
    /// The tokenizer the count is measured in (`ATH-MEM-009`).
    pub tokenizer: String,
}

impl ShortTermBudget {
    /// Whether a context of `tokens` tokens is admitted.
    ///
    /// # Errors
    /// [`TokenOverflow`] naming the ceiling when over.
    pub fn check(&self, tokens: u64) -> Result<(), TokenOverflow> {
        if tokens <= self.max_tokens {
            return Ok(());
        }
        Err(TokenOverflow {
            tokens,
            max_tokens: self.max_tokens,
            tokenizer: self.tokenizer.clone(),
        })
    }
}

/// The short-term overflow surface (`ATH-MEM-002`): evictable, never silently trimmed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "short-term context {tokens} tokens exceeds the {max_tokens} ceiling (counted with {tokenizer:?}; evict oldest first)"
)]
pub struct TokenOverflow {
    /// The over-budget count.
    pub tokens: u64,
    /// The ceiling.
    pub max_tokens: u64,
    /// The tokenizer in force.
    pub tokenizer: String,
}

// --- Tests ----------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> MemoryPolicy {
        MemoryPolicy::from_values(
            32_000,
            "provider-claude",
            500,
            true,
            10_000,
            "memory-summarizer",
        )
        .expect("valid policy")
    }

    #[test]
    fn the_three_tiers_carry_their_spec_facts() {
        let policy = policy();

        // Short: tokens, oldest-first summarization by the named agent.
        let short = policy.tier(MemoryTier::Short).expect("active");
        assert_eq!(short.ceiling, Ceiling::Tokens(32_000));
        assert_eq!(
            short.eviction,
            Eviction::SummarizeOldestFirst("memory-summarizer".into())
        );

        // Working: records, completed-PlanStep summarization.
        let working = policy.tier(MemoryTier::Working).expect("active");
        assert_eq!(working.ceiling, Ceiling::Records(500));
        assert_eq!(
            working.eviction,
            Eviction::SummarizeCompletedSteps("memory-summarizer".into())
        );

        // Long: records, explicit deliberation — never a silent digest.
        let long = policy.tier(MemoryTier::Long).expect("active");
        assert_eq!(long.ceiling, Ceiling::Records(10_000));
        assert_eq!(long.eviction, Eviction::ExplicitDeliberation);
    }

    #[test]
    fn an_disabled_long_term_tier_has_no_policy() {
        let policy =
            MemoryPolicy::from_values(32000, "t", 500, false, 10_000, "summarizer").expect("valid");
        assert!(policy.tier(MemoryTier::Long).is_none());
        // And the short/working facts still stand.
        assert!(policy.tier(MemoryTier::Short).is_some());
        assert!(policy.tier(MemoryTier::Working).is_some());
    }

    #[test]
    fn unusable_policies_refuse_their_inputs() {
        // An empty tokenizer (ATH-MEM-009).
        assert!(matches!(
            MemoryPolicy::from_values(32000, "  ", 500, true, 10000, "summarizer"),
            Err(PolicyFault::TokenizerUnstated)
        ));
        // An empty summarizer (ATH-MEM-008).
        assert!(matches!(
            MemoryPolicy::from_values(32000, "tokenizer", 500, true, 10000, ""),
            Err(PolicyFault::SummarizerUnstated)
        ));
        // Zero ceilings per tier.
        assert!(matches!(
            MemoryPolicy::from_values(0, "t", 500, true, 10000, "s"),
            Err(PolicyFault::NonPositiveCeiling(MemoryTier::Short))
        ));
        assert!(matches!(
            MemoryPolicy::from_values(32000, "t", 0, true, 10000, "s"),
            Err(PolicyFault::NonPositiveCeiling(MemoryTier::Working))
        ));
        assert!(matches!(
            MemoryPolicy::from_values(32000, "t", 500, true, 0, "s"),
            Err(PolicyFault::NonPositiveCeiling(MemoryTier::Long))
        ));
        // A disabled long tier's zero ceiling is irrelevant.
        assert!(MemoryPolicy::from_values(32000, "t", 500, false, 0, "s").is_ok());
    }

    #[test]
    fn the_short_term_budget_admits_or_names_the_overflow() {
        let budget = policy().short_term_budget();
        budget.check(32_000).expect("at the ceiling is admitted");
        budget.check(1).expect("a small context");
        let over = budget.check(32_001).expect_err("over");
        assert_eq!(over.max_tokens, 32_000);
        assert_eq!(over.tokens, 32_001);
        assert_eq!(over.tokenizer, "provider-claude");
        assert!(over.to_string().contains("evict oldest first"));
    }

    #[test]
    fn the_eviction_summarizer_carries_its_aiprj_context() {
        let policy = policy();
        let id = summarizer_agent_id(&policy, std::path::Path::new("/work/mine/project"));
        assert!(
            id.starts_with("memory-summarizer@/work/mine/project/.aiprj"),
            "{id}"
        );
    }
}
