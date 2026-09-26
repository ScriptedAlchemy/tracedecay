#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use tracedecay_domain::{
    ObservationOrderingDomainV1, ObservationScopeV1, ObservationSourceGenerationV1,
    ObservationSourceIdentityV1, ObservationSourceRangeV1, ProjectId, ProviderId, SessionId,
};
use tracedecay_store::{
    CursorAdvanceLedgerDisagreementV1, CursorAdvanceLedgerIdentityV1, ObservationCoverageReason,
    ObservationCoverageV1, ObservationStoreError,
};

use crate::observation::ObservationCancellation;
use crate::runtime::shared::TranscriptIngestStats;
use crate::runtime::{SessionProvider, hosts::claude_observation, hosts::codex, source};

use super::failure::{
    IngestPassBounds, IngestPassCoverage, allocate_pass_byte_budgets,
    classify_claude_observation_failure, classify_transcript_ingest_failure,
    plan_round_robin_admission, scheduling_write_required,
};
use super::project::{home_dir, with_transcript_source_home};
use super::scheduler::{
    finish_user_provider_coverage, merge_project_provider_backpressure,
    plan_provider_rotation_admission,
};
use super::startup::{StartupUserIngestClaim, StartupUserIngestGuard, TranscriptIngestOutcome};
use super::user::provider_selected;

const TEST_INGEST_BOUNDS: IngestPassBounds = IngestPassBounds {
    discovered_units: 16,
    units_per_pass: 8,
    units_per_source: 8,
    queue_depth: 8,
    bytes_per_unit: 1024,
    bytes_per_pass: 4096,
    retries: 0,
};
#[tokio::test]
async fn scoped_transcript_source_home_overrides_ambient_home_without_mutating_it() {
    let isolated_home = tempfile::tempdir().unwrap();
    let ambient_home = std::env::var_os("HOME");

    let resolved =
        with_transcript_source_home(isolated_home.path().to_path_buf(), async { home_dir() }).await;

    assert_eq!(resolved.as_deref(), Some(isolated_home.path()));
    assert_eq!(std::env::var_os("HOME"), ambient_home);
}

#[tokio::test]
async fn cancelled_codex_provider_stops_before_opening_the_next_jsonl_source() {
    use crate::admission::test_support::PanicHostAdmission;

    let temp = tempfile::tempdir().unwrap();
    let project_id = ProjectId::new("scheduler-test-cancelled").unwrap();
    let cancellation = ObservationCancellation::default();
    cancellation.cancel();

    let outcome =
        codex::try_admit_codex_jsonl_observations_for_project_with_admission_and_cancellation(
            &temp.path().join("must-not-open.jsonl"),
            temp.path(),
            project_id,
            &PanicHostAdmission,
            None,
            &cancellation,
        )
        .await
        .unwrap();

    assert_eq!(outcome.bytes_consumed, 0);
    assert!(outcome.source_deferred);
}

#[test]
fn provider_scoped_user_catch_up_excludes_unrelated_providers() {
    assert!(provider_selected(
        Some(SessionProvider::Hermes),
        SessionProvider::Hermes
    ));
    for unrelated in [
        SessionProvider::Codex,
        SessionProvider::Cursor,
        SessionProvider::Claude,
        SessionProvider::Vibe,
        SessionProvider::Cline,
        SessionProvider::RooCode,
        SessionProvider::Kilo,
        SessionProvider::Kiro,
    ] {
        assert!(!provider_selected(Some(SessionProvider::Hermes), unrelated));
    }
    assert!(provider_selected(None, SessionProvider::Codex));
    assert!(provider_selected(None, SessionProvider::Hermes));
}

