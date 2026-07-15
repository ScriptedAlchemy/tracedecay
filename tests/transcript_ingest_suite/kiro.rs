use tempfile::TempDir;
use tracedecay::sessions::cursor::open_project_session_db;
use tracedecay::sessions::kiro::KiroSource;
use tracedecay::sessions::source::{
    StoredCursor, TranscriptIngestError, TranscriptSource, ingest_source,
};

use crate::support::{assert_metadata_path_eq, create_git_repo_with_linked_worktree, setup};

fn encode_workspace_path(path: &std::path::Path) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let path_str = path.to_string_lossy();
    let bytes = path_str.as_bytes();
    let mut out = String::new();
    let mut buf = 0_u32;
    let mut bits = 0_u32;
    for &byte in bytes {
        buf = (buf << 8) | u32::from(byte);
        bits += 8;
        while bits >= 6 {
            bits -= 6;
            let idx = ((buf >> bits) & 0x3F) as usize;
            out.push(TABLE[idx] as char);
        }
    }
    if bits > 0 {
        buf <<= 6 - bits;
        let idx = (buf & 0x3F) as usize;
        out.push(TABLE[idx] as char);
    }
    out.replace('/', "_")
}

fn write_legacy_chat(
    home: &std::path::Path,
    project: &std::path::Path,
    workspace_hash: &str,
    execution_id: &str,
) -> std::path::PathBuf {
    let data_dir = tracedecay::agents::kiro_data_dir(home);
    let ws_storage = data_dir.join("User/workspaceStorage").join(workspace_hash);
    std::fs::create_dir_all(&ws_storage).unwrap();
    std::fs::write(
        ws_storage.join("workspace.json"),
        serde_json::json!({
            "folder": format!("file://{}", project.display())
        })
        .to_string(),
    )
    .unwrap();

    let agent_dir = data_dir
        .join("User/globalStorage/kiro.kiroagent")
        .join(workspace_hash);
    std::fs::create_dir_all(&agent_dir).unwrap();
    let chat_path = agent_dir.join(format!("{execution_id}.chat"));
    std::fs::write(
        &chat_path,
        serde_json::to_string_pretty(&serde_json::json!({
            "executionId": execution_id,
            "chat": [
                {"role": "human", "content": "Investigate the billing pipeline regression"},
                {"role": "bot", "content": "The billing pipeline regression is fixed."}
            ],
            "metadata": {
                "workflowId": "kiro-workflow-1",
                "modelId": "claude-sonnet-4.6",
                "startTime": 1_800_000_000_i64
            }
        }))
        .unwrap(),
    )
    .unwrap();
    chat_path
}

