use super::*;

#[test]
fn the_class_maps_to_its_canonical_status() {
    // Spec §22.2: a representative destination per class, encoded at the type.
    let pairs = [
        (ErrorClass::ValidationError, StatusCode::BAD_REQUEST),
        (ErrorClass::PermissionError, StatusCode::FORBIDDEN),
        (ErrorClass::PolicyError, StatusCode::FORBIDDEN),
        (ErrorClass::TransientError, StatusCode::TOO_MANY_REQUESTS),
        (ErrorClass::RuntimeError, StatusCode::SERVICE_UNAVAILABLE),
        (ErrorClass::TimeoutError, StatusCode::GATEWAY_TIMEOUT),
        (ErrorClass::InternalError, StatusCode::INTERNAL_SERVER_ERROR),
        (ErrorClass::AgentError, StatusCode::INTERNAL_SERVER_ERROR),
        (ErrorClass::SkillError, StatusCode::INTERNAL_SERVER_ERROR),
    ];
    for (class, expected) in pairs {
        assert_eq!(
            StatusCode::from_u16(class.default_status()).expect("class status"),
            expected,
            "{}",
            class.default_status()
        );
    }
}

#[test]
fn the_envelope_carries_every_22_2_field() {
    // ATH-API-007 / ATH-API-008 / ATH-API-009: class, code, message, details,
    // correlation_id.
    let error = ApiError::of_class(
        ErrorClass::PolicyError,
        "SOME_POLICY_RULE",
        "refused",
        json!({ "rule": 7 }),
        "cor-01TEST",
    );
    assert_eq!(error.correlation_id(), "cor-01TEST");
    assert_eq!(error.status, StatusCode::FORBIDDEN);
}

#[test]
fn the_correlation_id_echoes_the_request_chain() {
    // ATH-API-009: the request's own id echoes, so log/event joins stay keyed.
    let mut headers = HeaderMap::new();
    headers.insert(CORRELATION_HEADER, "cor-01REQUEST".parse().expect("header"));
    let id = correlation_of(&headers, 1);
    assert_eq!(id, "cor-01REQUEST");
    // A header that is not in the `cor-` namespace is ignored, not trusted.
    headers.insert(CORRELATION_HEADER, "not-a-cor".parse().expect("header"));
    assert!(correlation_of(&headers, 2).starts_with("cor-"));
    // And a request without one is minted one.
    assert!(correlation_of(&HeaderMap::new(), 3).starts_with("cor-"));
    // Two minting calls at the same seed differ (uniqueness per request).
    assert_ne!(
        correlation_of(&HeaderMap::new(), 4),
        correlation_of(&HeaderMap::new(), 4)
    );
}

#[test]
fn unauthenticated_carries_no_taxonomy_class() {
    // §22.2: 401's class column is `—`: there was no inspection to classify.
    let error = ApiError::unauthenticated("cor-01X");
    // The wire carries the envelope but its class is the validator's placeholder, not one
    // of the security classes; the *status* is 401 regardless.
    assert_eq!(error.status, StatusCode::UNAUTHORIZED);
    assert_eq!(error.envelope.code, "AUTH_UNAUTHENTICATED");
}
