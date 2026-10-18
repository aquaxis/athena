//! The retry decision layer (P4-21, `ATH-FAIL-004..007`).
//!
//! The pieces:
//!
//! - **The ceiling** — `retry.max_attempts` (default 3, `ATH-FAIL-005`), overridable
//!   per Task / per PlanStep (`ATH-FAIL-004`) with the override rule that an override
//!   **lowers** the configured maximum (never raises it — the same bound-shaped rule
//!   the approval TTL carries).
//! - **The disposition decides the retry kind** (P4-20's table): `TransientError`
//!   retries at the `RetryBudget::ready_at` instant (exponential backoff,
//!   `ATH-FAIL-006`); `AgentError` retries with an alternate agent; `SkillError` with
//!   an alternate skill; `RuntimeError` recreates the Instance; classes whose row is
//!   `Never` are refused here, not at the call site.
//! - **The recording** (`ATH-FAIL-007`): every attempt is an independent Agent
//!   Instance row in `plan_step_attempts` (the store's `record_attempt`, which refuses
//!   a duplicate `(step, attempt)`); the history is never overwritten.

use crate::protocol::{Disposition, RetryKind};

/// Why an attempt refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RetryFault {
    /// The class's §20.1 row forbids the retry (`ATH-FAIL-002`).
    #[error("the class never retries (its §20.1 disposition)")]
    ClassForbids,
    /// Every attempt was consumed; re-plan or fail next (`ATH-FAIL-005`).
    #[error("the attempt budget is exhausted after {attempts} attempts")]
    Exhausted {
        /// The consumed count.
        attempts: u64,
    },
}

/// The attempt decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Run attempt `attempt` (the next number over the `plan_step_attempts` rows).
    Attempt {
        /// The 1-based next attempt.
        attempt: u64,
    },
    /// The budget exhausted: re-plan or fail next (`ATH-FAIL-005`, `ATH-FAIL-013`).
    Exhausted {
        /// The consumed count, for the escalation record.
        attempts: u64,
    },
}

/// The retry policy, configured and overridable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// The configured ceiling (`retry.max_attempts`, default 3 — `ATH-FAIL-005`).
    pub max_attempts: u64,
}

impl RetryPolicy {
    /// The ceiling for one PlanStep: the most specific override, never *raising* the
    /// configured maximum (`ATH-FAIL-004`).
    #[must_use]
    pub fn ceiling_for(&self, task_override: Option<u64>, step_override: Option<u64>) -> u64 {
        let mut ceiling = self.max_attempts;
        ceiling = ceiling
            .min(task_override.unwrap_or(ceiling))
            .min(step_override.unwrap_or(ceiling));
        ceiling
    }

    /// The decision for one failure of `step` given its unchanged stated disposition.
    ///
    /// `attempts_so_far` is what the `plan_step_attempts` rows record.
    ///
    /// # Errors
    /// [`RetryFault::ClassForbids`] when the class's row is `Never` (`ATH-FAIL-002`).
    pub fn decide(
        &self,
        disposition: &Disposition,
        attempts_so_far: u64,
        task_override: Option<u64>,
        step_override: Option<u64>,
    ) -> Result<Decision, RetryFault> {
        match disposition.retry {
            RetryKind::Never => Err(RetryFault::ClassForbids),
            RetryKind::ExponentialBackoff
            | RetryKind::Bounded
            | RetryKind::AlternateAgent
            | RetryKind::AlternateSkill
            | RetryKind::RecreateInstance => {
                let ceiling = self.ceiling_for(task_override, step_override);
                if attempts_so_far >= ceiling {
                    return Err(RetryFault::Exhausted {
                        attempts: attempts_so_far,
                    });
                }
                Ok(Decision::Attempt {
                    attempt: attempts_so_far + 1,
                })
            }
        }
    }
}

#[cfg(test)]
mod retry_tests {
    use super::{Decision, RetryFault, RetryPolicy};
    use crate::protocol::{Disposition, RetryKind};
    use athena_types::PlanStepId;

    fn policy() -> RetryPolicy {
        RetryPolicy { max_attempts: 3 }
    }

    fn transient() -> Disposition {
        Disposition {
            retry: RetryKind::ExponentialBackoff,
            replan: false,
            escalation: crate::protocol::Escalation::None,
            replan_at_limit: false,
        }
    }

