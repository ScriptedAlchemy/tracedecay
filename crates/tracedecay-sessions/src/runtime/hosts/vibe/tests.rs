use super::*;
use std::time::{Duration, SystemTime};

use serde_json::Value;
use tempfile::TempDir;

use crate::admission::test_support::MemoryHostAdmission;

fn write_session_messages(root: &Path, name: &str, mtime_secs: u64) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("messages.jsonl");
    std::fs::write(&path, "").unwrap();
    let mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(mtime_secs);
    filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(mtime)).unwrap();
    path
}

fn write_ineligible_jsonl(root: &Path, name: &str, file_name: &str) {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(file_name), "").unwrap();
}

#[test]
fn ineligible_jsonl_does_not_crowd_out_eligible_newest() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join(".vibe/logs/session");
    for index in 0..20 {
        write_ineligible_jsonl(&root, &format!("noise-{index:02}"), "other.jsonl");
    }
    let older = write_session_messages(&root, "session-older", 100);
    let newer = write_session_messages(&root, "session-newer", 200);
    let source = VibeSource::with_home(tmp.path());
    let bounds = TranscriptDiscoveryBounds {
        max_files: 2,
        ..TranscriptDiscoveryBounds::from_discovered_units(2)
    };
    let report = source.discover_transcript_paths(tmp.path(), bounds);
    assert_eq!(report.paths, vec![newer, older]);
    assert!(!report.is_truncated());
}

#[test]
fn newest_selection_uses_stable_path_tie_break() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join(".vibe/logs/session");
    let shared_mtime = 1_700_000_000;
    let alpha = write_session_messages(&root, "session-a", shared_mtime);
    let bravo = write_session_messages(&root, "session-b", shared_mtime);
    let charlie = write_session_messages(&root, "session-c", shared_mtime);
    let source = VibeSource::with_home(tmp.path());
    let bounds = TranscriptDiscoveryBounds {
        max_files: 3,
        ..TranscriptDiscoveryBounds::from_discovered_units(3)
    };
    let report = source.discover_transcript_paths(tmp.path(), bounds);
    assert_eq!(report.paths, vec![alpha, bravo, charlie]);
}

#[test]
fn pagination_completes_older_work_then_surfaces_finite_new_arrivals() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join(".vibe/logs/session");
    let oldest = write_session_messages(&root, "session-old", 10);
    let mid = write_session_messages(&root, "session-mid", 20);
    let newest = write_session_messages(&root, "session-new", 30);
    let source = VibeSource::with_home(tmp.path());
    let bounds = TranscriptDiscoveryBounds {
        max_files: 2,
        ..TranscriptDiscoveryBounds::from_discovered_units(2)
    };

    let (page0, omitted0) = source.discover_transcript_paths_page(tmp.path(), bounds, 0);
    assert_eq!(page0.paths, vec![newest.clone(), mid.clone()]);
    assert_eq!(page0.truncated, Some(FileDiscoveryLimit::FileCount));
    assert_eq!(omitted0, 1);

    // A new session shifts the positional ranking, but the next page still
    // reaches the previously omitted oldest session. Once the finite
    // ranking is exhausted, the scheduler's existing reset-to-zero path
    // makes the arrival part of the next newest-first cycle.
    let arrived = write_session_messages(&root, "session-arrived", 40);
    let (page1, omitted1) = source.discover_transcript_paths_page(tmp.path(), bounds, 2);
    assert_eq!(page1.paths, vec![mid, oldest.clone()]);
    assert!(!page1.is_truncated());
    assert_eq!(omitted1, 2);

    let (exhausted, omitted_exhausted) =
        source.discover_transcript_paths_page(tmp.path(), bounds, 4);
    assert!(exhausted.paths.is_empty());
    assert_eq!(omitted_exhausted, 4);

    let (page0_after, _) = source.discover_transcript_paths_page(tmp.path(), bounds, 0);
    assert_eq!(page0_after.paths[0], arrived);
    assert!(page0_after.paths.contains(&newest));
    assert!(!page0_after.paths.contains(&oldest));
}

