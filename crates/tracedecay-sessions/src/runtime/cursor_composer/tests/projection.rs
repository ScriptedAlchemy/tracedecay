use super::super::*;

use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use tracedecay_domain::{
    ObservationScopeV1, ObservationSourceCursorV1, ObservationSourceGenerationV1,
    ObservationSourceIdentityV1, ProviderId, SessionId,
};
use tracedecay_store::ParseOffset;
use tracedecay_store::observation::{CursorAdvanceOutcome, ObservationCursorAdvance};

use crate::admission::test_support::{MemoryHostAdmission, PanicHostAdmission};
use crate::admission::{
    AdmissionFuture, HostAdmission, HostAdmissionOutcome, HostProjectionDrainOutcome,
};
use crate::observation::{
    CaptureObservationOutcome, CaptureObservationRequest, ObservationCancellation,
};
use crate::runtime::source::TranscriptIngestError;

async fn queue_cursor_projection(
    admission: &MemoryHostAdmission,
    project_id: &tracedecay_domain::ProjectId,
    composer_id: &str,
) {
    queue_cursor_projection_at(admission, project_id, composer_id, "bubble-0", 0).await;
}

async fn queue_cursor_projection_at(
    admission: &MemoryHostAdmission,
    project_id: &tracedecay_domain::ProjectId,
    composer_id: &str,
    bubble_id: &str,
    position: u64,
) {
    let scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };
    let source = ObservationSourceIdentityV1::for_provider(
        ProviderId::new(PROVIDER).expect("Cursor provider id"),
        SessionId::new(composer_id).expect("Cursor composer id"),
    )
    .expect("Cursor composer source");
    let expected_cursor = admission
        .get_source_cursor(&source, &scope)
        .await
        .expect("Cursor composer cursor");
    let request = build_cursor_composer_capture_request(
        composer_id,
        bubble_id,
        &json!({ "type": 1, "text": "queued Cursor projection" }),
        scope,
        ObservationSourceGenerationV1::new(1).expect("valid snapshot generation"),
        position,
        expected_cursor,
    )
    .expect("queued Cursor projection request");
    assert!(matches!(
        admission.capture_observation(request).await,
        Ok(CaptureObservationOutcome::Persisted { .. })
    ));
}

fn write_composer_state(
    home: &std::path::Path,
    project: &std::path::Path,
    composer_id: &str,
    bubbles: &[(&str, &str)],
) {
    let state_dir = home
        .join(".config")
        .join("Cursor")
        .join("User")
        .join("globalStorage");
    std::fs::create_dir_all(&state_dir).expect("Cursor state directory");
    let connection =
        rusqlite::Connection::open(state_dir.join("state.vscdb")).expect("Cursor state db");
    connection
        .execute_batch(
            "PRAGMA journal_mode=DELETE;
             CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);",
        )
        .expect("Cursor state schema");
    let headers = bubbles
        .iter()
        .map(|(bubble_id, _)| json!({ "bubbleId": bubble_id }))
        .collect::<Vec<_>>();
    connection
        .execute(
            "INSERT INTO cursorDiskKV(key, value) VALUES (?1, ?2)",
            rusqlite::params![
                format!("composerData:{composer_id}"),
                json!({
                    "composerId": composer_id,
                    "workspaceIdentifier": {
                        "uri": { "fsPath": project.to_string_lossy() }
                    },
                    "fullConversationHeadersOnly": headers
                })
                .to_string()
            ],
        )
        .expect("composer envelope");
    for (bubble_id, text) in bubbles {
        connection
            .execute(
                "INSERT INTO cursorDiskKV(key, value) VALUES (?1, ?2)",
                rusqlite::params![
                    format!("bubbleId:{composer_id}:{bubble_id}"),
                    json!({ "type": 1, "text": text }).to_string()
                ],
            )
            .expect("composer bubble");
    }
}

fn write_cursor_jsonl(home: &std::path::Path, project: &std::path::Path, session_id: &str) {
    let slug = crate::runtime::cursor::cursor_project_slug(project).expect("Cursor project slug");
    let transcript_dir = home
        .join(".cursor")
        .join("projects")
        .join(slug)
        .join("agent-transcripts")
        .join(session_id);
    std::fs::create_dir_all(&transcript_dir).expect("Cursor transcript directory");
    std::fs::write(
        transcript_dir.join(format!("{session_id}.jsonl")),
        "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"JSONL fallback\"}]}}\n",
    )
    .expect("Cursor JSONL transcript");
}