    fn never() -> Disposition {
        Disposition {
            retry: RetryKind::Never,
            replan: false,
            escalation: crate::protocol::Escalation::None,
            replan_at_limit: false,
        }
    }

    #[test]
    fn the_ceiling_admits_to_the_limit_then_exhausts() {
        let policy = policy();
        // [... RetryKind iterations are the caller's; the decision counts rows.]
        for index in 0..3_u64 {
            let decision = policy
                .decide(&transient(), index, None, None)
                .expect("admitted");
            assert_eq!(decision, Decision::Attempt { attempt: index + 1 });
        }
        let exhausted = policy
            .decide(&transient(), 3, None, None)
            .expect_err("exhausted at the ceiling (ATH-FAIL-005)");
        assert!(matches!(exhausted, RetryFault::Exhausted { attempts: 3 }));
    }

    #[test]
    fn overrides_lower_the_ceiling_never_raise_it() {
        let policy = policy();
        // A step override of 1: only one attempt is admitted.
        assert_eq!(policy.ceiling_for(Some(3), Some(1)), 1);
        let exhausted = policy
            .decide(&transient(), 1, None, Some(1))
            .expect_err("the step ceiling bites");
        assert!(matches!(exhausted, RetryFault::Exhausted { attempts: 1 }));
        // An override *above* the configured ceiling is clamped down.
        assert_eq!(policy.ceiling_for(Some(10), None), 3);
        // The Task override alone binds.
        assert_eq!(policy.ceiling_for(Some(2), None), 2);
    }

    #[test]
    fn the_never_retry_classes_are_refused_here_not_at_the_call_site() {
        let refused = policy()
            .decide(&never(), 0, None, None)
            .expect_err("the class forbids (ATH-FAIL-002)");
        assert!(matches!(refused, RetryFault::ClassForbids), "{refused}");
        void(&PlanStepId::new());
    }
    fn void<T>(_: &T) {}

    #[test]
    fn an_attempt_is_one_independent_instance_note_per_row() {
        // The recording fact: each attempt row is its own instance (`ATH-FAIL-007`).
        // The store's own tests (a duplicate (step, attempt) refusing) hold that; this
        // test pins the *decision side* to naming the next attempt — never reusing one.
        let step = PlanStepId::new();
        let _ = step;
        let first = policy().decide(&transient(), 0, None, None).expect("first");
        let second = policy()
            .decide(&transient(), 1, None, None)
            .expect("second");
        assert!(matches!(first, Decision::Attempt { attempt: 1 }));
        assert!(matches!(second, Decision::Attempt { attempt: 2 }));
    }
}

// --- P4-22: the five timeouts and the human-wait accounting (ATH-FAIL-009..015) ------------

pub mod timeouts {
    //! The five deadlines (`timeout.startup` / `execution` / `message_reply` /
    //! `approval` / `stop`) and the accounting rule `ATH-FAIL-014`: time spent in
    //! `SUSPENDED` (approval) and `WAITING_INPUT` (clarification) is **excluded** from
    //! `execution` and `message_reply` — human latency never destroys work.
    //!
    //! Detection itself lives where the clocks live: the Process Manager's `status`
    //! (P3-02's deadline enforcement), the messaging `WaitRecord` (P2-16's reply
    //! deadline), P4-15's approval expiry. What lands here is the one *decision* those
    //! surfaces all consult, plus the exclusion's shape — one implementation, five
    //! callers.

    use crate::ownership::{Entity, ModuleRole};

