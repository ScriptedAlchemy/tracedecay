use super::*;
use crate::admission::test_support::MemoryHostAdmission;

fn cursor_sweep_test_fixture() -> (
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
    std::fs::write(
        transcript_dir.join("session-cancelled.jsonl"),
        concat!(
            r#"{"role":"user","message":{"content":[{"type":"text","text":"must not ingest"}]}}"#,
            "\n"
        ),
    )
    .unwrap();
    let source = CursorSweepSource::with_home(home.path());
    (project, home, source, project_id)
}

#[tokio::test]
async fn cancelled_startup_sweep_stops_before_admitting_cursor_jsonl() {
    let (project, _home, source, project_id) = cursor_sweep_test_fixture();
    assert_eq!(source.transcript_paths(project.path()).len(), 1);
    let admission = MemoryHostAdmission::default();
    let cancellation = ObservationCancellation::default();
    cancellation.cancel();

    let error = admit_cursor_sweep_observations_with_admission(
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

    let replay = admit_cursor_sweep_observations_with_admission(
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
    assert_eq!(replay.messages_upserted, 1);

    let deduplicated = admit_cursor_sweep_observations_with_admission(
        &source,
        project.path(),
        &admission,
        None,
        ObservationScopeV1::Project { project_id },
        &ObservationCancellation::default(),
    )
    .await
    .expect("completed Cursor retry must be deduplicated");
    assert_eq!(deduplicated.messages_upserted, 0);
}

#[tokio::test]
async fn mid_admission_cancellation_stops_cursor_before_projection() {
    let (project, _home, source, project_id) = cursor_sweep_test_fixture();
    let admission = MemoryHostAdmission::default();
    let cancellation = ObservationCancellation::default();
    admission.cancel_on_next_cursor_read(cancellation.clone());

    let error = admit_cursor_sweep_observations_with_admission(
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

    let replay = admit_cursor_sweep_observations_with_admission(
        &source,
        project.path(),
        &admission,
        None,
        ObservationScopeV1::Project { project_id },
        &ObservationCancellation::default(),
    )
    .await
    .expect("uncancelled Cursor retry must admit the untouched source");
    assert_eq!(replay.messages_upserted, 1);
}
