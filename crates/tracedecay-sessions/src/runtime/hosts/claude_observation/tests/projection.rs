use std::fs;

use super::super::{
    ClaudeObservationIngestError, ClaudeSource, MAX_PROJECTIONS_PER_PASS, ObservationCancellation,
    ObservationScopeV1,
};
use super::Fixture;
use crate::admission::HostAdmissionOutcome;
use crate::runtime::source::TranscriptIngestError;
use serde_json::json;

#[tokio::test]
async fn persistence_failure_charges_the_source_scan_budget() {
    let fixture = Fixture::new("budget-failure-0");
    fixture.write_record(
        "first source is deliberately longer than the later source",
        "budget-failure-secret",
    );
    let later = fixture
        .transcript
        .parent()
        .expect("transcript parent")
        .join("budget-failure-1.jsonl");
    fs::write(
        &later,
        format!(
            "{}\n",
            json!({
                "type": "user",
                "sessionId": "budget-failure-1",
                "uuid": "budget-failure-message-1",
                "timestamp": "2026-07-15T00:00:00Z",
                "message": {"role": "user", "content": "short"}
            })
        ),
    )
    .expect("write later source");
    let budget = fs::metadata(&fixture.transcript)
        .expect("first source metadata")
        .len();
    assert!(fs::metadata(&later).expect("later source metadata").len() < budget);
    fixture.admission.fail_next_capture();
    let source = ClaudeSource::with_home(&fixture.home).for_user_scope(None, Vec::new());

    let error = fixture
        .ingest(&source, Some(budget), ObservationCancellation::default())
        .await
        .expect_err("capture failure remains visible");
    let cause = match error {
        ClaudeObservationIngestError::Terminated { error, .. } => *error,
        error => error,
    };
    assert!(matches!(
        cause,
        ClaudeObservationIngestError::SourceFailures {
            failed_sources: 1,
            ..
        }
    ));
}

#[tokio::test]
async fn capped_projection_backlog_marks_claude_source_deferred_until_the_next_pass() {
    let fixture = Fixture::new("projection-backlog");
    let source = fixture.source("projection-backlog");
    let record_count = MAX_PROJECTIONS_PER_PASS.saturating_add(1);
    let mut transcript = String::new();
    for index in 0..record_count {
        let record = json!({
            "type": "user",
            "sessionId": "projection-backlog",
            "uuid": format!("projection-backlog-message-{index}"),
            "timestamp": "2026-07-15T00:00:00Z",
            "cwd": fixture.temp.path(),
            "message": {"role": "user", "content": format!("queued projection {index}")}
        });
        transcript.push_str(&record.to_string());
        transcript.push('\n');
    }
    fs::write(&fixture.transcript, transcript).expect("write capped Claude projection backlog");

    let first = fixture
        .ingest(&source, None, ObservationCancellation::default())
        .await
        .expect("first capped Claude production pass");
    assert_eq!(first.observations_committed, record_count as u64);
    assert_eq!(first.projections_completed, MAX_PROJECTIONS_PER_PASS as u64);
    assert_eq!(
        first.transcript.messages_upserted,
        MAX_PROJECTIONS_PER_PASS as u64
    );
    assert_eq!(first.projection_outputs, MAX_PROJECTIONS_PER_PASS as u64);
    assert_eq!(first.deferred_sources, 1);
    assert_eq!(fixture.admission.pending_projection_count(), 1);

    let second = fixture
        .ingest(&source, None, ObservationCancellation::default())
        .await
        .expect("second capped Claude production pass");
    assert_eq!(second.projections_completed, 1);
    assert_eq!(second.transcript.messages_upserted, 1);
    assert_eq!(second.deferred_sources, 0);
    assert_eq!(fixture.admission.pending_projection_count(), 0);
}

#[tokio::test]
async fn typed_projection_cancellation_stops_the_claude_production_path() {
    let fixture = Fixture::new("projection-cancelled");
    let source = fixture.source("projection-cancelled");
    let cancellation = ObservationCancellation::default();
    fixture
        .admission
        .fail_next_projection_drain_after_cancelling(
            HostAdmissionOutcome::retained_backpressured("admission_cancelled"),
            cancellation.clone(),
        );

    let error = fixture
        .ingest(&source, None, cancellation)
        .await
        .expect_err("typed projection cancellation must stop Claude control flow");
    assert!(matches!(
        error,
        ClaudeObservationIngestError::Transcript(TranscriptIngestError::Cancelled {
            provider: "claude"
        })
    ));
}

#[tokio::test]
async fn projection_authority_error_racing_cancellation_stays_visible_for_claude() {
    let fixture = Fixture::new("projection-error");
    let source = fixture.source("projection-error");
    let cancellation = ObservationCancellation::default();
    fixture
        .admission
        .fail_next_projection_drain_after_cancelling(
            HostAdmissionOutcome::registered_authority_unavailable(),
            cancellation.clone(),
        );

    let error = fixture
        .ingest(&source, None, cancellation)
        .await
        .expect_err("projection authority error must remain visible");
    assert!(matches!(
        error,
        ClaudeObservationIngestError::Transcript(TranscriptIngestError::HostAdmission {
            provider: "claude",
            reason: "registered_authority_unavailable",
            retryable: true,
        })
    ));
}

