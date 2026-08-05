use super::super::*;

use serde_json::json;
use tracedecay_domain::{ObservationScopeV1, ObservationSourceGenerationV1};

use crate::admission::test_support::{MemoryHostAdmission, PanicHostAdmission};
use crate::admission::{HostAdmission, HostAdmissionOutcome};
use crate::observation::{CaptureObservationOutcome, ObservationCancellation};
use crate::runtime::source::TranscriptIngestError;

async fn queue_cursor_projection(
    admission: &MemoryHostAdmission,
    project_id: &tracedecay_domain::ProjectId,
    composer_id: &str,
) {
    let request = build_cursor_composer_capture_request(
        composer_id,
        "bubble-0",
        &json!({ "type": 1, "text": "queued Cursor projection" }),
        ObservationScopeV1::Project {
            project_id: project_id.clone(),
        },
        ObservationSourceGenerationV1::new(1).expect("valid snapshot generation"),
        0,
        None,
    )
    .expect("queued Cursor projection request");
    assert!(matches!(
        admission.capture_observation(request).await,
        Ok(CaptureObservationOutcome::Persisted { .. })
    ));
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
            project_id,
            DEFAULT_COMPOSER_ENVELOPE_CAP,
            None,
            &cancellation,
        )
        .await;

    assert!(matches!(
        result,
        Err(TranscriptIngestError::Cancelled { provider: "cursor" })
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
            project_id,
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
        Err(TranscriptIngestError::Cancelled { provider: "cursor" })
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