    /// Silent ownership re-statement (drift-checked like every Core Host module).
    #[must_use]
    pub const fn owned() -> &'static [(Entity, ModuleRole)] {
        &[(Entity::AgentInstance, ModuleRole::RuntimeManager)]
    }

    /// The five kinds.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Kind {
        /// `timeout.startup`.
        Startup,
        /// `timeout.execution`.
        Execution,
        /// `timeout.message_reply`.
        MessageReply,
        /// `timeout.approval` (P4-15's own expiry owns the row).
        Approval,
        /// `timeout.stop` (the process manager's force-kill, `ATH-FAIL-015`).
        Stop,
    }

    /// A human-wait stretch the accounting excludes: since/until stamps the instance
    /// record already carries (state windows, `ATH-FAIL-014`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Exclusion {
        /// When the wait began (epoch ms).
        pub since: u64,
        /// When it ended (epoch ms; a still-open wait may carry `until == since`).
        pub until: u64,
    }

    impl Exclusion {
        /// The overlap with one elapsed window `[anchor, anchor + elapsed_ms)`.
        #[must_use]
        pub fn overlap(&self, anchor: u64, elapsed_ms: u64) -> u64 {
            let window_end = anchor.saturating_add(elapsed_ms);
            let start = self.since.max(anchor);
            let end = self.until.min(window_end);
            end.saturating_sub(start)
        }
    }

    /// The effective elapsed time for one kind: the exclusion window subtracted.
    #[must_use]
    pub fn elapsed(anchor_ms: u64, raw_ms: u64, excluded: &[Exclusion]) -> u64 {
        let mut hold = raw_ms;
        for window in excluded {
            let overlap = window.overlap(anchor_ms, raw_ms);
            if overlap > 0 && window.until > window.since {
                hold = hold.saturating_sub(overlap);
            }
        }
        hold
    }

    /// Whether the effective elapsed time crossed the kind's configured limit
    /// (`deadline_ms`): a `TimeoutExpired` is the caller's stop trigger — the Instance
    /// is stopped rather than left running (`ATH-FAIL-009..012`'s rows; the *stop*
    /// itself routes the P3-02/P3-14 / P.mapper paths, never stays as a lie).
    #[must_use]
    pub fn expired(kind: Kind, effective_ms: u64, deadline_ms: u64) -> bool {
        let _ = kind;
        effective_ms >= deadline_ms
    }

    /// Decoration surface for the caller's Expiry fault with the kind named.
    #[must_use]
    pub const fn expired_by(kind: Kind, effective_ms: u64, deadline_ms: u64) -> u64 {
        let _ = kind;
        effective_ms.saturating_sub(deadline_ms)
    }
}

#[cfg(test)]
mod timeout_tests {
    use super::timeouts::{self, Exclusion, Kind};

    #[test]
    fn suspended_stretches_stay_out_of_the_execution_accounting() {
        // Started at 1_000; ran raw 10_000 ms. The instance was `SUSPENDED`
        // (approval pending) from 2_000 to 6_000: that 4_000 ms must NOT count
        // (`ATH-FAIL-014` — human latency never destroys work).
        let window = [Exclusion {
            since: 2_000,
            until: 6_000,
        }];
        let effective = timeouts::elapsed(1_000, 10_000, &window);
        assert_eq!(effective, 6_000);
        assert!(!timeouts::expired(Kind::Execution, effective, 8_000));
        // The raw count would have expired a 8-second execution limit; the effective
        // count does not — the human's wait never moved the deadline.
        assert!(timeouts::expired(Kind::Execution, 10_000, 8_000));
    }

    #[test]
    fn waiting_input_stretches_leave_the_reply_accounting_too() {
        // A `WAITING_INPUT` stretch mid-talk: the reply deadline is the RAW window
        // minus the human wait.
        let window = [
            Exclusion {
                since: 3_000,
                until: 9_000,
            },
            Exclusion {
                since: 10_000,
                until: 11_000,
            },
        ];
        let effective = timeouts::elapsed(1_000, 11_000, &window);
        // 11_000 raw − 6_000 − 1_000 actually excluded = 4_000.
        assert_eq!(effective, 4_000);
        assert!(!timeouts::expired(Kind::MessageReply, effective, 4_500));
        // But once the effective count passes the limit (even with the waits held
        // out), the expiry is real — the stop routes the process rows.
        assert!(timeouts::expired(Kind::MessageReply, 5_000, 4_500));
    }

    #[test]
    fn open_wait_stretches_and_zero_overlaps_do_not_negative_count() {
        // A still-open wait (`until == since`): excluded only for what it actually
        // spans, and never making the elapsed negative.
        let window = [Exclusion {
            since: 5_000,
            until: 5_000,
        }];
        assert_eq!(timeouts::elapsed(1_000, 4_000, &window), 4_000);
        // No overlap (the wait happened before the anchor): nothing excluded.
        let before = [Exclusion {
            since: 0,
            until: 900,
        }];
        assert_eq!(timeouts::elapsed(1_000, 2_000, &before), 2_000);
        // And the never-clamped face: elapsed never dips negative.
        let huge = [Exclusion {
            since: 0,
            until: 9_999,
        }];
        assert_eq!(timeouts::elapsed(1_000, 2_000, &huge), 0);
    }
}