#[test]
fn transcript_failure_classification_is_bounded_and_drives_outcome_success() {
    let error =
        source::TranscriptIngestError::Store(tracedecay_store::TranscriptStoreError::Storage {
            operation: "private operation",
            source: Box::new(std::io::Error::other("private source detail")),
        });
    let failure = classify_transcript_ingest_failure("codex", "transcript", &error);

    assert_eq!(failure.provider, "codex");
    assert_eq!(failure.source, "transcript");
    assert_eq!(failure.reason_code, "transcript_storage_failed");
    assert!(failure.retryable);
    let outcome = TranscriptIngestOutcome::new(TranscriptIngestStats::default(), vec![failure]);
    assert!(!outcome.coverage.is_complete() || !outcome.failures.is_empty());
    let rendered = serde_json::to_string(&outcome.failures).unwrap();
    assert!(!rendered.contains("private operation"));
    assert!(!rendered.contains("private source detail"));
}

#[test]
fn transcript_contract_failures_are_not_retryable() {
    let error = source::TranscriptIngestError::CursorKeyMismatch {
        expected: "private expected key".to_string(),
        actual: "private actual key".to_string(),
    };
    let failure = classify_transcript_ingest_failure("cursor", "hook", &error);

    assert_eq!(failure.reason_code, "transcript_cursor_key_mismatch");
    assert!(!failure.retryable);
}

#[test]
fn cursor_advance_receipt_collisions_are_permanent() {
    let error = claude_observation::ClaudeObservationIngestError::Store(
        tracedecay_store::ObservationStoreError::CursorAdvanceCollision,
    );

    let failure = classify_claude_observation_failure(&error);

    assert_eq!(failure.reason_code, "observation_cursor_advance_collision");
    assert!(!failure.retryable);
}

#[test]
fn immutable_cursor_ledger_disagreements_are_permanent_and_bounded() {
    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new("cursor").unwrap(),
        SessionId::new("session.fixture").unwrap(),
    )
    .unwrap();
    let coverage = ObservationCoverageV1::new(
        ObservationSourceGenerationV1::new(7).unwrap(),
        ObservationOrderingDomainV1::FileBytes,
        ObservationSourceRangeV1::new(10, 20).unwrap(),
    );
    let disagreement = CursorAdvanceLedgerDisagreementV1::new(
        source,
        ObservationScopeV1::Profile,
        coverage,
        CursorAdvanceLedgerIdentityV1::new(ObservationCoverageReason::BlankFrame, None),
        CursorAdvanceLedgerIdentityV1::new(ObservationCoverageReason::OutOfScope, None),
    );
    let error = claude_observation::ClaudeObservationIngestError::Store(
        ObservationStoreError::CursorAdvanceLedgerDisagreement {
            disagreement: Box::new(disagreement),
        },
    );

    let failure = classify_claude_observation_failure(&error);

    assert_eq!(
        failure.reason_code,
        "observation_cursor_advance_ledger_disagreement"
    );
    assert!(!failure.retryable);
    assert_eq!(
        failure.status,
        crate::admission::HostAdmissionStatus::Degraded
    );
}