fn write_workspace_session_json(
    home: &std::path::Path,
    project: &std::path::Path,
    session_id: &str,
) -> std::path::PathBuf {
    let data_dir = tracedecay::agents::kiro_data_dir(home);
    let encoded = encode_workspace_path(project);
    let session_dir = data_dir
        .join("User/globalStorage/kiro.kiroagent/workspace-sessions")
        .join(encoded);
    std::fs::create_dir_all(&session_dir).unwrap();
    let path = session_dir.join(format!("{session_id}.json"));
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::json!({
            "sessionId": session_id,
            "modelId": "claude-sonnet-4.6",
            "messages": [
                {"role": "user", "content": "Investigate the billing pipeline regression", "timestamp": 1_800_000_000_000_i64},
                {"role": "assistant", "content": "The billing pipeline regression is fixed.", "timestamp": 1_800_000_010_000_i64}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    path
}

fn write_extensionless_execution(
    home: &std::path::Path,
    project: &std::path::Path,
    workspace_hash: &str,
    session_id: &str,
) -> std::path::PathBuf {
    let data_dir = tracedecay::agents::kiro_data_dir(home);
    let ws_storage = data_dir.join("User/workspaceStorage").join(workspace_hash);
    std::fs::create_dir_all(&ws_storage).unwrap();
    std::fs::write(
        ws_storage.join("workspace.json"),
        serde_json::json!({"folder": format!("file://{}", project.display())}).to_string(),
    )
    .unwrap();
    let path = data_dir
        .join("User/globalStorage/kiro.kiroagent")
        .join(workspace_hash)
        .join(session_id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        serde_json::json!({
            "sessionId": session_id,
            "messages": [
                {"role": "user", "content": "Remember my preferred review style"},
                {"role": "assistant", "content": "I will remember that preference."}
            ]
        })
        .to_string(),
    )
    .unwrap();
    path
}

#[tokio::test]
async fn kiro_legacy_chat_populates_searchable_messages() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let linked_worktree = tmp.path().join("linked-worktree");
    create_git_repo_with_linked_worktree(&project, &linked_worktree);
    write_legacy_chat(
        &home,
        &linked_worktree,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "exec-1",
    );

    let db = open_project_session_db(&project).await.unwrap();
    let source = KiroSource::with_home(&home);
    let stats = ingest_source(&db, &source, &project, None).await;
    assert_eq!(stats.messages_upserted, 2);

    let results = db
        .search_session_messages(
            "kiro",
            Some(project.to_string_lossy().as_ref()),
            "billing pipeline",
            10,
        )
        .await;
    assert_eq!(results.len(), 2);
    assert!(results.iter().any(|hit| {
        hit.message.model.as_deref() == Some("claude-sonnet-4-6")
            || hit.message.model.as_deref() == Some("claude-sonnet-4.6")
    }));
    let session = db.get_session("kiro", "kiro-workflow-1").await.unwrap();
    let session_metadata: serde_json::Value =
        serde_json::from_str(session.metadata_json.as_deref().unwrap()).unwrap();
    assert_metadata_path_eq(&session_metadata["kiro_workspace_cwd"], &linked_worktree);
    assert_metadata_path_eq(
        &session_metadata["kiro_workspace_worktree"],
        &linked_worktree,
    );
    assert_eq!(
        session_metadata["kiro_workspace_location_provenance"].as_str(),
        Some("workspace_mapping")
    );
    assert_eq!(session.started_at, Some(1_800_000_000));
    assert_eq!(session.ended_at, Some(1_800_000_001));
    let first = results
        .iter()
        .find(|hit| hit.message.ordinal == 0)
        .expect("first Kiro message");
    let first_metadata: serde_json::Value =
        serde_json::from_str(first.message.metadata_json.as_deref().unwrap()).unwrap();
    assert_metadata_path_eq(&first_metadata["kiro_workspace_cwd"], &linked_worktree);
    assert_metadata_path_eq(&first_metadata["kiro_workspace_worktree"], &linked_worktree);
    assert_eq!(
        first_metadata["kiro_workspace_location_provenance"].as_str(),
        Some("workspace_mapping")
    );
    assert_eq!(first.message.timestamp, Some(1_800_000_000));
    let second = results
        .iter()
        .find(|hit| hit.message.ordinal == 1)
        .expect("second Kiro message");
    assert_eq!(second.message.timestamp, Some(1_800_000_001));

    assert_eq!(
        ingest_source(&db, &source, &project, None)
            .await
            .messages_upserted,
        0
    );
}

#[tokio::test]
async fn kiro_workspace_sessions_json_is_ingested() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let linked_worktree = tmp.path().join("linked-worktree");
    create_git_repo_with_linked_worktree(&project, &linked_worktree);
    write_workspace_session_json(&home, &linked_worktree, "sess-modern");

    let db = open_project_session_db(&project).await.unwrap();
    let source = KiroSource::with_home(&home);
    let stats = ingest_source(&db, &source, &project, None).await;
    assert_eq!(stats.messages_upserted, 2);

    let results = db
        .search_session_messages(
            "kiro",
            Some(project.to_string_lossy().as_ref()),
            "billing pipeline",
            10,
        )
        .await;
    assert_eq!(results.len(), 2);
    let session = db.get_session("kiro", "sess-modern").await.unwrap();
    let session_metadata: serde_json::Value =
        serde_json::from_str(session.metadata_json.as_deref().unwrap()).unwrap();
    assert_metadata_path_eq(&session_metadata["kiro_workspace_cwd"], &linked_worktree);
    assert_metadata_path_eq(
        &session_metadata["kiro_workspace_worktree"],
        &linked_worktree,
    );
    assert_eq!(
        session_metadata["kiro_workspace_location_provenance"].as_str(),
        Some("workspace_mapping")
    );
    assert_eq!(session.started_at, Some(1_800_000_000));
    assert_eq!(session.ended_at, Some(1_800_000_010));
    let first = results
        .iter()
        .find(|hit| hit.message.ordinal == 0)
        .expect("first Kiro message");
    let first_metadata: serde_json::Value =
        serde_json::from_str(first.message.metadata_json.as_deref().unwrap()).unwrap();
    assert_metadata_path_eq(&first_metadata["kiro_workspace_cwd"], &linked_worktree);
    assert_metadata_path_eq(&first_metadata["kiro_workspace_worktree"], &linked_worktree);
    assert_eq!(
        first_metadata["kiro_workspace_location_provenance"].as_str(),
        Some("workspace_mapping")
    );
    assert_eq!(first.message.timestamp, Some(1_800_000_000));
    let second = results
        .iter()
        .find(|hit| hit.message.ordinal == 1)
        .expect("second Kiro message");
    assert_eq!(second.message.timestamp, Some(1_800_000_010));
}

#[tokio::test]
async fn kiro_fallback_identity_survives_snapshot_insertion() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let path = write_workspace_session_json(&home, &project, "sess-stable");

    let db = open_project_session_db(&project).await.unwrap();
    let source = KiroSource::with_home(&home);
    ingest_source(&db, &source, &project, None).await;
    let before = db
        .search_session_messages("kiro", None, "regression is fixed", 10)
        .await;
    let assistant_id = before
        .iter()
        .find(|hit| hit.message.role == "assistant")
        .expect("assistant before insertion")
        .message
        .message_id
        .clone();

    std::fs::write(
        &path,
        serde_json::json!({
            "sessionId": "sess-stable",
            "modelId": "claude-sonnet-4.6",
            "messages": [
                {"role": "user", "content": "New earlier context", "timestamp": 1_799_999_999_000_i64},
                {"role": "user", "content": "Investigate the billing pipeline regression", "timestamp": 1_800_000_000_000_i64},
                {"role": "assistant", "content": "The billing pipeline regression is fixed.", "timestamp": 1_800_000_010_000_i64}
            ]
        })
        .to_string(),
    )
    .unwrap();
    ingest_source(&db, &source, &project, None).await;

    let after = db
        .search_session_messages("kiro", None, "regression is fixed", 10)
        .await;
    let assistant = after
        .iter()
        .find(|hit| hit.message.role == "assistant")
        .expect("assistant after insertion");
    assert_eq!(assistant.message.message_id, assistant_id);
}

