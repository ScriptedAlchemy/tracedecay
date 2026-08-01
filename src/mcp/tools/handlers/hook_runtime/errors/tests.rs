use std::io;

use tracedecay_domain::{CanonicalObservationIdV1, ObservationCollisionOutcomeV1, PayloadDigestV1};
use tracedecay_store::{ObservationStoreError, ProjectionStoreError};

use crate::application::observation::{
    CaptureClaudeObservationRequestError, ObservationApplicationError,
};

use super::*;

#[test]
fn hook_error_response_fixtures_are_legal_and_redacted() {
    let secret = "hook-secret-error-fixture";
    let fixtures = [
        ("malformed", "malformed_event", false, "degraded"),
        ("unknown-version", "unknown_version", false, "degraded"),
        ("degraded", "source_degraded", true, "degraded"),
        ("no-source", "source_unavailable", true, "degraded"),
        (
            "repeated-delivery",
            "observation_duplicate",
            false,
            "degraded",
        ),
    ];

    for (fixture, reason_code, retryable, status) in fixtures {
        let error = TraceDecayError::hook_runtime(
            reason_code,
            retryable,
            format!("transcript fixture {fixture} failed"),
        );
        let data = structured_hook_error_data(&error).unwrap();
        let snapshot = data.to_string();

        assert_eq!(data["tool"], "tracedecay_hook_runtime", "{fixture}");
        assert_eq!(data["status"], status, "{fixture}");
        assert_eq!(data["reason_code"], reason_code, "{fixture}");
        assert_eq!(data["retryable"], retryable, "{fixture}");
        assert!(!error.to_string().contains(secret), "{fixture}");
        assert!(!snapshot.contains(secret), "{fixture}");
    }
}

#[test]
fn claude_observation_request_errors_are_bounded_hook_errors() {
    let error = ClaudeObservationIngestError::Request(
        CaptureClaudeObservationRequestError::SourceRangeMismatch,
    );
    let mapped = map_claude_observation_ingest_error(&error);
    let rendered = mapped.to_string();

    assert!(rendered.contains("Claude observation request is invalid"));
    assert!(!rendered.contains("source range"));
    let data = structured_hook_error_data(&mapped).unwrap();
    assert_eq!(data["status"], "degraded");
    assert_eq!(data["reason_code"], "observation_request_invalid");
    assert_eq!(data["retryable"], false);
}

#[test]
fn claude_observation_store_errors_keep_bounded_context_without_source_detail() {
    let error = ClaudeObservationIngestError::Store(ObservationStoreError::Storage {
        operation: "private store operation",
        source: Box::new(io::Error::other("private store source detail")),
    });
    let mapped = map_claude_observation_ingest_error(&error);
    let rendered = mapped.to_string();

    assert!(rendered.contains("Claude observation store operation failed"));
    assert!(!rendered.contains("private store operation"));
    assert!(!rendered.contains("private store source detail"));
    let data = structured_hook_error_data(&mapped).unwrap();
    assert_eq!(data["status"], "unavailable");
    assert_eq!(data["reason_code"], "observation_storage_failed");
    assert_eq!(data["retryable"], true);
}

#[test]
fn claude_observation_application_store_errors_keep_bounded_context() {
    let error = ClaudeObservationIngestError::Application(ObservationApplicationError::Store(
        ObservationStoreError::Storage {
            operation: "private application store operation",
            source: Box::new(io::Error::other("private application store source detail")),
        },
    ));
    let mapped = map_claude_observation_ingest_error(&error);
    let rendered = mapped.to_string();

    assert!(rendered.contains("Claude observation application failed"));
    assert!(!rendered.contains("private application store operation"));
    assert!(!rendered.contains("private application store source detail"));
}

#[test]
fn unavailable_persisted_observation_is_a_bounded_hook_error() {
    let error = ClaudeObservationIngestError::Application(
        ObservationApplicationError::PersistedObservationUnavailable,
    );
    let mapped = map_claude_observation_ingest_error(&error);
    let rendered = mapped.to_string();

    assert!(rendered.contains("Claude observation application failed"));
    assert!(!rendered.contains("persisted Claude observation"));
}

#[test]
fn claude_observation_projection_errors_keep_bounded_context_without_source_detail() {
    let error = ClaudeObservationIngestError::Projection(ProjectionStoreError::Storage {
        operation: "private projection operation",
        source: Box::new(io::Error::other("private projection source detail")),
    });
    let mapped = map_claude_observation_ingest_error(&error);
    let rendered = mapped.to_string();

    assert!(rendered.contains("Claude observation projection failed"));
    assert!(!rendered.contains("private projection operation"));
    assert!(!rendered.contains("private projection source detail"));
}

#[test]
fn claude_observation_failures_expose_stable_retry_contracts() {
    let cases = [
        (
            ClaudeObservationIngestError::Store(ObservationStoreError::CursorConflict {
                expected: Box::new(None),
                actual: Box::new(None),
            }),
            "observation_cursor_conflict",
            true,
        ),
        (
            ClaudeObservationIngestError::Store(ObservationStoreError::ObservationCollision {
                observation_id: Box::new(
                    CanonicalObservationIdV1::new(format!("sha256:{}", "1".repeat(64))).unwrap(),
                ),
                existing_digest: Box::new(
                    PayloadDigestV1::new(format!("sha256:{}", "2".repeat(64))).unwrap(),
                ),
                candidate_digest: Box::new(
                    PayloadDigestV1::new(format!("sha256:{}", "3".repeat(64))).unwrap(),
                ),
                outcome: ObservationCollisionOutcomeV1::IdentityCollision,
            }),
            "observation_identity_collision",
            false,
        ),
        (
            ClaudeObservationIngestError::Store(
                ObservationStoreError::SanitizationReceiptCollision,
            ),
            "sanitization_receipt_collision",
            false,
        ),
        (
            ClaudeObservationIngestError::Application(ObservationApplicationError::Cancelled),
            "observation_cancelled",
            true,
        ),
        (
            ClaudeObservationIngestError::Projection(ProjectionStoreError::Gap {
                expected: 4,
                actual: 6,
            }),
            "observation_projection_checkpoint_gap",
            false,
        ),
    ];

    for (error, reason_code, retryable) in cases {
        let mapped = map_claude_observation_ingest_error(&error);
        let data = structured_hook_error_data(&mapped).unwrap();
        assert_eq!(data["reason_code"], reason_code);
        assert_eq!(data["retryable"], retryable);
    }
}

#[test]
fn transcript_hook_errors_keep_bounded_retry_data_without_cursor_detail() {
    let error = crate::sessions::source::TranscriptIngestError::CursorKeyMismatch {
        expected: "private expected cursor".to_string(),
        actual: "private actual cursor".to_string(),
    };
    let mapped = map_transcript_ingest_error(&error);
    let data = structured_hook_error_data(&mapped).unwrap();

    assert_eq!(data["reason_code"], "transcript_cursor_key_mismatch");
    assert_eq!(data["retryable"], false);
    let rendered = data.to_string();
    assert!(!rendered.contains("private expected cursor"));
    assert!(!rendered.contains("private actual cursor"));
}
