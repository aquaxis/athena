//! Tests for the high-risk skill approval route (P5-12).

use super::*;
use athena_types::{ApprovalState, RiskLevel};

#[test]
fn low_and_medium_risks_proceed_without_an_approval() {
    for risk in [RiskLevel::Low, RiskLevel::Medium] {
        assert!(approve_skill_use("calm", risk, None).is_ok());
    }
}

#[test]
fn a_high_risk_skill_without_a_record_is_held() {
    assert!(matches!(
        approve_skill_use("dangerous", RiskLevel::High, None),
        Err(SkillUseFault::ApprovalRequired(_))
    ));
    // A pending REQUEST is held too — never an implicit grant.
    assert!(matches!(
        approve_skill_use("dangerous", RiskLevel::High, Some(ApprovalState::Requested)),
        Err(SkillUseFault::ApprovalRequired(_))
    ));
}

#[test]
fn a_grant_admits_the_use() {
    assert!(approve_skill_use("dangerous", RiskLevel::High, Some(ApprovalState::Granted)).is_ok());
}

#[test]
fn rejection_and_expiry_deny_the_use() {
    let fault = approve_skill_use("dangerous", RiskLevel::High, Some(ApprovalState::Rejected))
        .expect_err("denied");
    assert!(matches!(
        fault,
        SkillUseFault::Denied(_, "rejected by the human")
    ));

    let fault = approve_skill_use("dangerous", RiskLevel::High, Some(ApprovalState::Expired))
        .expect_err("expired");
    assert!(matches!(
        fault,
        SkillUseFault::Denied(_, "expired; silence is rejection")
    ));
}

#[test]
fn a_consumed_record_demands_a_fresh_approval() {
    assert!(matches!(
        approve_skill_use("dangerous", RiskLevel::High, Some(ApprovalState::Consumed)),
        Err(SkillUseFault::Consumed(_))
    ));
}
