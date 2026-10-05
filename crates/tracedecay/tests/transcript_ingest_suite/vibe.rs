use tempfile::TempDir;
use tracedecay_sessions::runtime::SessionProvider;
use tracedecay_sessions::runtime::hosts::vibe::VibeSource;
use tracedecay_sessions::runtime::source::{
    FileDiscoveryLimit, TranscriptDiscoveryBounds, TranscriptSource,
};

use crate::restart_atomicity::{ingest_global_sources_for_provider, open_project_session_db};
use crate::support::{assert_metadata_path_eq, assert_path_text_eq, setup};

#[cfg(unix)]
use tracedecay_runtime_core::test_executable::write_executable_script;

#[test]
fn vibe_history_enumeration_is_bounded() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    for index in 0..513 {
        let dir = home.join(format!(".vibe/logs/session/session-{index:04}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("messages.jsonl"), "").unwrap();
    }
    let source = VibeSource::with_home(home);
    let report = source
        .discover_transcript_paths(home, TranscriptDiscoveryBounds::from_discovered_units(512));
    assert_eq!(report.paths.len(), 512);
    assert_eq!(report.truncated, Some(FileDiscoveryLimit::FileCount));
    assert_eq!(source.transcript_paths(home).len(), 512);
}

#[test]
fn vibe_ineligible_jsonl_does_not_crowd_eligible_under_cap() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let root = home.join(".vibe/logs/session");
    for index in 0..40 {
        let dir = root.join(format!("noise-{index:02}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("events.jsonl"), "").unwrap();
    }
    for (name, stamp) in [("session-old", 10u64), ("session-new", 20u64)] {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("messages.jsonl");
        std::fs::write(&path, "").unwrap();
        let mtime = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(stamp);
        filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(mtime)).unwrap();
    }
    let source = VibeSource::with_home(home);
    let report = source.discover_transcript_paths(
        home,
        TranscriptDiscoveryBounds {
            max_files: 2,
            ..TranscriptDiscoveryBounds::from_discovered_units(2)
        },
    );
    assert_eq!(report.paths.len(), 2);
    assert!(
        report.paths[0].ends_with("session-new/messages.jsonl"),
        "newest eligible must win despite ineligible crowding: {:?}",
        report.paths
    );
    assert!(report.paths[1].ends_with("session-old/messages.jsonl"));
    assert!(!report.is_truncated());
}

#[test]
fn vibe_discovery_pagination_progresses_older_and_keeps_new_on_page_zero() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let root = home.join(".vibe/logs/session");
    for (name, stamp) in [
        ("session-a", 10u64),
        ("session-b", 20u64),
        ("session-c", 30u64),
    ] {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("messages.jsonl");
        std::fs::write(&path, "").unwrap();
        let mtime = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(stamp);
        filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(mtime)).unwrap();
    }
    let source = VibeSource::with_home(home);
    let bounds = TranscriptDiscoveryBounds {
        max_files: 2,
        ..TranscriptDiscoveryBounds::from_discovered_units(2)
    };
    let (page0, omitted0) = source.discover_transcript_paths_page(home, bounds, 0);
    assert_eq!(page0.paths.len(), 2);
    assert!(page0.paths[0].ends_with("session-c/messages.jsonl"));
    assert!(page0.paths[1].ends_with("session-b/messages.jsonl"));
    assert_eq!(page0.truncated, Some(FileDiscoveryLimit::FileCount));
    assert_eq!(omitted0, 1);

    let (page1, omitted1) = source.discover_transcript_paths_page(home, bounds, 2);
    assert_eq!(page1.paths.len(), 1);
    assert!(page1.paths[0].ends_with("session-a/messages.jsonl"));
    assert!(!page1.is_truncated());
    assert_eq!(omitted1, 2);

    let arrived_dir = root.join("session-d");
    std::fs::create_dir_all(&arrived_dir).unwrap();
    let arrived = arrived_dir.join("messages.jsonl");
    std::fs::write(&arrived, "").unwrap();
    let mtime = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(40);
    filetime::set_file_mtime(&arrived, filetime::FileTime::from_system_time(mtime)).unwrap();
    let (page0_after, _) = source.discover_transcript_paths_page(home, bounds, 0);
    assert!(page0_after.paths[0].ends_with("session-d/messages.jsonl"));
    assert!(
        page0_after
            .paths
            .iter()
            .any(|path| path.ends_with("session-c/messages.jsonl"))
    );
    assert!(
        !page0_after
            .paths
            .iter()
            .any(|path| path.ends_with("session-a/messages.jsonl")),
        "oldest must yield page-0 slots to newer arrivals"
    );
}

/// Re-runs this test binary with `GIT` pointing at a script that outlives the
/// bounded capture deadline, so every session-ingest git identity resolution
/// times out into `Unknown` inside the child.
#[cfg(unix)]
pub(super) fn run_unknown_membership_child(child_env: &str, test_name: &str) {
    use std::process::Command;

    let tmp = TempDir::new().unwrap();
    let fake_git = tmp.path().join("git-timeout");
    write_executable_script(&fake_git, "#!/bin/sh\nexec /bin/sleep 3\n").unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .arg(test_name)
        .arg("--exact")
        .env(child_env, "1")
        .env("GIT", fake_git)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "child must defer unknown project membership\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // `--exact` exits 0 on a name miss; require the non-vacuous single run.
    assert!(
        stdout.contains("1 passed"),
        "child filter must match exactly one test\nstdout:\n{stdout}"
    );
}

#[tokio::test]
async fn vibe_session_location_projects_session_metadata() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let session_dir = home.join(".vibe/logs/session/vibe-location");
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(
        session_dir.join("meta.json"),
        serde_json::json!({
            "session_id": "vibe-location",
            "environment": {"working_directory": project.to_string_lossy()},
            "model": "vibe-model"
        })
        .to_string(),
    )
    .unwrap();
    let transcript = session_dir.join("messages.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            "{\"role\":\"user\",\"content\":\"Investigate the billing pipeline regression\"}\n",
            "{\"role\":\"assistant\",\"content\":\"The billing pipeline regression is fixed.\"}\n"
        ),
    )
    .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    ingest_global_sources_for_provider(&home, &db, &project, Some(SessionProvider::Vibe)).await;

    let session = db.get_session("vibe", "vibe-location").await.unwrap();
    assert_path_text_eq(
        session
            .transcript_path
            .as_deref()
            .expect("session transcript path"),
        &transcript,
    );
    let metadata: serde_json::Value =
        serde_json::from_str(session.metadata_json.as_deref().unwrap()).unwrap();
    assert_metadata_path_eq(&metadata["vibe_session_cwd"], &project);
    assert_metadata_path_eq(&metadata["vibe_session_worktree"], &project);
    assert_eq!(
        metadata["vibe_session_location_provenance"].as_str(),
        Some("session_meta")
    );

    let hit = db
        .search_session_messages("vibe", None, "billing pipeline", 10)
        .await
        .into_iter()
        .next()
        .expect("the session message should be searchable");
    let message_metadata: serde_json::Value =
        serde_json::from_str(hit.message.metadata_json.as_deref().unwrap()).unwrap();
    assert_metadata_path_eq(&message_metadata["vibe_session_cwd"], &project);
    assert_metadata_path_eq(&message_metadata["vibe_session_worktree"], &project);
    assert_eq!(
        message_metadata["vibe_session_location_provenance"].as_str(),
        Some("session_meta")
    );
}
