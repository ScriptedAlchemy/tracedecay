use super::*;
use crate::admission::test_support::MemoryHostAdmission;

/// One more session than a sweep page holds: empty filler sessions sort
/// first, then `z-tail` carries the only message, past the first page.
fn corpus_one_session_past_the_first_page() -> (tempfile::TempDir, tempfile::TempDir, usize) {
    crate::runtime::observation::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
    let project = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let transcripts_dir = home
        .path()
        .join(".cursor")
        .join("projects")
        .join(cursor_project_slug(project.path()).unwrap())
        .join("agent-transcripts");
    let page_files = TranscriptDiscoveryBounds::default_walk().max_files;
    for index in 0..page_files {
        let session = format!("filler-{index:05}");
        let dir = transcripts_dir.join(&session);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{session}.jsonl")), b"").unwrap();
    }
    let tail = transcripts_dir.join("z-tail");
    std::fs::create_dir_all(&tail).unwrap();
    std::fs::write(
        tail.join("z-tail.jsonl"),
        "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"tail session\"}]}}\n",
    )
    .unwrap();
    (project, home, page_files + 1)
}

async fn sweep_pass(
    home: &Path,
    project: &Path,
    admission: &MemoryHostAdmission,
) -> CursorSweepIngestOutcome {
    // A fresh source per pass, as after a daemon restart: only the admission
    // store carries state between passes.
    admit_cursor_sweep_observations_with_session_ids(
        &CursorSweepSource::with_home(home),
        project,
        admission,
        None,
        ObservationScopeV1::Project {
            project_id: ProjectId::new("project.cursor-sweep-page").unwrap(),
        },
        &ObservationCancellation::default(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn cursor_sweep_admits_sessions_past_the_first_page_on_the_next_pass() {
    let (project, home, _) = corpus_one_session_past_the_first_page();
    let admission = MemoryHostAdmission::default();

    let first = sweep_pass(home.path(), project.path(), &admission).await;
    assert!(
        first.stats.source_deferred,
        "a pass that stops before the last session must report deferred work"
    );
    assert!(admission.observations().is_empty());

    let second = sweep_pass(home.path(), project.path(), &admission).await;
    assert_eq!(admission.observations().len(), 1);
    assert_eq!(second.stats.messages_upserted, 1);
    assert!(!second.stats.source_deferred);
}

#[tokio::test]
async fn cursor_sweep_reports_lap_coverage_and_restarts_after_a_complete_lap() {
    let (project, home, total) = corpus_one_session_past_the_first_page();
    let admission = MemoryHostAdmission::default();
    let continuing = CursorSweepCoverage::Continuing {
        resume_at: u64::try_from(total - 1).unwrap(),
        total: u64::try_from(total).unwrap(),
    };

    let first = sweep_pass(home.path(), project.path(), &admission).await;
    assert_eq!(first.coverage, continuing);
    assert_eq!(first.coverage.deferred_sessions(), 1);

    let second = sweep_pass(home.path(), project.path(), &admission).await;
    assert_eq!(second.coverage, CursorSweepCoverage::Complete);

    let third = sweep_pass(home.path(), project.path(), &admission).await;
    assert_eq!(
        third.coverage, continuing,
        "a completed lap starts the next one from the first session"
    );
    assert_eq!(admission.observations().len(), 1);
}
