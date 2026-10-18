//! Risk classification from `policy.rules` (ATH-CFG-009, ATH-HITL-002).
//!
//! This module is the matching algorithm only. The *enforcement* point is Core Host's Policy
//! Manager and Tool Manager (ATH-SEC-022, ATH-SEC-026), which calls in here; the rules live in
//! configuration, so the matcher lives with them rather than being duplicated in Core.
//!
//! Two properties are load-bearing. Rules are evaluated top-down and the first match wins, so
//! order is significant. An operation matching no rule is `High`, which makes the default deny
//! rather than allow — a rule set that forgets a case fails safe.

use crate::error::{ConfigError, Result};
use crate::schema::{PolicyRule, PolicySection};
use athena_types::{RiskLevel, ToolName};
use regex::Regex;

/// The operation being classified.
///
/// Built by Core Host from an inspected request, never from agent prose: the agent's `reason`
/// and `impact` must not influence classification (ATH-CORE-038).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operation {
    /// Tool being invoked.
    pub tool: ToolName,
    /// Arguments as a single string, which `args_pattern` is matched against.
    pub args: String,
    /// Target hosts, for network operations.
    pub hosts: Vec<String>,
    /// Coarse path scope, e.g. `outside_workspace`.
    pub path_scope: Option<String>,
}

impl Operation {
    /// An operation with just a tool and arguments.
    #[must_use]
    pub fn new(tool: ToolName, args: impl Into<String>) -> Self {
        Self {
            tool,
            args: args.into(),
            hosts: Vec::new(),
            path_scope: None,
        }
    }

    /// Set the target hosts.
    #[must_use]
    pub fn with_hosts(mut self, hosts: Vec<String>) -> Self {
        self.hosts = hosts;
        self
    }

    /// Set the path scope.
    #[must_use]
    pub fn with_path_scope(mut self, scope: impl Into<String>) -> Self {
        self.path_scope = Some(scope.into());
        self
    }
}

/// The outcome of classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Classification {
    /// Assigned risk.
    pub risk: RiskLevel,
    /// Whether an approval must be obtained first.
    pub requires_approval: bool,
    /// Index of the matching rule, or `None` when the default-deny applied.
    ///
    /// Recorded so the canonical descriptor can state which rule decided (ATH-HITL-010).
    pub matched_rule: Option<usize>,
}

impl Classification {
    /// The classification used when no rule matches (ATH-CFG-009).
    #[must_use]
    pub const fn default_deny() -> Self {
        Self {
            risk: RiskLevel::High,
            requires_approval: true,
            matched_rule: None,
        }
    }
}

/// A `policy.rules` set with its regular expressions compiled.
///
/// Compiling once at startup serves two purposes: an invalid pattern becomes a startup failure
/// rather than a surprise during a Tool execution, and classification stays allocation-light on
/// the hot path.
#[derive(Debug)]
pub struct CompiledPolicy {
    rules: Vec<CompiledRule>,
}

#[derive(Debug)]
struct CompiledRule {
    rule: PolicyRule,
    args_pattern: Option<Regex>,
}

impl CompiledPolicy {
    /// Compile a policy section, failing on an invalid pattern.
    pub fn compile(section: &PolicySection) -> Result<Self> {
        let mut rules = Vec::with_capacity(section.rules.len());
        for (index, rule) in section.rules.iter().enumerate() {
            let args_pattern =
                match &rule.matcher.args_pattern {
                    Some(pattern) => Some(Regex::new(pattern).map_err(|e| {
                        ConfigError::ConstraintViolated {
                            constraint: format!(
                                "policy.rules[{index}].match.args_pattern is not a valid regular \
                             expression: {e}"
                            ),
                        }
                    })?),
                    None => None,
                };
            rules.push(CompiledRule {
                rule: rule.clone(),
                args_pattern,
            });
        }
        Ok(Self { rules })
    }

    /// Classify an operation.
    ///
    /// Returns the first matching rule's verdict, or `Classification::default_deny()` when none
    /// matches.
    #[must_use]
    pub fn classify(&self, op: &Operation) -> Classification {
        for (index, compiled) in self.rules.iter().enumerate() {
            if compiled.matches(op) {
                return Classification {
                    risk: compiled.rule.risk,
                    requires_approval: compiled.rule.requires_approval,
                    matched_rule: Some(index),
                };
            }
        }
        Classification::default_deny()
    }

