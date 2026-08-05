use super::*;
use crate::admission::HostAdmissionOutcome;
use crate::admission::test_support::MemoryHostAdmission;

fn cursor_sweep_test_fixture_with_messages(
    message_count: usize,
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    CursorSweepSource,
    ProjectId,
) {
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let project_id = ProjectId::new("project.cursor-cancelled-startup").unwrap();
    let slug = cursor_project_slug(project.path()).unwrap();
    let transcript_dir = home
        .path()
        .join(".cursor")
        .join("projects")
        .join(slug)
        .join("agent-transcripts")
        .join("session-cancelled");
    std::fs::create_dir_all(&transcript_dir).unwrap();
    let transcript = (0..message_count)
        .map(|index| {
            format!(
                r#"{{"role":"user","message":{{"content":[{{"type":"text","text":"message {index}"}}]}}}}"#
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(transcript_dir.join("session-cancelled.jsonl"), transcript).unwrap();
    let source = CursorSweepSource::with_home(home.path());
    (project, home, source, project_id)
}

fn cursor_sweep_test_fixture() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    CursorSweepSource,
    ProjectId,
) {
    cursor_sweep_test_fixture_with_messages(1)
}

#[tokio::test]
async fn cancelled_startup_sweep_stops_before_admitting_cursor_jsonl() {
    let (project, _home, source, project_id) = cursor_sweep_test_fixture();
    assert_eq!(source.transcript_paths(project.path()).len(), 1);
    let admission = MemoryHostAdmission::default();
    let cancellation = ObservationCancellation::default();
    cancellation.cancel();

    let error = admit_cursor_sweep_observations_with_session_ids(
        &source,
        project.path(),
        &admission,
        None,
        ObservationScopeV1::Project {
            project_id: project_id.clone(),
        },
        &cancellation,
    )
    .await
    .expect_err("pre-cancelled Cursor sweep must stop before persistence");

    assert!(matches!(
        error,
        TranscriptIngestError::Cancelled { provider: "cursor" }
    ));
    assert!(admission.observations().is_empty());

    let replay = admit_cursor_sweep_observations_with_session_ids(
        &source,
        project.path(),
        &admission,
        None,
        ObservationScopeV1::Project {
            project_id: project_id.clone(),
        },
        &ObservationCancellation::default(),
    )
    .await
    .expect("uncancelled Cursor retry must admit the untouched source");
    assert_eq!(admission.observations().len(), 1);
    assert_eq!(replay.stats.messages_upserted, 1);

    let deduplicated = admit_cursor_sweep_observations_with_session_ids(
        &source,
        project.path(),
        &admission,
        None,
        ObservationScopeV1::Project { project_id },
        &ObservationCancellation::default(),
    )
    .await
    .expect("completed Cursor retry must be deduplicated");
    assert_eq!(deduplicated.stats.messages_upserted, 0);
}

#[tokio::test]
async fn mid_admission_cancellation_stops_cursor_before_projection() {
    let (project, _home, source, project_id) = cursor_sweep_test_fixture();
    let admission = MemoryHostAdmission::default();
    let cancellation = ObservationCancellation::default();
    admission.cancel_on_next_cursor_read(cancellation.clone());

    let error = admit_cursor_sweep_observations_with_session_ids(
        &source,
        project.path(),
        &admission,
        None,
        ObservationScopeV1::Project {
            project_id: project_id.clone(),
        },
        &cancellation,
    )
    .await
    .expect_err("mid-admission cancellation must stop before projection");

    assert!(matches!(
        error,
        TranscriptIngestError::Cancelled { provider: "cursor" }
    ));
    assert!(admission.observations().is_empty());

    let replay = admit_cursor_sweep_observations_with_session_ids(
        &source,
        project.path(),
        &admission,
        None,
        ObservationScopeV1::Project { project_id },
        &ObservationCancellation::default(),
    )
    .await
    .expect("uncancelled Cursor retry must admit the untouched source");
    assert_eq!(replay.stats.messages_upserted, 1);
}

#[tokio::test]
async fn projection_backlog_over_pass_limit_converges_in_two_passes() {
    let (project, _home, source, project_id) =
        cursor_sweep_test_fixture_with_messages(MAX_CURSOR_PROJECTIONS_PER_PASS + 1);
    let admission = MemoryHostAdmission::default();
    let scope = ObservationScopeV1::Project {
        project_id: project_id.clone(),
    };

    let first = admit_cursor_sweep_observations_with_session_ids(
        &source,
        project.path(),
        &admission,
        None,
        scope.clone(),
        &ObservationCancellation::default(),
    )
    .await
    .expect("first bounded projection pass");

    assert_eq!(
        first.stats.messages_upserted,
        MAX_CURSOR_PROJECTIONS_PER_PASS as u64
    );
    assert!(first.stats.source_deferred);
    assert_eq!(admission.pending_projection_count(), 1);

    let second = admit_cursor_sweep_observations_with_session_ids(
        &source,
        project.path(),
        &admission,
        None,
        scope,
        &ObservationCancellation::default(),
    )
    .await
    .expect("second bounded projection pass");

    assert_eq!(second.stats.messages_upserted, 1);
    assert!(!second.stats.source_deferred);
    assert_eq!(admission.pending_projection_count(), 0);
}

#[tokio::test]
async fn typed_projection_cancellation_is_control_termination() {
    let admission = MemoryHostAdmission::default();
    let cancellation = ObservationCancellation::default();
    admission.fail_next_projection_drain_after_cancelling(
        HostAdmissionOutcome::retained_backpressured("admission_cancelled"),
        cancellation.clone(),
    );

    let error = drain_cursor_observation_projections(
        &admission,
        &ObservationScopeV1::Profile,
        &cancellation,
    )
    .await
    .expect_err("typed projection cancellation must stop control flow");

    assert!(matches!(
        error,
        TranscriptIngestError::Cancelled { provider: "cursor" }
    ));
}

#[tokio::test]
async fn projection_error_racing_cancellation_remains_visible() {
    let admission = MemoryHostAdmission::default();
    let cancellation = ObservationCancellation::default();
    admission.fail_next_projection_drain_after_cancelling(
        HostAdmissionOutcome::registered_authority_unavailable(),
        cancellation.clone(),
    );

    let error = drain_cursor_observation_projections(
        &admission,
        &ObservationScopeV1::Profile,
        &cancellation,
    )
    .await
    .expect_err("projection authority error must remain visible");

    assert!(matches!(
        error,
        TranscriptIngestError::NonDurableRecord {
            provider: "cursor",
            reason: "registered_authority_unavailable",
            ..
        }
    ));
}
