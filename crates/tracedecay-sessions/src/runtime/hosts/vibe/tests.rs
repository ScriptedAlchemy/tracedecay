use super::*;
use std::time::{Duration, SystemTime};

use serde_json::Value;
use tempfile::TempDir;

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

#[test]
fn vibe_workflow_lookalike_stays_ordinary_message_without_goal_kind() {
    // Vibe has no DurableObservation / WorkflowLifecycle normalizer yet.
    // Prove lookalike lifecycle bags are not promoted into session_messages
    // kind=goal (the legacy goals surface) or metadata keys.
    let input: Value = serde_json::from_str(include_str!(
        "../../../../../../tests/fixtures/provider_normalization/vibe/workflow_lookalike.input.json"
    ))
    .expect("Vibe workflow lookalike input");
    let meta = VibeMeta {
        session_id: "vibe-lookalike".to_string(),
        working_directory: PathBuf::from("/tmp/vibe-project"),
        model: Some("vibe-model".to_string()),
    };
    let message = message_from_line(
        &input,
        &meta,
        Path::new("/tmp/vibe-project/.vibe/logs/session/vibe-lookalike/messages.jsonl"),
        0,
    )
    .expect("lookalike must still parse as a transcript message");
    assert_eq!(message.kind.as_deref(), Some("message"));
    assert_eq!(
        message.text,
        "Vibe workflow lookalike remains an ordinary message"
    );
    let metadata: Value = serde_json::from_str(message.metadata_json.as_deref().unwrap()).unwrap();
    for key in ["workflow", "todos", "thread_goal_updated", "status", "kind"] {
        assert!(
            metadata.get(key).is_none(),
            "Vibe must not promote lookalike key {key} into message metadata"
        );
    }
    let encoded = message.metadata_json.unwrap_or_default();
    for rejected in [
        "vibe-hostile-task",
        "todo-hostile-1",
        "invented todo",
        "invented goal",
    ] {
        assert!(
            !encoded.contains(rejected),
            "{rejected} must not survive Vibe message metadata shaping"
        );
    }
}
