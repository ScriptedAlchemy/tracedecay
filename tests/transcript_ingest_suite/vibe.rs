use std::io::Write;

use tempfile::TempDir;
use tracedecay::sessions::cursor::open_project_session_db;
use tracedecay::sessions::source::{TranscriptSource, ingest_source};
use tracedecay::sessions::vibe::VibeSource;

use crate::support::{assert_metadata_path_eq, create_git_repo_with_linked_worktree, setup};

fn write_vibe_session(
    home: &std::path::Path,
    project: &std::path::Path,
    session_id: &str,
) -> std::path::PathBuf {
    let dir = home
        .join(".vibe/logs/session")
        .join(format!("session_20260608_010000_{session_id}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("meta.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "session_id": session_id,
            "environment": {"working_directory": project},
            "config": {"active_model": "mistral-medium-3.5"}
        }))
        .unwrap(),
    )
    .unwrap();
    let messages = dir.join("messages.jsonl");
    std::fs::write(
        &messages,
        format!(
            "{}\n{}\n",
            serde_json::json!({
                "role": "user",
                "content": "Investigate the billing pipeline regression",
                "timestamp": 1_800_000_000_i64
            }),
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {"text": "The billing pipeline regression is fixed."},
                    {"tool_call": {"name": "read_file"}}
                ],
                "timestamp": 1_800_000_010_i64
            }),
        ),
    )
    .unwrap();
    messages
}

#[tokio::test]
async fn vibe_messages_populate_searchable_session_messages() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let linked_worktree = tmp.path().join("linked-worktree");
    create_git_repo_with_linked_worktree(&project, &linked_worktree);
    write_vibe_session(&home, &linked_worktree, "vibe-sess");

    let db = open_project_session_db(&project).await.unwrap();
    let source = VibeSource::with_home(&home);
    let stats = ingest_source(&db, &source, &project, None).await;
    assert_eq!(stats.messages_upserted, 2);

    let results = db
        .search_session_messages(
            "vibe",
            Some(project.to_string_lossy().as_ref()),
            "billing pipeline",
            10,
        )
        .await;
    assert_eq!(results.len(), 2);
    assert!(
        results
            .iter()
            .any(|hit| hit.message.tool_names.as_deref() == Some("read_file"))
    );
    assert!(
        results
            .iter()
            .all(|hit| hit.message.model.as_deref() == Some("mistral-medium-3.5"))
    );

    let assistant = results
        .iter()
        .find(|hit| hit.message.tool_names.as_deref() == Some("read_file"))
        .expect("assistant tool-call message should be searchable");
    let expected_content = serde_json::json!([
        {"text": "The billing pipeline regression is fixed."},
        {"tool_call": {"name": "read_file"}}
    ]);
    let raw = db
        .lcm_load_raw_message("vibe", &assistant.message.message_id)
        .await
        .expect("structured Vibe content should be in raw LCM storage");
    assert_eq!(
        raw.content,
        serde_json::to_string(&expected_content).unwrap()
    );
    let message_metadata: serde_json::Value =
        serde_json::from_str(assistant.message.metadata_json.as_deref().unwrap()).unwrap();
    assert_metadata_path_eq(&message_metadata["vibe_session_cwd"], &linked_worktree);
    assert_metadata_path_eq(&message_metadata["vibe_session_worktree"], &linked_worktree);
    assert_eq!(
        message_metadata["vibe_session_location_provenance"].as_str(),
        Some("session_meta")
    );
    let session = db.get_session("vibe", "vibe-sess").await.unwrap();
    let session_metadata: serde_json::Value =
        serde_json::from_str(session.metadata_json.as_deref().unwrap()).unwrap();
    assert_metadata_path_eq(&session_metadata["vibe_session_cwd"], &linked_worktree);
    assert_metadata_path_eq(&session_metadata["vibe_session_worktree"], &linked_worktree);
    assert_eq!(
        session_metadata["vibe_session_location_provenance"].as_str(),
        Some("session_meta")
    );
}

