use std::fs;

use serde_json::json;
use super::super::{
    ClaudeObservationIngestError, MAX_PROJECTIONS_PER_PASS, ObservationCancellation,
};
use super::Fixture;
use crate::admission::HostAdmissionOutcome;
use crate::runtime::source::TranscriptIngestError;

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
        ClaudeObservationIngestError::Transcript(TranscriptIngestError::NonDurableRecord {
            provider: "claude",
            reason: "registered_authority_unavailable",
            ..
        })
    ));
}
