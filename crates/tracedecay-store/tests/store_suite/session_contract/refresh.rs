use super::common::*;
use super::*;

#[test]
fn refresh_begin_request_preserves_temporal_coverage_mode() {
    let session_id = session("session.coverage-mode");
    let frontier = SessionRefreshFrontierV1::new(10, 8).unwrap();
    let current = SessionRefreshBeginOrJoinRequestV1::new(session_id.clone(), frontier);
    let forensic = SessionRefreshBeginOrJoinRequestV1::new(session_id, frontier)
        .with_coverage_request(tracedecay_domain::SessionTemporalCoverageRequestV1::new(
            tracedecay_domain::TemporalModeV1::Forensic,
        ));

    assert_eq!(
        current.coverage_request().mode(),
        tracedecay_domain::TemporalModeV1::Current
    );
    assert_eq!(
        forensic.coverage_request().mode(),
        tracedecay_domain::TemporalModeV1::Forensic
    );
    assert!(current.is_equivalent_to(&forensic));
}

#[test]
fn refresh_progress_is_persistable_and_terminal_receipts_preserve_coverage() {
    let session_id = session("session.fixture");
    let operation_id = operation_id();
    let frontier = SessionRefreshFrontierV1::new(10, 8).unwrap();
    let progress = SessionRefreshProgressV1::new(
        operation_id.clone(),
        session_id.clone(),
        frontier,
        coverage(),
        2,
        8,
        UtcMicros(100),
    );
    assert_eq!(progress.session_id(), &session_id);
    assert_eq!(progress.coverage(), &coverage());

    let completion = SessionRefreshCompletionRequestV1::new(
        operation_id.clone(),
        session_id.clone(),
        SessionRefreshFrontierV1::new(10, 10).unwrap(),
        coverage(),
    )
    .unwrap();
    let receipt = SessionRefreshReceiptV1::completed(completion, UtcMicros(110));
    assert_eq!(receipt.state(), SessionRefreshTerminalStateV1::Complete);
    assert_eq!(receipt.frontier().committed_through(), 10);
    assert_eq!(receipt.coverage(), &coverage());

    assert!(matches!(
        SessionRefreshCompletionRequestV1::new(operation_id, session_id, frontier, coverage(),),
        Err(SessionStoreError::InvalidRefreshState { .. })
    ));
}

#[test]
fn refresh_failure_and_cancellation_return_terminal_receipts() {
    let session_id = session("session.fixture");
    let operation_id = operation_id();
    let frontier = SessionRefreshFrontierV1::new(10, 8).unwrap();
    let failure = SessionRefreshFailureRequestV1::new(
        operation_id.clone(),
        session_id.clone(),
        frontier,
        coverage(),
        "source_unavailable",
    )
    .unwrap();
    let failed = SessionRefreshReceiptV1::failed(failure, UtcMicros(110));
    assert_eq!(failed.state(), SessionRefreshTerminalStateV1::Failed);
    assert_eq!(
        failed
            .failure_code()
            .map(SessionRefreshFailureCodeV1::as_str),
        Some("source_unavailable")
    );

    let cancellation =
        SessionRefreshCancellationRequestV1::new(operation_id, session_id, frontier, coverage());
    let cancelled = SessionRefreshReceiptV1::cancelled(cancellation, UtcMicros(111));
    assert_eq!(cancelled.state(), SessionRefreshTerminalStateV1::Cancelled);
    assert_eq!(cancelled.frontier(), frontier);
    assert_eq!(cancelled.coverage(), &coverage());
}