/// One unbounded profile-scope capture of the Vibe home under `home`.
async fn capture_profile(
    admission: &MemoryHostAdmission,
    home: &Path,
    project: &Path,
) -> VibeCaptureOutcome {
    capture_vibe_observations(
        admission,
        &VibeSource::with_home(home),
        project,
        ObservationScopeV1::Profile,
        None,
        &ObservationCancellation::default(),
        None,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn accepted_prefixed_session_and_nonmessage_prefix_share_canonical_cursor_identity() {
    crate::runtime::observation::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let session_id = "session_280d6113-53d5-460d-b519-9cf819759a28";
    let session = tmp.path().join(".vibe/logs/session").join(session_id);
    std::fs::create_dir_all(&session).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        session.join("meta.json"),
        serde_json::json!({
            "session_id": session_id,
            "environment": {"working_directory": project}
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        session.join("messages.jsonl"),
        format!(
            "{}\n{}\n",
            serde_json::json!({"type": "metadata"}),
            serde_json::json!({"role": "assistant", "content": "visible after metadata"})
        ),
    )
    .unwrap();
    let admission = MemoryHostAdmission::default();

    let outcome = capture_profile(&admission, tmp.path(), &project).await;

    assert!(!outcome.deferred);
    let observations = admission.observations();
    assert_eq!(observations.len(), 1);
    assert_eq!(
        observations[0].observation().source().session_id().as_str(),
        tracedecay_privacy::protect_sensitive_structural_id(session_id).unwrap()
    );
    assert!(
        observations[0]
            .observation()
            .payload()
            .to_string()
            .contains("visible after metadata")
    );
}

#[tokio::test]
async fn vibe_workflow_lookalike_admits_as_one_ordinary_message() {
    // Vibe has no workflow-lifecycle normalizer: a record carrying lookalike
    // goal/todo/workflow bags admits as its message content alone.
    crate::runtime::observation::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let session = tmp.path().join(".vibe/logs/session/vibe-lookalike");
    std::fs::create_dir_all(&session).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        session.join("meta.json"),
        serde_json::json!({
            "session_id": "vibe-lookalike",
            "environment": {"working_directory": project}
        })
        .to_string(),
    )
    .unwrap();
    let input: Value = serde_json::from_str(include_str!(
        "../../../../../../tests/fixtures/provider_normalization/vibe/workflow_lookalike.input.json"
    ))
    .expect("Vibe workflow lookalike input");
    std::fs::write(session.join("messages.jsonl"), format!("{input}\n")).unwrap();
    let admission = MemoryHostAdmission::default();

    capture_profile(&admission, tmp.path(), &project).await;

    let observations = admission.observations();
    assert_eq!(observations.len(), 1);
    let envelope: tracedecay_domain::CanonicalObservationEnvelopeV1 =
        serde_json::from_value(observations[0].observation().payload().clone()).unwrap();
    let [
        tracedecay_domain::CanonicalObservationFactV1::Session {
            location_provenance,
            ..
        },
        tracedecay_domain::CanonicalObservationFactV1::Message { content, .. },
    ] = envelope.facts()
    else {
        panic!(
            "lookalike must admit as a location session plus one message fact: {:?}",
            envelope.facts()
        );
    };
    assert_eq!(
        location_provenance.as_deref(),
        Some("session_meta"),
        "the leading session fact must carry meta.json location provenance"
    );
    assert_eq!(
        content,
        &Value::String("Vibe workflow lookalike remains an ordinary message".to_owned())
    );
    let payload = observations[0].observation().payload().to_string();
    for rejected in [
        "vibe-hostile-task",
        "todo-hostile-1",
        "invented todo",
        "invented goal",
    ] {
        assert!(
            !payload.contains(rejected),
            "{rejected} must not survive Vibe observation normalization"
        );
    }
}
