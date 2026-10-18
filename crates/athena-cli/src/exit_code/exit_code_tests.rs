//! Tests for the exit-code contract (P6-05).

use super::ErrorClass;
use super::*;

#[test]
fn the_codes_are_the_contract_x008_values() {
    assert_eq!(ExitCode::Success.as_i32(), 0);
    assert_eq!(ExitCode::RequestShapeViolation.as_i32(), 2);
    assert_eq!(ExitCode::PermissionRefusal.as_i32(), 3);
    assert_eq!(ExitCode::ExecutionFailure.as_i32(), 4);
    assert_eq!(ExitCode::InternalError.as_i32(), 70);
}

#[test]
fn the_class_face_maps_onto_the_codes() {
    assert_eq!(
        ExitCode::from_error_class(&ErrorClass::ValidationError),
        ExitCode::RequestShapeViolation
    );
    for refusal in [ErrorClass::PermissionError, ErrorClass::PolicyError] {
        assert_eq!(
            ExitCode::from_error_class(&refusal),
            ExitCode::PermissionRefusal
        );
    }
    assert_eq!(
        ExitCode::from_error_class(&ErrorClass::InternalError),
        ExitCode::InternalError
    );
    // The execution family (timeout / agent / runtime / transient / skill).
    for execution in [
        ErrorClass::TransientError,
        ErrorClass::TimeoutError,
        ErrorClass::AgentError,
        ErrorClass::SkillError,
        ErrorClass::RuntimeError,
    ] {
        assert_eq!(
            ExitCode::from_error_class(&execution),
            ExitCode::ExecutionFailure
        );
    }
}

#[test]
fn the_outcome_face_carries_the_mapped_code() {
    let outcome = outcome_of_class(&ErrorClass::ValidationError, "the pin is mandatory");
    assert_eq!(outcome.code, 2);
    let outcome = outcome_of_class(&ErrorClass::PermissionError, "admin only");
    assert_eq!(outcome.code, 3);
}