#[test]
fn refresh_failure_codes_are_bounded_non_sensitive_and_canonical() {
    let max_length_code = "a".repeat(SessionRefreshFailureCodeV1::MAX_LEN);
    let code = SessionRefreshFailureCodeV1::new(max_length_code.clone()).unwrap();
    assert_eq!(code.as_str(), max_length_code);
    assert_eq!(code.to_string(), max_length_code);

    for (value, reason) in [
        (
            String::new(),
            SessionRefreshFailureCodeInvalidReasonV1::Empty,
        ),
        (
            "a".repeat(SessionRefreshFailureCodeV1::MAX_LEN + 1),
            SessionRefreshFailureCodeInvalidReasonV1::TooLong,
        ),
        (
            "source\nunavailable".to_owned(),
            SessionRefreshFailureCodeInvalidReasonV1::ContainsControl,
        ),
        (
            "Source_Unavailable".to_owned(),
            SessionRefreshFailureCodeInvalidReasonV1::NonCanonical,
        ),
        (
            "source__unavailable".to_owned(),
            SessionRefreshFailureCodeInvalidReasonV1::NonCanonical,
        ),
        (
            "source-unavailable".to_owned(),
            SessionRefreshFailureCodeInvalidReasonV1::NonCanonical,
        ),
    ] {
        let error = SessionRefreshFailureCodeV1::new(value).unwrap_err();
        assert!(matches!(
            &error,
            SessionStoreError::InvalidRefreshFailureCode {
                reason: actual_reason
            } if *actual_reason == reason
        ));
        assert!(!error.to_string().contains("source"));
    }
}

#[test]
fn refresh_failure_codes_round_trip_through_validated_json() {
    let code = SessionRefreshFailureCodeV1::new("source_unavailable").unwrap();
    let encoded = serde_json::to_string(&code).unwrap();
    assert_eq!(encoded, "\"source_unavailable\"");
    let decoded: SessionRefreshFailureCodeV1 = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, code);
    assert_eq!(decoded.as_str(), "source_unavailable");
}

#[test]
fn refresh_failure_code_json_rejects_empty_oversized_control_and_sensitive_shapes() {
    for value in [
        json!(""),
        json!("a".repeat(SessionRefreshFailureCodeV1::MAX_LEN + 1)),
        json!("source\nunavailable"),
        json!("Source_Unavailable"),
        json!("source__unavailable"),
        json!("source-unavailable"),
        json!("source unavailable"),
        json!("path=/var/secret"),
        json!("user@host"),
    ] {
        let error = serde_json::from_value::<SessionRefreshFailureCodeV1>(value).unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("invalid non-sensitive refresh failure code")
                || message.contains("Empty")
                || message.contains("TooLong")
                || message.contains("ContainsControl")
                || message.contains("NonCanonical"),
            "unexpected deserialize error: {message}"
        );
        assert!(!message.contains("/var/secret"), "{message}");
        assert!(!message.contains("user@host"), "{message}");
    }
}

#[test]
fn invalid_refresh_state_errors_carry_the_exhaustive_typed_state() {
    let error = SessionRefreshCompletionRequestV1::new(
        operation_id(),
        session("session.fixture"),
        SessionRefreshFrontierV1::new(10, 8).unwrap(),
        coverage(),
    )
    .unwrap_err();

    assert!(matches!(
        &error,
        SessionStoreError::InvalidRefreshState {
            state: SessionRefreshStateV1::Running,
            ..
        }
    ));
    assert!(error.to_string().contains("Running"));
}

#[test]
fn refresh_frontiers_and_progress_are_monotonic_and_terminal() {
    assert!(matches!(
        SessionRefreshFrontierV1::new(7, 8),
        Err(SessionStoreError::InvalidRefreshFrontier {
            observed_through: 7,
            committed_through: 8,
        })
    ));

    let session_id = session("session.refresh-monotonic");
    let initial = SessionRefreshProgressV1::new(
        operation_id(),
        session_id.clone(),
        SessionRefreshFrontierV1::new(10, 8).unwrap(),
        coverage(),
        1,
        8,
        UtcMicros(100),
    );
    let regressed = SessionRefreshProgressV1::new(
        operation_id(),
        session_id.clone(),
        SessionRefreshFrontierV1::new(10, 7).unwrap(),
        coverage(),
        2,
        9,
        UtcMicros(101),
    );
    assert!(matches!(
        initial.validate_successor(&regressed),
        Err(SessionStoreError::InvalidStateTransition {
            context: "refresh progress successor"
        })
    ));

    let terminal = SessionRefreshReceiptV1::completed(
        SessionRefreshCompletionRequestV1::new(
            operation_id(),
            session_id,
            SessionRefreshFrontierV1::new(10, 10).unwrap(),
            coverage(),
        )
        .unwrap(),
        UtcMicros(102),
    );
    assert!(terminal.validate_transition_from(&initial).is_ok());
}