#[tokio::test]
async fn source_authority_error_precedes_racing_final_projection_cancellation() {
    let fixture = Fixture::new("source-error-before-projection-cancel");
    fixture.write_record("source authority failure", "source-failure-secret");
    let source = fixture.source("source-error-before-projection-cancel");
    let cancellation = ObservationCancellation::default();
    fixture.admission.fail_next_capture();
    fixture
        .admission
        .fail_next_projection_drain_after_cancelling(
            HostAdmissionOutcome::retained_backpressured("admission_cancelled"),
            cancellation.clone(),
        );

    let error = fixture
        .ingest(&source, None, cancellation)
        .await
        .expect_err("source authority failure must remain visible");
    let cause = match error {
        ClaudeObservationIngestError::Terminated { error, .. } => *error,
        error => error,
    };
    assert!(matches!(
        cause,
        ClaudeObservationIngestError::SourceFailures {
            failed_sources: 1,
            ..
        }
    ));
}

#[tokio::test]
async fn final_projection_cancellation_carries_durable_claude_progress() {
    let fixture = Fixture::new("projection-cancelled-after-capture");
    fixture.write_record("persist before cancellation", "projection-cancelled-secret");
    let source = fixture.source("projection-cancelled-after-capture");
    let cancellation = ObservationCancellation::default();
    fixture
        .admission
        .fail_next_projection_drain_after_cancelling(
            HostAdmissionOutcome::retained_backpressured("admission_cancelled"),
            cancellation.clone(),
        );

    let error = fixture
        .ingest(&source, None, cancellation)
        .await
        .expect_err("final projection cancellation must retain durable source progress");
    match error {
        ClaudeObservationIngestError::Terminated { stats, error } => {
            assert_eq!(stats.observations_committed, 1);
            assert!(stats.source_bytes_scanned > 0);
            assert!(matches!(
                *error,
                ClaudeObservationIngestError::Transcript(TranscriptIngestError::Cancelled {
                    provider: "claude"
                })
            ));
        }
        other => panic!("final projection cancellation must carry Claude progress: {other}"),
    }
}

#[tokio::test]
async fn final_projection_authority_error_carries_durable_claude_progress() {
    let fixture = Fixture::new("projection-error-after-capture");
    fixture.write_record("persist before authority error", "projection-error-secret");
    let source = fixture.source("projection-error-after-capture");
    let cancellation = ObservationCancellation::default();
    fixture
        .admission
        .fail_next_projection_drain_after_cancelling(
            HostAdmissionOutcome::registered_authority_unavailable(),
            cancellation.clone(),
        );

    let error = fixture
        .ingest(&source, None, cancellation)
        .await
        .expect_err("final projection authority error must retain durable source progress");
    match error {
        ClaudeObservationIngestError::Terminated { stats, error } => {
            assert_eq!(stats.observations_committed, 1);
            assert!(stats.source_bytes_scanned > 0);
            assert!(matches!(
                *error,
                ClaudeObservationIngestError::Transcript(TranscriptIngestError::HostAdmission {
                    provider: "claude",
                    reason: "registered_authority_unavailable",
                    retryable: true,
                })
            ));
        }
        other => panic!("final projection error must carry Claude progress: {other}"),
    }
}

#[tokio::test]
async fn one_claude_session_spanning_internal_and_orchestration_drains_counts_once() {
    let fixture = Fixture::new("projection-one-session");
    let source = fixture.source("projection-one-session");
    let mut transcript = String::new();
    for index in 0..MAX_PROJECTIONS_PER_PASS.saturating_add(1) {
        let record = json!({
            "type": "user",
            "sessionId": "projection-one-session",
            "uuid": format!("projection-one-session-message-{index}"),
            "timestamp": "2026-07-15T00:00:00Z",
            "cwd": fixture.temp.path(),
            "message": {"role": "user", "content": format!("session projection {index}")}
        });
        transcript.push_str(&record.to_string());
        transcript.push('\n');
    }
    fs::write(&fixture.transcript, transcript).expect("write one-session Claude projection cap");

    let internal = fixture
        .ingest(&source, None, ObservationCancellation::default())
        .await
        .expect("internal Claude projection drain");
    let orchestration = super::super::drain_projection_queue(
        &fixture.admission,
        &ObservationScopeV1::Profile,
        &ObservationCancellation::default(),
    )
    .await
    .expect("orchestration Claude projection drain");
    let mut projected_session_ids = internal.projected_session_ids().clone();
    let merged = internal
        .transcript
        .merge(orchestration.deduplicated_transcript_stats(&mut projected_session_ids));

    assert_eq!(internal.transcript.sessions_upserted, 1);
    assert_eq!(orchestration.transcript.sessions_upserted, 1);
    assert_eq!(merged.sessions_upserted, 1);
    assert_eq!(
        merged.messages_upserted,
        MAX_PROJECTIONS_PER_PASS as u64 + 1
    );
    assert_eq!(fixture.admission.pending_projection_count(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn source_failure_after_durable_claude_progress_carries_stats_and_cause() {
    let fixture = Fixture::new("source-failure-progress");
    fixture.write_record("valid after source failure", "source-failure-secret");
    fs::write(
        fixture
            .transcript
            .parent()
            .expect("transcript parent")
            .join("!bad\nsource.jsonl"),
        b"{}\n",
    )
    .expect("write isolated invalid Claude source");
    let source = ClaudeSource::with_home(&fixture.home).for_user_scope(None, Vec::new());

    let error = fixture
        .ingest(&source, None, ObservationCancellation::default())
        .await
        .expect_err("source failure after durable progress must remain visible");
    match error {
        ClaudeObservationIngestError::Terminated { stats, error } => {
            assert_eq!(stats.observations_committed, 1);
            assert_eq!(stats.transcript.messages_upserted, 1);
            assert!(matches!(
                *error,
                ClaudeObservationIngestError::SourceFailures {
                    failed_sources: 1,
                    first_reason_code: "observation_domain_invalid",
                    first_retryable: false,
                }
            ));
        }
        other => panic!("source failure must retain durable Claude progress: {other}"),
    }
}