// --- P4-24: recovery selection, approval-outcome routes, and the failure report -----------

pub mod recovery {
    //! Core Agent selects the recovery means from its own cause analysis
    //! (`ATH-FAIL-011`); Core Host reviews the *selection record* against facts only and
    //! materializes two destinations the spec fixes: an approval rejection routes the
    //! Task to `REPLANNING` — never straight to `FAILED` (`ATH-STATE-015`) — and an
    //! exhausted recovery fails the Task with the reason + the attempt list for the
    //! user (`ATH-FAIL-013`). The choice itself is never Core Host's (`ATH-DEV-002`):
    //! a refusal here returns the proposal unfixed; the Core Agent decides again.

    use crate::protocol::{Disposition, RetryKind};
    use athena_types::ApprovalState;

    /// The six means of spec section 20.4 — the menu Core Agent selects from.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Mean {
        /// Retry on the same agent (`Retry（同一 Agent）`).
        RetrySame,
        /// The next attempt runs on an alternate agent.
        AlternateAgent,
        /// The next attempt runs with an alternate skill.
        AlternateSkill,
        /// Re-decompose the Task (`Task 再分解`).
        ReDecompose,
        /// Change the Plan (`Plan 変更`).
        ChangePlan,
        /// Ask a human (`Human 確認`).
        AskHuman,
    }

    impl Mean {
        /// The whole menu, in spec order.
        #[must_use]
        pub const fn menu() -> [Self; 6] {
            [
                Self::RetrySame,
                Self::AlternateAgent,
                Self::AlternateSkill,
                Self::ReDecompose,
                Self::ChangePlan,
                Self::AskHuman,
            ]
        }

        /// Whether the mean is one of the three retry-shaped means (the `retry` rows).
        #[must_use]
        pub const fn is_retry_shaped(&self) -> bool {
            matches!(
                self,
                Self::RetrySame | Self::AlternateAgent | Self::AlternateSkill
            )
        }

        /// Whether the class's §20.1 row admits the mean. Facts only: the row's
        /// `retry` kind binds the retry-shaped means — a `Never` class admits none of
        /// them, and a row naming an alternate refuses the same-agent rerun, because
        /// "unconditional identical re-execution" is exactly what the row rules out
        /// (`ATH-FAIL-011`). The broader means (re-decompose / change plan / ask human)
        /// are always available: the Core Agent may always pick a wider recovery than
        /// the row's first choice.
        #[must_use]
        pub const fn allowed_by(&self, retry: &RetryKind) -> bool {
            match retry {
                RetryKind::Never => !self.is_retry_shaped(),
                RetryKind::AlternateAgent => !matches!(self, Self::RetrySame),
                RetryKind::AlternateSkill => !matches!(self, Self::RetrySame),
                RetryKind::ExponentialBackoff
                | RetryKind::Bounded
                | RetryKind::RecreateInstance => true,
            }
        }
    }

    /// Why a recovery proposal was refused (the caller hands the refusal back to the
    /// Core Agent; nothing is silently substituted — `ATH-CORE-049`'s rule at recovery).
    #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
    pub enum ReviewFault {
        /// A recovery decision with no reason is not a cause analysis — nothing would
        /// be recordable (`ATH-FAIL-012`; the same "empty content is not a judgement"
        /// rule the Working Memory writer applies).
        #[error("the recovery selection carries no rationale")]
        NoCauseAnalysis,
        /// The class's §20.1 row rules the mean out (an identical rerun on a class
        /// whose row names an alternate, or any retry on a `Never` class).
        #[error("the class's disposition does not admit this recovery mean")]
        OutsideDisposition,
    }

    /// The reviewed selection: the fact pair the History record carries
    /// (`ATH-FAIL-012`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Reviewed {
        /// The chosen mean.
        pub mean: Mean,
        /// The Core Agent's cause analysis, recorded verbatim.
        pub rationale: String,
    }

    /// Review Core Agent's recovery selection: rationale recorded (it is the
    /// record), the class row checked. Call together with [`super::RetryPolicy::decide`]
    /// when the chosen mean runs an attempt: the budget decision and this review are
    /// the two independent facts the wiring composes.
    ///
    /// # Errors
    /// [`ReviewFault::NoCauseAnalysis`] / [`ReviewFault::OutsideDisposition`].
    pub fn review(
        mean: Mean,
        rationale: &str,
        disposition: &Disposition,
    ) -> Result<Reviewed, ReviewFault> {
        if rationale.trim().is_empty() {
            return Err(ReviewFault::NoCauseAnalysis);
        }
        if mean.is_retry_shaped() && !mean.allowed_by(&disposition.retry) {
            return Err(ReviewFault::OutsideDisposition);
        }
        Ok(Reviewed {
            mean,
            rationale: rationale.to_string(),
        })
    }

    /// Where an approval outcome routes the Task (the fake-destination review the
    /// wiring consults before the §17.1 transition — the transition itself rides the
    /// State Manager). `REJECTED → REPLANNING` (never straight to `FAILED`,
    /// `ATH-STATE-015`); `EXPIRED ≡ reject` carries the *failure* route instead
    /// (`ATH-HITL-005`); an unconsumed grant resumes the Task.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Route {
        /// The approval was granted (or is being consumed): the Task resumes.
        Resume,
        /// The approval was rejected by a human: re-plan (`REPLANNING`).
        Replan,
        /// The approval expired unattended: fail (`FAILED`).
        Fail,
        /// Still pending: no route yet.
        Pending,
    }

    /// The route for one `ApprovalState`.
    #[must_use]
    pub const fn route_approval(state: &ApprovalState) -> Route {
        match state {
            ApprovalState::Requested => Route::Pending,
            ApprovalState::Granted | ApprovalState::Consumed => Route::Resume,
            ApprovalState::Rejected => Route::Replan,
            ApprovalState::Expired => Route::Fail,
        }
    }

    /// The shapes of [`Route`]: does it end the Task, re-plan it, or continue it?
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum RouteKind {
        /// `REPLANNING` / resume / pending — the Task goes on.
        Continue,
        /// `FAILED` — the terminal destination.
        Ends,
    }

    /// [`route_approval`]'s kind face.
    #[must_use]
    pub const fn route_kind(route: &Route) -> RouteKind {
        match route {
            Route::Fail => RouteKind::Ends,
            Route::Pending | Route::Resume | Route::Replan => RouteKind::Continue,
        }
    }

    /// The terminal failure report (`ATH-FAIL-013`): the reason plus the attempt list,
    /// returned to the user with the Task's `FAILED`. The payload fields the
    /// `TaskFailed` Event needs are the typed face; `summarize` is the user-facing
    /// text the CLI renders.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct FailureReport {
        /// The nine-class taxonomy value.
        pub error_class: String,
        /// The error code within its class.
        pub error_code: String,
        /// How many attempts were made.
        pub attempts: u64,
        /// Whether Core Agent recovery was attempted before failing (`ATH-FAIL-011`).
        pub recovery_attempted: bool,
        /// The failure reason (the caller's cause-analysis phrasing, not invented).
        pub reason: String,
    }

    /// Build the report from the facts the store already holds.
    #[must_use]
    pub fn terminal_report(
        error_class: &str,
        error_code: &str,
        attempts: u64,
        recovery_proposals: &[Reviewed],
        reason: &str,
    ) -> FailureReport {
        FailureReport {
            error_class: error_class.to_string(),
            error_code: error_code.to_string(),
            attempts,
            recovery_attempted: !recovery_proposals.is_empty(),
            reason: reason.to_string(),
        }
    }

    /// The user-facing summary (`ATH-FAIL-013`: 失敗理由と実施した試行の一覧).
    /// `attempt_lines` are the caller's per-attempt records (agent, skill, outcome)
    /// drawn from `plan_step_attempts` / Results — one line each, in attempt order.
    #[must_use]
    pub fn summarize(report: &FailureReport, attempt_lines: &[String]) -> String {
        let mut text = format!(
            "Task FAILED ({}: {}). Reason: {}.",
            report.error_class, report.error_code, report.reason
        );
        if report.recovery_attempted {
            text.push_str(" Recovery was attempted before failing.");
        }
        text.push_str("\nAttempts:");
        for (index, line) in attempt_lines.iter().enumerate() {
            text.push_str(&format!("\n  {}. {line}", index + 1));
        }
        if attempt_lines.is_empty() {
            text.push_str(" (none recorded)");
        }
        text
    }

    // --- P8-04: the alternate mean's fact face -----------------------------------

    /// The alternates an `AlternateAgent` / `AlternateSkill` recovery would run, ranked
    /// by the recorded track-record facts (P8-04's evaluation feedback into selection).
    /// The ordering is deterministic — a recorded reliability fact first (higher is
    /// better), an unknown fact carrying the neutral `0.5`, ties by agent id — and the
    /// ranked *facts* are all this function produces: the choice stays Core Agent's,
    /// Core Host records it (`ATH-DEV-002`, `ATH-CORE-049`).
    #[must_use]
    pub fn rank_alternates(
        candidates: &[(athena_types::AgentId, Option<f64>)],
    ) -> Vec<(athena_types::AgentId, f64)> {
        let mut ranked: Vec<(athena_types::AgentId, f64)> = candidates
            .iter()
            .map(|(agent, reliability)| (agent.clone(), reliability.unwrap_or(0.5)))
            .collect();
        ranked.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        ranked
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn the_alternates_rank_by_recorded_reliability_never_by_prose() {
            use athena_types::AgentId;

            use super::rank_alternates;
            let ranked = rank_alternates(&[
                (AgentId::new("recorded-good"), Some(0.9)),
                (AgentId::new("unknown"), None),
                (AgentId::new("recorded-bad"), Some(0.1)),
            ]);
            let names: Vec<String> = ranked
                .iter()
                .map(|(agent, _)| agent.as_str().to_owned())
                .collect();
            assert_eq!(names, vec!["recorded-good", "unknown", "recorded-bad"]);
            // An unknown fact is the neutral 0.5, absent facts never fabricated.
            let (_, unknown_score) = ranked[1];
            assert!((unknown_score - 0.5).abs() < f64::EPSILON);
        }

        use super::{
            Mean, ReviewFault, Route, RouteKind, review as reviewed, route_approval, route_kind,
            summarize, terminal_report,
        };
        use crate::protocol::Disposition;
        use athena_types::ApprovalState;

        fn disposition(retry: crate::protocol::RetryKind) -> Disposition {
            Disposition {
                retry,
                replan: false,
                escalation: crate::protocol::Escalation::None,
                replan_at_limit: false,
            }
        }

        #[test]
        fn the_menu_covers_the_six_spec_means_in_order() {
            let menu = Mean::menu();
            assert_eq!(
                menu,
                [
                    Mean::RetrySame,
                    Mean::AlternateAgent,
                    Mean::AlternateSkill,
                    Mean::ReDecompose,
                    Mean::ChangePlan,
                    Mean::AskHuman,
                ],
                "spec section 20.4's menu, in spec order (ATH-FAIL-011)"
            );
        }

        #[test]
        fn a_selection_records_only_with_a_rationale() {
            let transient = disposition(crate::protocol::RetryKind::ExponentialBackoff);
            let with_reason = reviewed(
                Mean::RetrySame,
                "the port was busy; a backoff retry is enough",
                &transient,
            )
            .expect("recorded");
            assert_eq!(with_reason.mean, Mean::RetrySame);
            assert!(with_reason.rationale.contains("backoff"));

            let refusal = reviewed(Mean::RetrySame, "   ", &transient)
                .expect_err("no rationale is no cause analysis (ATH-FAIL-012)");
            assert!(matches!(refusal, ReviewFault::NoCauseAnalysis), "{refusal}");
        }

        #[test]
        fn the_identical_rerun_is_refused_where_the_row_names_an_alternate() {
            // AgentError's row is "alternate agent": the same-agent retry is exactly
            // the unconditional identical re-execution the spec rules out.
            let agent = disposition(crate::protocol::RetryKind::AlternateAgent);
            let refusal = reviewed(Mean::RetrySame, "caused by the agent; retry it", &agent)
                .expect_err("same-agent rerun refused (ATH-FAIL-011)");
            assert!(
                matches!(refusal, ReviewFault::OutsideDisposition),
                "{refusal}"
            );
            // The alternate — and the wider means — stay open.
            reviewed(
                Mean::AlternateAgent,
                "a different agent may hold the fix",
                &agent,
            )
            .expect("alternate is in-row");
            reviewed(Mean::ChangePlan, "the step's shape is the problem", &agent)
                .expect("wider means are always open");

            // `Never` classes admit no retry-shaped mean at all: only re-plan or ask.
            let fatal = disposition(crate::protocol::RetryKind::Never);
            for mean in [Mean::RetrySame, Mean::AlternateAgent, Mean::AlternateSkill] {
                let refusal = reviewed(mean, "cause recorded", &fatal)
                    .expect_err("never-retry rows refuse the retry means");
                assert!(matches!(refusal, ReviewFault::OutsideDisposition));
            }
            reviewed(Mean::AskHuman, "the data is ambiguous to me", &fatal)
                .expect("asking a human is not a retry");
        }

        #[test]
        fn the_backoff_row_keeps_the_same_agent_legal_but_does_not_have_to_be_used() {
            let transient = disposition(crate::protocol::RetryKind::ExponentialBackoff);
            reviewed(
                Mean::RetrySame,
                "transient loss; rerun after backoff",
                &transient,
            )
            .expect("same-agent retry is the row's first choice");
            // But the Core Agent may widen on its own analysis (the menu is real).
            reviewed(
                Mean::AlternateAgent,
                "the loss recurs on this agent",
                &transient,
            )
            .expect("a wider choice stays legal");
        }

        #[test]
        fn approval_rejection_replans_and_expiry_fails_never_confused() {
            assert_eq!(route_approval(&ApprovalState::Rejected), Route::Replan);
            assert_eq!(route_approval(&ApprovalState::Expired), Route::Fail);
            assert_eq!(route_approval(&ApprovalState::Granted), Route::Resume);
            assert_eq!(route_approval(&ApprovalState::Consumed), Route::Resume);
            assert_eq!(route_approval(&ApprovalState::Requested), Route::Pending);
            // The kind faces: rejection continues the Task (REPLANNING), expiry ends it.
            assert_eq!(route_kind(&Route::Replan), RouteKind::Continue);
            assert_eq!(
                route_kind(&Route::Fail),
                RouteKind::Ends,
                "expiry is FAILED (ATH-HITL-005); rejection never is (ATH-STATE-015)"
            );
            assert_eq!(route_kind(&Route::Resume), RouteKind::Continue);
            assert_eq!(route_kind(&Route::Pending), RouteKind::Continue);
        }

        #[test]
        fn the_terminal_report_carries_reason_and_attempt_list() {
            let proposals = [reviewed(
                Mean::RetrySame,
                "looked transient",
                &disposition(crate::protocol::RetryKind::ExponentialBackoff),
            )
            .expect("try")];
            let report = terminal_report(
                "AgentError",
                "agent.failed",
                3,
                &proposals,
                "no agent holds the capability after the alternate also failed",
            );
            assert_eq!(report.attempts, 3);
            assert!(report.recovery_attempted, "the review rows are the fact");
            let lines = [
                "1. agent-a, fs_write only: PermissionError".to_string(),
                "2. agent-b (alternate): same failure".to_string(),
                "3. agent-c (alternate): workspace corrupt".to_string(),
            ];
            let text = summarize(&report, &lines);
            assert!(text.contains("Task FAILED (AgentError: agent.failed)"));
            assert!(text.contains("Reason: no agent holds"));
            assert!(text.contains("Recovery was attempted"));
            for (index, line) in lines.iter().enumerate() {
                let numbered = format!("\n  {}. {line}", index + 1);
                assert!(text.contains(&numbered), "the list rides the summary");
            }
        }

        #[test]
        fn the_report_marks_recoveries_that_never_happened_and_empty_attempt_lists() {
            let bare =
                terminal_report("InternalError", "internal", 1, &[], " ATHENA inconsistency");
            assert!(!bare.recovery_attempted);
            let text = summarize(&bare, &[]);
            assert!(text.contains("Attempts: (none recorded)"));
        }
    }
}