enum PostFirstDrain {
    Cancel(ObservationCancellation),
    Fail,
}

struct PostFirstDrainAdmission {
    inner: MemoryHostAdmission,
    drains: AtomicUsize,
    action: PostFirstDrain,
}

impl PostFirstDrainAdmission {
    fn cancelling(inner: MemoryHostAdmission, cancellation: ObservationCancellation) -> Self {
        Self {
            inner,
            drains: AtomicUsize::new(0),
            action: PostFirstDrain::Cancel(cancellation),
        }
    }

    fn failing(inner: MemoryHostAdmission) -> Self {
        Self {
            inner,
            drains: AtomicUsize::new(0),
            action: PostFirstDrain::Fail,
        }
    }
}

impl HostAdmission for PostFirstDrainAdmission {
    fn capture_observation<'a>(
        &'a self,
        _request: CaptureObservationRequest,
    ) -> AdmissionFuture<'a, CaptureObservationOutcome> {
        panic!("projection-only sweep attempted observation admission")
    }

    fn advance_non_durable_source_cursor<'a>(
        &'a self,
        _advance: ObservationCursorAdvance,
        _cancellation: ObservationCancellation,
    ) -> AdmissionFuture<'a, CursorAdvanceOutcome> {
        panic!("projection-only sweep attempted cursor admission")
    }

    fn get_source_cursor<'a>(
        &'a self,
        _source: &'a ObservationSourceIdentityV1,
        _scope: &'a ObservationScopeV1,
    ) -> AdmissionFuture<'a, Option<ObservationSourceCursorV1>> {
        panic!("projection-only sweep attempted cursor read")
    }

    fn drain_projection_queue<'a>(
        &'a self,
        provider: &'a str,
        scope: &'a ObservationScopeV1,
        cancellation: &'a ObservationCancellation,
        max: usize,
    ) -> AdmissionFuture<'a, HostProjectionDrainOutcome> {
        Box::pin(async move {
            let drain = self.drains.fetch_add(1, Ordering::SeqCst);
            if drain == 1 && matches!(&self.action, PostFirstDrain::Fail) {
                return Err(HostAdmissionOutcome::retained_unavailable(
                    "projection_authority_unavailable",
                ));
            }
            let outcome = self
                .inner
                .drain_projection_queue(provider, scope, cancellation, max)
                .await;
            if drain == 0
                && let PostFirstDrain::Cancel(cancellation) = &self.action
            {
                cancellation.cancel();
            }
            outcome
        })
    }

    fn has_session_message<'a>(
        &'a self,
        _scope: &'a ObservationScopeV1,
        _provider: &'a str,
        _message_id: &'a str,
    ) -> AdmissionFuture<'a, bool> {
        panic!("projection-only sweep attempted session-message read")
    }

    fn get_parse_offset<'a>(
        &'a self,
        _scope: &'a ObservationScopeV1,
        _path: &'a str,
    ) -> AdmissionFuture<'a, Option<ParseOffset>> {
        panic!("projection-only sweep attempted parse-offset read")
    }

    fn advance_parse_offset<'a>(
        &'a self,
        _scope: &'a ObservationScopeV1,
        _path: &'a str,
        _offset: ParseOffset,
    ) -> AdmissionFuture<'a, ()> {
        panic!("projection-only sweep attempted parse-offset write")
    }
}

#[tokio::test]
async fn cancelled_composer_sweep_stops_before_scanning_state_database() {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let state_dir = home
        .path()
        .join(".config")
        .join("Cursor")
        .join("User")
        .join("globalStorage");
    std::fs::create_dir_all(&state_dir).unwrap();
    let connection = rusqlite::Connection::open(state_dir.join("state.vscdb")).unwrap();
    connection
        .execute_batch(
            "PRAGMA journal_mode=DELETE;
             CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO cursorDiskKV(key, value) VALUES (?1, ?2)",
            rusqlite::params![
                "composerData:cancelled",
                json!({
                    "composerId": "cancelled",
                    "workspaceIdentifier": {
                        "uri": { "fsPath": project.path().to_string_lossy() }
                    },
                    "fullConversationHeadersOnly": []
                })
                .to_string()
            ],
        )
        .unwrap();
    drop(connection);
    let project_id =
        tracedecay_domain::ProjectId::new("project.cursor-composer-cancelled").unwrap();
    let cancellation = ObservationCancellation::default();
    cancellation.cancel();

    let result = CursorComposerSource::with_home(home.path())
        .ingest_capped_with_cancellation(
            &PanicHostAdmission,
            project.path(),
            project_id.clone(),
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
            &cancellation,
        )
        .await;

    assert!(matches!(
        result,
        Err(CursorComposerSweepFailure {
            error: TranscriptIngestError::Cancelled { provider: "cursor" },
            ..
        })
    ));
}