#[test]
fn kiro_complete_malformed_snapshot_is_typed_non_durable() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let path = write_workspace_session_json(&home, &project, "sess-malformed");
    std::fs::write(&path, "{not-json]").unwrap();

    let source = KiroSource::with_home(&home);
    assert!(matches!(
        source.try_parse_new(&path, StoredCursor::default(), &project, None),
        Err(TranscriptIngestError::NonDurableRecord {
            provider: "kiro",
            reason: "malformed snapshot JSON",
            ..
        })
    ));
}

#[tokio::test]
async fn kiro_incomplete_snapshot_does_not_advance_frontier() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let path = write_workspace_session_json(&home, &project, "sess-incomplete");
    std::fs::write(&path, r#"{"sessionId":"sess-incomplete","messages":["#).unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let source = KiroSource::with_home(&home);
    assert_eq!(
        ingest_source(&db, &source, &project, None)
            .await
            .messages_upserted,
        0
    );
    assert!(
        db.get_parse_offset(path.to_string_lossy().as_ref())
            .await
            .is_none()
    );

    write_workspace_session_json(&home, &project, "sess-incomplete");
    assert_eq!(
        ingest_source(&db, &source, &project, None)
            .await
            .messages_upserted,
        2
    );
}

#[tokio::test]
async fn kiro_transcript_for_other_project_is_skipped() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let other = tmp.path().join("other-project");
    std::fs::create_dir_all(&other).unwrap();
    write_legacy_chat(
        &home,
        &other,
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "exec-other",
    );

    let db = open_project_session_db(&project).await.unwrap();
    let source = KiroSource::with_home(&home);
    assert_eq!(
        ingest_source(&db, &source, &project, None)
            .await
            .messages_upserted,
        0
    );
}

#[tokio::test]
async fn kiro_user_scope_includes_only_unregistered_sessions() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let projectless = tmp.path().join("general-chat");
    std::fs::create_dir_all(&projectless).unwrap();
    write_legacy_chat(
        &home,
        &project,
        "cccccccccccccccccccccccccccccccc",
        "registered-exec",
    );
    write_workspace_session_json(&home, &projectless, "user-kiro");
    write_extensionless_execution(
        &home,
        &projectless,
        "dddddddddddddddddddddddddddddddd",
        "user-extensionless",
    );

    let db = open_project_session_db(&project).await.unwrap();
    let source = KiroSource::with_home(&home).for_user_scope(vec![project.clone()]);
    let stats = ingest_source(&db, &source, tmp.path(), None).await;
    assert_eq!(stats.messages_upserted, 4);
    assert!(db.get_session("kiro", "kiro-workflow-1").await.is_none());
    let session = db.get_session("kiro", "user-kiro").await.unwrap();
    assert_eq!(session.project_key, "user");
    assert_eq!(session.project_path, "user");
    let extensionless = db.get_session("kiro", "user-extensionless").await.unwrap();
    assert_eq!(extensionless.project_key, "user");
}