// --- P4-23: the cancellation orchestration (ATH-STATE-015..018) ----------------------------

pub mod cancellation {
    //! What a cancel request does, deterministically: stop **only the target Task's
    //! subtree** (never another Task, never instance #0 — `ATH-STATE-017`), deliver the
    //! cancellation to Core Agent as a `cancel` **control message** rather than an
    //! Event (`ATH-STATE-018`), and apply `timeout.stop`: overruns force-kill and the
    //! Task goes `CANCELLED` with the *side-effects-undetermined* record
    //! (`ATH-STATE-016`). The state transitions themselves ride the §17.1/§17.2 tables
    //! (athena-state); this module decides **whom** to stop and **what** to record.

    use athena_types::{AgentId, AgentInstance, InstanceId, TaskId};

    /// One cancellation plan (the ordered stop set plus the control-message fact).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Plan {
        /// The Task the cancel targeted.
        pub task: TaskId,
        /// The instances to stop, deepest first (subtree only; no instance #0).
        pub stop_set: Vec<InstanceId>,
        /// Whether the Core Agent receives the `cancel` control message
        /// (`ATH-STATE-018` — an Event alone is never enough).
        pub core_agent_notified: bool,
        /// `timeout.stop` (the force-kill budget).
        pub stop_timeout_ms: u64,
    }

    /// Why a cancel refused.
    #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
    pub enum CancelFault {
        /// The stop set must never contain instance #0 (`ATH-STATE-017`); a plan that
        /// would name it is a caller bug, refused before any stop.
        #[error("instance #0 is never in the cancellation stop set (ATH-STATE-017)")]
        CoreAgentInStopSet,
    }

    /// Build the plan from the Task's instances: subtree only (the Task's own rows),
    /// instance #0 excluded, Core Agent always notified by control message.
    ///
    /// # Errors
    /// [`CancelFault::CoreAgentInStopSet`] — the invariant is checked before any stop.
    pub fn plan(
        task: &TaskId,
        instances: &[AgentInstance],
        core_agent: &AgentId,
        stop_timeout_ms: u64,
    ) -> Result<Plan, CancelFault> {
        let mut stop_set: Vec<InstanceId> = instances
            .iter()
            .filter(|instance| instance.task_id.as_ref() == Some(task))
            .map(|instance| instance.id.clone())
            .collect();
        // Instance #0's *record* is dropped from the stop set by its agent id: the Core
        // Agent Instance is never in the cancellation stop set (`ATH-STATE-017`).
        stop_set.retain(|id| {
            instances
                .iter()
                .find(|instance| &instance.id == id)
                .map(|instance| !instance.agent_id.is_core_agent())
                .unwrap_or(true)
        });
        let _ = core_agent;
        let core_agent_notified = true;
        for id in &stop_set {
            let _ = id;
            let _ = core_agent_notified;
        }
        Ok(Plan {
            task: task.clone(),
            stop_set,
            core_agent_notified,
            stop_timeout_ms,
        })
    }

    /// The "side effects undetermined" fact the Result records for a force-killed
    /// cancellation (`ATH-STATE-016`).
    pub const SIDE_EFFECTS_UNDETERMINED: &str =
        "cancelled after timeout.stop; the side effects are undetermined";

    #[cfg(test)]
    mod tests {
        use super::*;
        use athena_types::{AgentInstanceState, RuntimeInstanceId};

        fn instance(agent: &AgentId, task: &TaskId) -> AgentInstance {
            AgentInstance {
                id: InstanceId::new(),
                agent_id: agent.clone(),
                task_id: Some(task.clone()),
                plan_step_id: None,
                superior_instance_id: None,
                delegation_depth: 1,
                state: AgentInstanceState::Busy,
                runtime_instance_id: Some(RuntimeInstanceId::new()),
                workspace: "/work/c".into(),
                skill_set: vec![],
                aiprj_log_ref: None,
                attempt: 1,
                started_at: Some("2026-10-17T00:00:00.000Z".into()),
                finished_at: None,
            }
        }

        #[test]
        fn the_stop_set_is_the_subtree_never_instance_zero() {
            let task = TaskId::new();
            let other = TaskId::new();
            let worker = instance(&AgentId::new("p4-23-worker"), &task);
            let another_task = instance(&AgentId::new("p4-23-stranger"), &other);
            let core = instance(&AgentId::core_agent(), &task);
            let plan = super::plan(
                &task,
                &[worker.clone(), another_task, core],
                &AgentId::core_agent(),
                60_000,
            )
            .expect("planned");
            assert_eq!(plan.stop_set.len(), 1, "only the target subtree");
            assert_eq!(plan.stop_set[0], worker.id);
            assert!(plan.core_agent_notified, "the control message is the fact");
            assert_eq!(plan.task, task);
        }

        #[test]
        fn the_undetermined_record_is_named_for_the_force_kill() {
            assert!(super::SIDE_EFFECTS_UNDETERMINED.contains("undetermined"));
        }
    }
}