/// Projection work can predate the SQLite sweep. Both drain passes contribute
/// canonical output stats, and the first pass's pending work keeps this sweep
/// incomplete even when the final pass catches up the remainder.
#[tokio::test]
async fn queued_cursor_projections_are_reported_and_keep_the_pass_deferred() {
    const QUEUED_PROJECTIONS: usize = 257;

    let project = tempfile::tempdir().expect("project tempdir");
    let home = tempfile::tempdir().expect("Cursor home tempdir");
    let project_id =
        tracedecay_domain::ProjectId::new("project.cursor-composer-projections").expect("id");
    let admission = MemoryHostAdmission::default();
    for ordinal in 0..QUEUED_PROJECTIONS {
        queue_cursor_projection(
            &admission,
            &project_id,
            &format!("queued-composer-{ordinal}"),
        )
        .await;
    }

    let outcome = CursorComposerSource::with_home(home.path())
        .ingest_capped(
            &admission,
            project.path(),
            project_id.clone(),
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
        )
        .await
        .expect("projection-only composer sweep");

    assert_eq!(outcome.sessions_upserted, QUEUED_PROJECTIONS as u64);
    assert_eq!(outcome.messages_upserted, QUEUED_PROJECTIONS as u64);
    assert!(outcome.deferred_by_byte_cap);
    assert_eq!(admission.pending_projection_count(), 0);
}

#[tokio::test]
async fn queued_jsonl_projection_does_not_hide_new_message_from_the_same_session() {
    let project = tempfile::tempdir().expect("project tempdir");
    let home = tempfile::tempdir().expect("Cursor home tempdir");
    let project_id = tracedecay_domain::ProjectId::new("project.cursor-jsonl-append").expect("id");
    let admission = MemoryHostAdmission::default();
    queue_cursor_projection(&admission, &project_id, "queued-jsonl-session").await;
    write_cursor_jsonl(home.path(), project.path(), "queued-jsonl-session");

    let composer = CursorComposerSource::with_home(home.path())
        .ingest_capped(
            &admission,
            project.path(),
            project_id.clone(),
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
        )
        .await
        .expect("projection-only Composer sweep");
    assert_eq!(composer.sessions_upserted, 1);
    assert!(composer.owned_session_ids.is_empty());

    let jsonl = crate::runtime::with_transcript_source_home(
        home.path().to_path_buf(),
        crate::runtime::cursor::try_ingest_cursor_project_sweep_capped(
            project.path(),
            &admission,
            project_id,
            None,
            composer.jsonl_skip_session_ids(),
        ),
    )
    .await
    .expect("new JSONL content remains eligible");

    assert_eq!(jsonl.sessions_upserted, 1);
    assert_eq!(jsonl.messages_upserted, 1);
}

#[tokio::test]
async fn projection_session_spanning_both_drains_is_counted_once() {
    const QUEUED_PROJECTIONS: usize = 257;

    let project = tempfile::tempdir().expect("project tempdir");
    let home = tempfile::tempdir().expect("Cursor home tempdir");
    let project_id =
        tracedecay_domain::ProjectId::new("project.cursor-composer-one-session").expect("id");
    let admission = MemoryHostAdmission::default();
    for ordinal in 0..QUEUED_PROJECTIONS {
        queue_cursor_projection_at(
            &admission,
            &project_id,
            "one-composer",
            &format!("bubble-{ordinal}"),
            ordinal as u64,
        )
        .await;
    }

    let outcome = CursorComposerSource::with_home(home.path())
        .ingest_capped(
            &admission,
            project.path(),
            project_id,
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
        )
        .await
        .expect("projection-only composer sweep");

    assert_eq!(outcome.sessions_upserted, 1);
    assert_eq!(outcome.messages_upserted, QUEUED_PROJECTIONS as u64);
    assert!(outcome.deferred_by_byte_cap);
    assert_eq!(admission.pending_projection_count(), 0);
}