#[test]
fn transcript_privacy_and_non_durable_failures_are_bounded_and_permanent() {
    let privacy = source::TranscriptIngestError::Privacy(
        tracedecay_privacy::PrivacySanitizerError::InvalidPolicy,
    );
    let privacy = classify_transcript_ingest_failure("claude", "hook", &privacy);
    assert_eq!(privacy.reason_code, "transcript_privacy_rejected");
    assert!(!privacy.retryable);

    let non_durable = source::TranscriptIngestError::NonDurableRecord {
        provider: "claude",
        offset: 7,
        end_offset: 99,
        reason: "private detail",
    };
    let non_durable = classify_transcript_ingest_failure("claude", "hook", &non_durable);
    assert_eq!(non_durable.reason_code, "transcript_record_non_durable");
    assert!(!non_durable.retryable);
    let serialized = serde_json::to_string(&non_durable).unwrap();
    assert!(!serialized.contains("private detail"));
    assert!(serialized.contains(r#""source_locator":{"start":7,"end":99}"#));

    let controlled = source::TranscriptIngestError::NonDurableRecord {
        provider: "claude",
        offset: 11,
        end_offset: 23,
        reason: "malformed snapshot JSON",
    };
    let controlled = classify_transcript_ingest_failure("claude", "snapshot", &controlled);
    assert_eq!(controlled.reason_code, "malformed_snapshot_json");
    assert_eq!(controlled.source_locator.unwrap().start(), 11);
    assert_eq!(controlled.source_locator.unwrap().end(), 23);

    let bounded_code = source::TranscriptIngestError::NonDurableRecord {
        provider: "claude",
        offset: 0,
        end_offset: 0,
        reason: "authority_unavailable",
    };
    let bounded_code = classify_transcript_ingest_failure("claude", "hook", &bounded_code);
    assert_eq!(bounded_code.reason_code, "authority_unavailable");
    assert_eq!(bounded_code.source_locator, None);
}

#[test]
fn still_mounting_admission_failures_keep_the_admission_retryability() {
    let error = crate::runtime::snapshot_observation::host_admission_error(
        "codex",
        crate::admission::HostAdmissionOutcome {
            status: crate::admission::HostAdmissionStatus::Unavailable,
            retryable: true,
            reason_code: Some("authority_write_failed"),
            recovery: None,
            cause: None,
        },
    );

    let failure = classify_transcript_ingest_failure("codex", "observation", &error);

    assert_eq!(failure.reason_code, "authority_write_failed");
    assert!(
        failure.retryable,
        "a still-mounting write authority is a transient race, not a terminal block"
    );
}

#[test]
fn permanent_admission_failures_still_classify_permanent() {
    let error = crate::runtime::snapshot_observation::host_admission_error(
        "codex",
        crate::admission::HostAdmissionOutcome {
            status: crate::admission::HostAdmissionStatus::Degraded,
            retryable: false,
            reason_code: Some("invalid_observation_contract"),
            recovery: None,
            cause: None,
        },
    );

    let failure = classify_transcript_ingest_failure("codex", "observation", &error);

    assert_eq!(failure.reason_code, "invalid_observation_contract");
    assert!(!failure.retryable);
}

#[test]
fn transcript_source_contract_failures_are_bounded_and_permanent() {
    let errors = [
        source::TranscriptIngestError::Domain(tracedecay_domain::SessionId::new("").unwrap_err()),
        source::TranscriptIngestError::ObservationContract(
            tracedecay_domain::ObservationSourceGenerationV1::new(0).unwrap_err(),
        ),
        source::TranscriptIngestError::InvalidFrameState {
            provider: "private provider detail",
        },
        source::TranscriptIngestError::InvalidSourceIdentity {
            provider: "private provider detail",
            path: PathBuf::from("/private/source/path"),
        },
    ];

    for error in errors {
        let failure = classify_transcript_ingest_failure("claude", "hook", &error);
        assert_eq!(failure.reason_code, "transcript_source_contract_invalid");
        assert!(!failure.retryable);
        assert!(
            !serde_json::to_string(&failure)
                .unwrap()
                .contains("private provider detail")
        );
    }
}

#[test]
fn startup_user_ingest_claims_are_single_flight_and_cancellation_safe() {
    let profile = tempfile::tempdir().unwrap().path().to_path_buf();
    let StartupUserIngestClaim::Acquired(first) = StartupUserIngestGuard::claim(profile.clone())
    else {
        panic!("first claim must acquire");
    };
    assert!(matches!(
        StartupUserIngestGuard::claim(profile.clone()),
        StartupUserIngestClaim::Running
    ));

    drop(first);
    let StartupUserIngestClaim::Acquired(mut retry) =
        StartupUserIngestGuard::claim(profile.clone())
    else {
        panic!("an incomplete claim must release immediately");
    };
    retry.completed = true;
    drop(retry);

    assert!(
        matches!(
            StartupUserIngestGuard::claim(profile),
            StartupUserIngestClaim::RecentlyCompleted
        ),
        "a completed sweep should suppress the startup herd during cooldown"
    );
}

#[test]
fn fair_round_robin_does_not_starve_later_sources() {
    let first = plan_round_robin_admission(3, 0, 1);
    assert_eq!(first.admitted_indices, vec![0]);
    assert_eq!(
        first.coverage,
        IngestPassCoverage::Partial { deferred_units: 2 }
    );

    let second = plan_round_robin_admission(3, 1, 1);
    assert_eq!(second.admitted_indices, vec![1]);

    let third = plan_round_robin_admission(3, 2, 1);
    assert_eq!(third.admitted_indices, vec![2]);

    let wrap = plan_round_robin_admission(3, 3, 1);
    assert_eq!(wrap.admitted_indices, vec![0]);
}

#[test]
fn zero_unit_budget_is_typed_backpressure() {
    let plan = plan_round_robin_admission(3, 0, 0);
    assert!(plan.admitted_indices.is_empty());
    assert_eq!(
        plan.coverage,
        IngestPassCoverage::Backpressured {
            admitted_units: 0,
            rejected_units: 3,
        }
    );
}

#[test]
fn cancellation_and_empty_attempt_suppress_scheduling_writes() {
    assert!(!scheduling_write_required(
        IngestPassCoverage::Partial { deferred_units: 3 },
        2,
        true
    ));
    assert!(!scheduling_write_required(
        IngestPassCoverage::Partial { deferred_units: 3 },
        0,
        false
    ));
    assert!(scheduling_write_required(
        IngestPassCoverage::Partial { deferred_units: 3 },
        2,
        false
    ));
    assert!(scheduling_write_required(
        IngestPassCoverage::Backpressured {
            admitted_units: 2,
            rejected_units: 1
        },
        2,
        false
    ));
}

#[test]
fn pass_byte_allocations_never_exceed_aggregate_cap() {
    let bounds = IngestPassBounds {
        bytes_per_unit: 4,
        bytes_per_pass: 10,
        ..TEST_INGEST_BOUNDS
    };
    let budgets = allocate_pass_byte_budgets(4, bounds);
    assert_eq!(budgets, vec![4, 4, 2]);
    assert_eq!(budgets.iter().copied().sum::<u64>(), 10);

    let zero = IngestPassBounds {
        bytes_per_unit: 4,
        bytes_per_pass: 0,
        ..TEST_INGEST_BOUNDS
    };
    assert!(allocate_pass_byte_budgets(4, zero).is_empty());
}

#[test]
fn user_provider_admission_applies_discovery_bound_to_actual_work() {
    let bounds = IngestPassBounds {
        discovered_units: 2,
        bytes_per_unit: 4,
        bytes_per_pass: 8,
        ..TEST_INGEST_BOUNDS
    };
    let first = plan_provider_rotation_admission(5, 0, bounds);
    assert_eq!(first.admitted_indices, vec![0, 1]);
    assert_eq!(
        first.coverage,
        IngestPassCoverage::Partial { deferred_units: 3 }
    );
    let second = plan_provider_rotation_admission(5, 2, bounds);
    assert_eq!(second.admitted_indices, vec![2, 3]);
}

#[test]
fn provider_internal_deferral_is_typed_backpressure() {
    let coverage = finish_user_provider_coverage(IngestPassCoverage::Complete, 1, 1, 1);
    assert_eq!(
        coverage,
        IngestPassCoverage::Backpressured {
            admitted_units: 1,
            rejected_units: 1,
        }
    );
}

#[test]
fn project_provider_deferral_preserves_existing_deferred_work() {
    let coverage = merge_project_provider_backpressure(
        IngestPassCoverage::Partial { deferred_units: 2 },
        2,
        3,
        1,
    );
    assert_eq!(
        coverage,
        IngestPassCoverage::Backpressured {
            admitted_units: 5,
            rejected_units: 3,
        }
    );
}