#[tokio::test]
async fn vibe_messages_are_incremental() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let messages = write_vibe_session(&home, &project, "vibe-sess");

    let db = open_project_session_db(&project).await.unwrap();
    let source = VibeSource::with_home(&home);
    assert_eq!(
        ingest_source(&db, &source, &project, None)
            .await
            .messages_upserted,
        2
    );
    assert_eq!(
        ingest_source(&db, &source, &project, None)
            .await
            .messages_upserted,
        0
    );

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&messages)
        .unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({
            "role": "assistant",
            "content": "Added the regression test.",
            "timestamp": 1_800_000_020_i64
        })
    )
    .unwrap();
    drop(file);

    assert_eq!(
        ingest_source(&db, &source, &project, None)
            .await
            .messages_upserted,
        1
    );
}

#[tokio::test]
async fn vibe_session_for_other_project_is_skipped() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let other = tmp.path().join("other-project");
    std::fs::create_dir_all(&other).unwrap();
    write_vibe_session(&home, &other, "other-vibe");

    let db = open_project_session_db(&project).await.unwrap();
    let source = VibeSource::with_home(&home);
    assert_eq!(
        ingest_source(&db, &source, &project, None)
            .await
            .messages_upserted,
        0
    );
}

#[tokio::test]
async fn vibe_user_scope_includes_only_unregistered_sessions() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let projectless = tmp.path().join("general-chat");
    std::fs::create_dir_all(&projectless).unwrap();
    write_vibe_session(&home, &project, "registered-vibe");
    write_vibe_session(&home, &projectless, "user-vibe");

    let db = open_project_session_db(&project).await.unwrap();
    let source = VibeSource::with_home(&home).for_user_scope(vec![project.clone()]);
    let stats = ingest_source(&db, &source, tmp.path(), None).await;
    assert_eq!(stats.messages_upserted, 2);
    assert!(db.get_session("vibe", "registered-vibe").await.is_none());
    let session = db.get_session("vibe", "user-vibe").await.unwrap();
    assert_eq!(session.project_key, "user");
    assert_eq!(session.project_path, "user");
}

#[cfg(unix)]
#[tokio::test]
async fn vibe_unknown_project_membership_defers_persistence_and_offset() {
    const CHILD_ENV: &str = "TRACEDECAY_VIBE_UNKNOWN_MEMBERSHIP_CHILD";
    if std::env::var_os(CHILD_ENV).is_some() {
        let tmp = TempDir::new().unwrap();
        let (home, project) = setup(&tmp);
        let nested = project.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let messages = write_vibe_session(&home, &nested, "unknown-vibe");

        let db = open_project_session_db(&project).await.unwrap();
        let source = VibeSource::with_home(&home).for_user_scope(vec![project]);
        assert_eq!(
            ingest_source(&db, &source, tmp.path(), None)
                .await
                .messages_upserted,
            0
        );
        assert!(db.get_session("vibe", "unknown-vibe").await.is_none());
        assert!(
            db.get_parse_offset(messages.to_string_lossy().as_ref())
                .await
                .is_none()
        );
        return;
    }

    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let tmp = TempDir::new().unwrap();
    let fake_git = tmp.path().join("git-timeout");
    std::fs::write(&fake_git, "#!/bin/sh\nexec /bin/sleep 3\n").unwrap();
    std::fs::set_permissions(&fake_git, std::fs::Permissions::from_mode(0o755)).unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .arg("vibe::vibe_unknown_project_membership_defers_persistence_and_offset")
        .arg("--exact")
        .env(CHILD_ENV, "1")
        .env("GIT", fake_git)
        .env("GIT_DIR", "/nonexistent/tracedecay-vibe-timeout-git-dir")
        .status()
        .unwrap();
    assert!(
        status.success(),
        "child must defer unknown project membership"
    );
}

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
    assert_eq!(source.transcript_paths(home).len(), 512);
}