#[tokio::test]
async fn cancellation_after_first_drain_preserves_committed_projection_stats() {
    let project = tempfile::tempdir().expect("project tempdir");
    let home = tempfile::tempdir().expect("Cursor home tempdir");
    let project_id =
        tracedecay_domain::ProjectId::new("project.cursor-composer-post-drain-cancel").expect("id");
    let admission = MemoryHostAdmission::default();
    queue_cursor_projection(&admission, &project_id, "committed-before-cancel").await;
    let cancellation = ObservationCancellation::default();
    let admission = PostFirstDrainAdmission::cancelling(admission, cancellation.clone());

    let failure = CursorComposerSource::with_home(home.path())
        .ingest_capped_with_cancellation(
            &admission,
            project.path(),
            project_id,
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
            &cancellation,
        )
        .await
        .expect_err("post-drain cancellation must terminate the sweep");

    assert!(matches!(
        failure.error,
        TranscriptIngestError::Cancelled { provider: "cursor" }
    ));
    assert_eq!(failure.outcome.sessions_upserted, 1);
    assert_eq!(failure.outcome.messages_upserted, 1);
}

#[tokio::test]
async fn second_drain_failure_preserves_committed_projection_stats() {
    let project = tempfile::tempdir().expect("project tempdir");
    let home = tempfile::tempdir().expect("Cursor home tempdir");
    let project_id =
        tracedecay_domain::ProjectId::new("project.cursor-composer-second-drain-fail").expect("id");
    let admission = MemoryHostAdmission::default();
    queue_cursor_projection(&admission, &project_id, "committed-before-failure").await;
    let admission = PostFirstDrainAdmission::failing(admission);

    let failure = CursorComposerSource::with_home(home.path())
        .ingest_capped(
            &admission,
            project.path(),
            project_id,
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
        )
        .await
        .expect_err("second projection drain must remain a typed failure");

    assert!(matches!(
        failure.error,
        TranscriptIngestError::HostAdmission {
            provider: "cursor",
            reason: "projection_authority_unavailable",
            ..
        }
    ));
    assert_eq!(failure.outcome.sessions_upserted, 1);
    assert_eq!(failure.outcome.messages_upserted, 1);
}

#[tokio::test]
async fn zero_composer_cap_defers_owned_session_before_jsonl_handoff() {
    let project = tempfile::tempdir().expect("project tempdir");
    let home = tempfile::tempdir().expect("Cursor home tempdir");
    write_composer_state(
        home.path(),
        project.path(),
        "cap-deferred-composer",
        &[("bubble-0", "must wait for the next composer pass")],
    );
    write_cursor_jsonl(home.path(), project.path(), "cap-deferred-composer");
    let project_id =
        tracedecay_domain::ProjectId::new("project.cursor-composer-cap-deferred").expect("id");
    let admission = MemoryHostAdmission::default();

    let outcome = CursorComposerSource::with_home(home.path())
        .ingest_capped(&admission, project.path(), project_id.clone(), 0, None)
        .await
        .expect("cap-zero composer sweep");

    assert!(outcome.deferred_by_byte_cap);
    assert!(outcome.owned_session_ids.contains("cap-deferred-composer"));
    assert_eq!(outcome.messages_upserted, 0);
    let jsonl = crate::runtime::with_transcript_source_home(
        home.path().to_path_buf(),
        crate::runtime::cursor::try_ingest_cursor_project_sweep_capped(
            project.path(),
            &admission,
            project_id,
            None,
            outcome.owned_session_ids,
        ),
    )
    .await
    .expect("Cursor JSONL handoff");
    assert_eq!(jsonl.messages_upserted, 0);
}