    /// Number of rules.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Whether the rule set is empty, in which case everything is `High`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

impl CompiledRule {
    /// Whether every stated criterion of this rule holds.
    ///
    /// Criteria are conjunctive, and an unstated criterion does not constrain. A rule with no
    /// criteria at all therefore matches everything, which is why rule order matters.
    fn matches(&self, op: &Operation) -> bool {
        let m = &self.rule.matcher;

        if let Some(tool) = m.tool
            && tool != op.tool
        {
            return false;
        }

        if let Some(pattern) = &self.args_pattern
            && !pattern.is_match(&op.args)
        {
            return false;
        }

        if let Some(hosts) = &m.hosts {
            // `*` matches any host, including an operation that names none: a network call to
            // an unspecified destination must not slip past a wildcard rule.
            let wildcard = hosts.iter().any(|h| h == "*");
            if !wildcard && !op.hosts.iter().any(|h| hosts.contains(h)) {
                return false;
            }
        }

        if let Some(scope) = &m.path_scope
            && op.path_scope.as_deref() != Some(scope.as_str())
        {
            return false;
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Config, PolicyMatch};

    fn default_policy() -> CompiledPolicy {
        CompiledPolicy::compile(&Config::default().policy).expect("default rules compile")
    }

    #[test]
    fn the_default_rule_set_compiles() {
        assert_eq!(default_policy().len(), 6);
    }

    #[test]
    fn git_push_is_high_risk_and_needs_approval() {
        // Spec section 28.1's first rule; the publishing operation of ATH-HITL-001.
        let c = default_policy().classify(&Operation::new(ToolName::Shell, "git push origin main"));
        assert_eq!(c.risk, RiskLevel::High);
        assert!(c.requires_approval);
        assert_eq!(c.matched_rule, Some(0));
    }

    #[test]
    fn recursive_delete_is_high_risk_and_needs_approval() {
        let c = default_policy().classify(&Operation::new(ToolName::Shell, "rm -rf /work/build"));
        assert_eq!(c.risk, RiskLevel::High);
        assert!(c.requires_approval);
        assert_eq!(c.matched_rule, Some(1));
    }

    #[test]
    fn an_ordinary_shell_command_is_medium_and_needs_no_approval() {
        // Reached only because the two specific shell rules did not match first.
        let c = default_policy().classify(&Operation::new(ToolName::Shell, "cargo build"));
        assert_eq!(c.risk, RiskLevel::Medium);
        assert!(!c.requires_approval);
        assert_eq!(c.matched_rule, Some(4));
    }

    #[test]
    fn rule_order_decides_when_several_rules_could_match() {
        // ATH-CFG-009: first match wins. `git push` matches both rule 0 and the general shell
        // rule 4; the specific one must win, otherwise a publish would run unapproved.
        let p = default_policy();
        let push = p.classify(&Operation::new(ToolName::Shell, "git push"));
        assert_eq!(push.matched_rule, Some(0));
        assert_eq!(push.risk, RiskLevel::High);

        // Reversing the order inverts the outcome, which is what makes order normative.
        let mut section = Config::default().policy;
        section.rules.reverse();
        let reversed = CompiledPolicy::compile(&section).expect("compiles");
        let push = reversed.classify(&Operation::new(ToolName::Shell, "git push"));
        assert_eq!(push.risk, RiskLevel::Medium);
    }

    #[test]
    fn fs_read_is_low_risk() {
        let c = default_policy().classify(&Operation::new(ToolName::FsRead, "/work/src/main.rs"));
        assert_eq!(c.risk, RiskLevel::Low);
        assert!(!c.requires_approval);
    }

    #[test]
    fn an_unmatched_operation_defaults_to_high_and_requires_approval() {
        // ATH-CFG-009's default-deny. The stock rules say nothing about `git`, `terminal` or
        // `process`, so each must fail safe rather than fall through as permitted.
        let p = default_policy();
        for tool in [ToolName::Git, ToolName::Terminal, ToolName::Process] {
            let c = p.classify(&Operation::new(tool, "whatever"));
            assert_eq!(c.risk, RiskLevel::High, "{tool} must default to high");
            assert!(c.requires_approval, "{tool} must require approval");
            assert_eq!(c.matched_rule, None);
        }
    }

    #[test]
    fn an_empty_rule_set_classifies_everything_as_high() {
        let section = PolicySection {
            rules: Vec::new(),
            ..Config::default().policy
        };
        let p = CompiledPolicy::compile(&section).expect("compiles");
        assert!(p.is_empty());
        let c = p.classify(&Operation::new(ToolName::FsRead, "x"));
        assert_eq!(c.risk, RiskLevel::High);
        assert!(c.requires_approval);
    }

    #[test]
    fn a_wildcard_host_rule_matches_any_destination_including_none() {
        // An operation that names no host must not evade the wildcard network rule.
        let p = default_policy();
        let named = p.classify(
            &Operation::new(ToolName::Network, "GET /").with_hosts(vec!["example.invalid".into()]),
        );
        assert_eq!(named.risk, RiskLevel::High);

        let unnamed = p.classify(&Operation::new(ToolName::Network, "GET /"));
        assert_eq!(unnamed.risk, RiskLevel::High);
        assert!(unnamed.requires_approval);
    }

    #[test]
    fn a_specific_host_rule_only_matches_that_host() {
        let section = PolicySection {
            rules: vec![PolicyRule {
                matcher: PolicyMatch {
                    tool: Some(ToolName::Network),
                    hosts: Some(vec!["allowed.invalid".into()]),
                    ..PolicyMatch::default()
                },
                risk: RiskLevel::Low,
                requires_approval: false,
            }],
            ..Config::default().policy
        };
        let p = CompiledPolicy::compile(&section).expect("compiles");

        let hit = p.classify(
            &Operation::new(ToolName::Network, "GET /").with_hosts(vec!["allowed.invalid".into()]),
        );
        assert_eq!(hit.risk, RiskLevel::Low);

        // A different host falls through to default-deny.
        let miss = p.classify(
            &Operation::new(ToolName::Network, "GET /").with_hosts(vec!["other.invalid".into()]),
        );
        assert_eq!(miss.risk, RiskLevel::High);
        assert_eq!(miss.matched_rule, None);
    }

    #[test]
    fn writing_outside_the_workspace_is_high_risk() {
        let p = default_policy();
        let outside = p.classify(
            &Operation::new(ToolName::FsWrite, "/etc/hosts").with_path_scope("outside_workspace"),
        );
        assert_eq!(outside.risk, RiskLevel::High);
        assert!(outside.requires_approval);
        assert_eq!(outside.matched_rule, Some(3));
    }

    #[test]
    fn writing_inside_the_workspace_falls_through_to_default_deny() {
        // The stock rules classify fs_write only when it leaves the workspace, so an in-scope
        // write is unmatched and therefore high. This is the default-deny being conservative,
        // and it is why an operator tuning policy must add an explicit in-scope rule.
        let p = default_policy();
        let inside = p.classify(&Operation::new(ToolName::FsWrite, "src/main.rs"));
        assert_eq!(inside.risk, RiskLevel::High);
        assert_eq!(inside.matched_rule, None);
    }

    #[test]
    fn criteria_within_a_rule_are_conjunctive() {
        let section = PolicySection {
            rules: vec![PolicyRule {
                matcher: PolicyMatch {
                    tool: Some(ToolName::Shell),
                    args_pattern: Some("^deploy".into()),
                    path_scope: Some("outside_workspace".into()),
                    hosts: None,
                },
                risk: RiskLevel::Low,
                requires_approval: false,
            }],
            ..Config::default().policy
        };
        let p = CompiledPolicy::compile(&section).expect("compiles");

        // All criteria present: matches.
        let all = p.classify(
            &Operation::new(ToolName::Shell, "deploy now").with_path_scope("outside_workspace"),
        );
        assert_eq!(all.matched_rule, Some(0));

        // One criterion absent: no match, so default-deny.
        let partial = p.classify(&Operation::new(ToolName::Shell, "deploy now"));
        assert_eq!(partial.matched_rule, None);
    }

    #[test]
    fn a_rule_with_no_criteria_matches_everything() {
        let section = PolicySection {
            rules: vec![PolicyRule {
                matcher: PolicyMatch::default(),
                risk: RiskLevel::Low,
                requires_approval: false,
            }],
            ..Config::default().policy
        };
        let p = CompiledPolicy::compile(&section).expect("compiles");
        for tool in ToolName::ALL {
            assert_eq!(p.classify(&Operation::new(*tool, "x")).risk, RiskLevel::Low);
        }
    }

    #[test]
    fn an_invalid_pattern_fails_at_compile_time_not_at_execution_time() {
        // A broken pattern must abort startup, not surface mid-Task when a Tool runs.
        let section = PolicySection {
            rules: vec![PolicyRule {
                matcher: PolicyMatch {
                    tool: Some(ToolName::Shell),
                    args_pattern: Some("[unclosed".into()),
                    ..PolicyMatch::default()
                },
                risk: RiskLevel::High,
                requires_approval: true,
            }],
            ..Config::default().policy
        };
        let err = CompiledPolicy::compile(&section).expect_err("must be refused");
        assert!(err.to_string().contains("args_pattern"), "{err}");
    }

    #[test]
    fn classification_ignores_everything_the_agent_could_say() {
        // ATH-CORE-038 / ATH-HITL-002: the Operation type carries no agent-authored reason or
        // impact field, so prose cannot reach the classifier at all.
        let op = Operation::new(ToolName::Shell, "rm -rf /");
        let c = default_policy().classify(&op);
        assert_eq!(c.risk, RiskLevel::High);
        assert!(c.requires_approval);
    }
}