#[tokio::test]
async fn capture_admission_failure_defers_owned_session_before_jsonl_handoff() {
    let project = tempfile::tempdir().expect("project tempdir");
    let home = tempfile::tempdir().expect("Cursor home tempdir");
    write_composer_state(
        home.path(),
        project.path(),
        "capture-deferred-composer",
        &[("bubble-0", "retry after admission recovers")],
    );
    write_cursor_jsonl(home.path(), project.path(), "capture-deferred-composer");
    let project_id =
        tracedecay_domain::ProjectId::new("project.cursor-composer-capture-deferred").expect("id");
    let admission = MemoryHostAdmission::default();
    admission.fail_next_capture();

    let outcome = CursorComposerSource::with_home(home.path())
        .ingest_capped(
            &admission,
            project.path(),
            project_id.clone(),
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
        )
        .await
        .expect("retryable capture failure remains a deferred sweep");

    assert!(outcome.deferred_by_byte_cap);
    assert!(
        outcome
            .owned_session_ids
            .contains("capture-deferred-composer")
    );
    assert_eq!(outcome.messages_upserted, 0);
    let jsonl = crate::runtime::with_transcript_source_home(
        home.path().to_path_buf(),
        crate::runtime::cursor::try_ingest_cursor_project_sweep_capped(
            project.path(),
            &admission,
            project_id,
            None,
            outcome.owned_session_ids,
        ),
    )
    .await
    .expect("Cursor JSONL handoff");
    assert_eq!(jsonl.messages_upserted, 0);
}

#[tokio::test]
async fn composer_projection_cancellation_is_returned_as_a_typed_cursor_error() {
    let project = tempfile::tempdir().expect("project tempdir");
    let home = tempfile::tempdir().expect("Cursor home tempdir");
    let project_id =
        tracedecay_domain::ProjectId::new("project.cursor-composer-cancelled-drain").expect("id");
    let admission = MemoryHostAdmission::default();
    queue_cursor_projection(&admission, &project_id, "cancelled-drain").await;
    let cancellation = ObservationCancellation::default();
    admission.fail_next_projection_drain_after_cancelling(
        HostAdmissionOutcome::retained_backpressured("admission_cancelled"),
        cancellation.clone(),
    );

    let result = CursorComposerSource::with_home(home.path())
        .ingest_capped_with_cancellation(
            &admission,
            project.path(),
            project_id,
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
            &cancellation,
        )
        .await;

    assert!(matches!(
        result,
        Err(CursorComposerSweepFailure {
            error: TranscriptIngestError::Cancelled { provider: "cursor" },
            ..
        })
    ));
    assert_eq!(admission.pending_projection_count(), 1);
}

#[tokio::test]
async fn fresh_composer_observation_is_counted_once_after_its_projection() {
    let project = tempfile::tempdir().expect("project tempdir");
    let home = tempfile::tempdir().expect("Cursor home tempdir");
    let state_dir = home
        .path()
        .join(".config")
        .join("Cursor")
        .join("User")
        .join("globalStorage");
    std::fs::create_dir_all(&state_dir).expect("Cursor state directory");
    let connection =
        rusqlite::Connection::open(state_dir.join("state.vscdb")).expect("Cursor state db");
    connection
        .execute_batch(
            "PRAGMA journal_mode=DELETE;
             CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);",
        )
        .expect("Cursor state schema");
    connection
        .execute(
            "INSERT INTO cursorDiskKV(key, value) VALUES (?1, ?2)",
            rusqlite::params![
                "composerData:fresh-composer",
                json!({
                    "composerId": "fresh-composer",
                    "workspaceIdentifier": {
                        "uri": { "fsPath": project.path().to_string_lossy() }
                    },
                    "fullConversationHeadersOnly": [{ "bubbleId": "bubble-0" }]
                })
                .to_string()
            ],
        )
        .expect("composer envelope");
    connection
        .execute(
            "INSERT INTO cursorDiskKV(key, value) VALUES (?1, ?2)",
            rusqlite::params![
                "bubbleId:fresh-composer:bubble-0",
                json!({ "type": 1, "text": "fresh composer observation" }).to_string()
            ],
        )
        .expect("composer bubble");
    drop(connection);

    let project_id =
        tracedecay_domain::ProjectId::new("project.cursor-composer-fresh").expect("id");
    let admission = MemoryHostAdmission::default();
    let outcome = CursorComposerSource::with_home(home.path())
        .ingest_capped(
            &admission,
            project.path(),
            project_id,
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
        )
        .await
        .expect("fresh composer sweep");

    assert_eq!(outcome.sessions_upserted, 1);
    assert_eq!(outcome.messages_upserted, 1);
}
