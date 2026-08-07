use tempfile::TempDir;
use tracedecay::application::host_admission::HostAdmissionScope;
use tracedecay::sessions::SessionProvider;
use tracedecay::sessions::kiro::KiroSource;
use tracedecay::sessions::source::{StoredCursor, TranscriptIngestError, TranscriptSource};
use tracedecay_store::ObservationProjectionStore;

use crate::common::{EnvVarGuard, GLOBAL_DB_ENV_LOCK};
use crate::restart_atomicity::{
    assert_secret_absent_from_observation_sinks, durable_table_count,
    ingest_global_sources_for_provider, mark_test_project, observation_source_cursor,
    open_project_session_db, set_projection_failure, try_ingest_source,
};
use crate::support::{
    assert_metadata_path_eq, create_git_repo_with_linked_worktree, init_git_repo, setup,
};

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
            "folder": url::Url::from_file_path(project)
                .expect("project has a portable file URI")
                .to_string()
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
        serde_json::json!({
            "folder": url::Url::from_file_path(project)
                .expect("project has a portable file URI")
                .to_string()
        })
        .to_string(),
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
    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
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
        try_ingest_source(&db, &source, &project, None)
            .await
            .unwrap()
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
    let stats = try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
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
#[allow(clippy::await_holding_lock)]
async fn kiro_secret_is_sanitized_before_observation_and_projection() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let _home = EnvVarGuard::set("HOME", &home);
    init_git_repo(&project);
    mark_test_project(&project);
    let path = write_workspace_session_json(&home, &project, "kiro-secret");
    let secret = "sk-proj-kiro-canary-1234567890";
    std::fs::write(
        path,
        serde_json::json!({
            "sessionId": "kiro-secret",
            "messages": [{
                "role": "user",
                "content": format!("Kiro sanitizer safe text: {secret}"),
                "timestamp": 1_800_000_000_000_i64
            }]
        })
        .to_string(),
    )
    .unwrap();
    let db = open_project_session_db(&project).await.unwrap();

    assert_eq!(
        ingest_global_sources_for_provider(&db, &project, Some(SessionProvider::Kiro))
            .await
            .messages_upserted,
        1
    );
    assert_eq!(
        db.search_session_messages("kiro", None, "Kiro sanitizer safe text", 10)
            .await
            .len(),
        1
    );
    assert_secret_absent_from_observation_sinks(&db, "kiro", secret).await;
}

#[tokio::test]
async fn kiro_unversioned_timestamp_free_identity_survives_insertion_and_reorder() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let path = write_workspace_session_json(&home, &project, "sess-stable");
    std::fs::write(
        &path,
        serde_json::json!({
            "sessionId": "sess-stable",
            "messages": [
                {"role": "user", "content": "Investigate the billing pipeline regression"},
                {"role": "assistant", "content": "The billing pipeline regression is fixed."},
                {"role": "assistant", "content": "The billing pipeline regression is fixed."}
            ]
        })
        .to_string(),
    )
    .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let source = KiroSource::with_home(&home);
    try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    let before = db
        .search_session_messages("kiro", None, "regression is fixed", 10)
        .await;
    let mut assistant_ids = before
        .iter()
        .filter(|hit| hit.message.role == "assistant")
        .map(|hit| hit.message.message_id.clone())
        .collect::<Vec<_>>();
    assistant_ids.sort();
    assistant_ids.dedup();
    assert_eq!(
        assistant_ids.len(),
        2,
        "identical messages need distinct IDs"
    );

    std::fs::write(
        &path,
        serde_json::json!({
            "sessionId": "sess-stable",
            "messages": [
                {"role": "assistant", "content": "The billing pipeline regression is fixed."},
                {"role": "user", "content": "New earlier context"},
                {"role": "user", "content": "Investigate the billing pipeline regression"},
                {"role": "assistant", "content": "The billing pipeline regression is fixed."}
            ]
        })
        .to_string(),
    )
    .unwrap();
    try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();

    let after = db
        .search_session_messages("kiro", None, "regression is fixed", 10)
        .await;
    let mut reordered_ids = after
        .iter()
        .filter(|hit| hit.message.role == "assistant")
        .map(|hit| hit.message.message_id.clone())
        .collect::<Vec<_>>();
    reordered_ids.sort();
    reordered_ids.dedup();
    assert_eq!(reordered_ids, assistant_ids);
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
        try_ingest_source(&db, &source, &project, None)
            .await
            .unwrap()
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
        try_ingest_source(&db, &source, &project, None)
            .await
            .unwrap()
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
        try_ingest_source(&db, &source, &project, None)
            .await
            .unwrap()
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
    let stats = try_ingest_source(&db, &source, tmp.path(), None)
        .await
        .unwrap();
    assert_eq!(stats.messages_upserted, 4);
    assert!(db.get_session("kiro", "kiro-workflow-1").await.is_none());
    let session = db.get_session("kiro", "user-kiro").await.unwrap();
    assert_eq!(session.project_key, "user");
    assert_eq!(session.project_path, "user");
    let extensionless = db.get_session("kiro", "user-extensionless").await.unwrap();
    assert_eq!(extensionless.project_key, "user");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn kiro_restart_catchup_and_replaced_snapshot_are_deterministic() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let _home = EnvVarGuard::set("HOME", &home);
    init_git_repo(&project);
    mark_test_project(&project);
    let path = write_workspace_session_json(&home, &project, "sess-restart");

    let db = open_project_session_db(&project).await.unwrap();
    let _ = ingest_global_sources_for_provider(&db, &project, Some(SessionProvider::Kiro)).await;
    assert_eq!(db.session_message_count().await.unwrap(), 2);
    let first_cursor = observation_source_cursor(&db, "kiro", "sess-restart", &project)
        .await
        .expect("committed Kiro observation cursor");
    assert_eq!(first_cursor.position(), 2);
    drop(db);

    let replay = open_project_session_db(&project).await.unwrap();
    assert_eq!(
        ingest_global_sources_for_provider(&replay, &project, Some(SessionProvider::Kiro))
            .await
            .messages_upserted,
        0
    );
    assert_eq!(
        observation_source_cursor(&replay, "kiro", "sess-restart", &project).await,
        Some(first_cursor.clone())
    );
    drop(replay);

    // Grow/replace the snapshot with an additional durable message.
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::json!({
            "sessionId": "sess-restart",
            "modelId": "claude-sonnet-4.6",
            "messages": [
                {"role": "user", "content": "Investigate the billing pipeline regression", "timestamp": 1_800_000_000_000_i64},
                {"role": "assistant", "content": "The billing pipeline regression is fixed.", "timestamp": 1_800_000_010_000_i64},
                {"role": "user", "content": "Kiro restart catch-up suffix", "timestamp": 1_800_000_020_000_i64}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    let catchup = open_project_session_db(&project).await.unwrap();
    let _ =
        ingest_global_sources_for_provider(&catchup, &project, Some(SessionProvider::Kiro)).await;
    assert_eq!(catchup.session_message_count().await.unwrap(), 3);
    let replaced_cursor = observation_source_cursor(&catchup, "kiro", "sess-restart", &project)
        .await
        .expect("committed Kiro observation cursor");
    assert_ne!(replaced_cursor.generation(), first_cursor.generation());
    assert_eq!(replaced_cursor.position(), 3);
    assert_eq!(
        catchup
            .search_session_messages("kiro", None, "Kiro restart catch-up", 10)
            .await
            .len(),
        1
    );
    assert_eq!(
        ingest_global_sources_for_provider(&catchup, &project, Some(SessionProvider::Kiro))
            .await
            .messages_upserted,
        0
    );
    assert_eq!(
        observation_source_cursor(&catchup, "kiro", "sess-restart", &project).await,
        Some(replaced_cursor)
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn kiro_delimiter_ambiguous_native_ids_survive_restart_and_rebuild() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let _home = EnvVarGuard::set("HOME", &home);
    init_git_repo(&project);
    mark_test_project(&project);
    let left_path = write_workspace_session_json(&home, &project, "delimiter-left");
    let right_path = write_workspace_session_json(&home, &project, "delimiter-right");
    std::fs::write(
        left_path,
        serde_json::json!({
            "sessionId": "a:b",
            "messages": [{
                "messageId": "c",
                "role": "assistant",
                "content": "Kiro delimiter collision fixture"
            }]
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        right_path,
        serde_json::json!({
            "sessionId": "a",
            "messages": [{
                "messageId": "b:c",
                "role": "assistant",
                "content": "Kiro delimiter collision fixture"
            }]
        })
        .to_string(),
    )
    .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let _ = ingest_global_sources_for_provider(&db, &project, Some(SessionProvider::Kiro)).await;
    let hits = db
        .search_session_messages("kiro", None, "delimiter collision fixture", 10)
        .await;
    assert_eq!(hits.len(), 2);
    let mut ids = hits
        .iter()
        .map(|hit| hit.message.message_id.as_str())
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 2);
    assert!(ids.iter().all(|id| id.starts_with("kiro.message-id.v2.")));
    drop(db);

    let reopened = open_project_session_db(&project).await.unwrap();
    assert_eq!(
        ingest_global_sources_for_provider(&reopened, &project, Some(SessionProvider::Kiro))
            .await
            .messages_upserted,
        0
    );
    let committed = durable_table_count(&reopened, "observations").await;
    let store = reopened
        .runtime()
        .observation_store(HostAdmissionScope::Project)
        .unwrap();
    loop {
        if store
            .rebuild_projection(committed)
            .await
            .unwrap()
            .is_complete()
        {
            break;
        }
    }
    let rebuilt = reopened
        .search_session_messages("kiro", None, "delimiter collision fixture", 10)
        .await;
    assert_eq!(rebuilt.len(), 2);
    assert_ne!(rebuilt[0].message.message_id, rebuilt[1].message.message_id);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn kiro_projection_failure_commits_frontier_and_replays_once() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let _home = EnvVarGuard::set("HOME", &home);
    init_git_repo(&project);
    mark_test_project(&project);
    let path = write_workspace_session_json(&home, &project, "sess-crash");

    let db = open_project_session_db(&project).await.unwrap();
    let _ = ingest_global_sources_for_provider(&db, &project, Some(SessionProvider::Kiro)).await;
    assert_eq!(db.session_message_count().await.unwrap(), 2);
    let prefix_cursor = observation_source_cursor(&db, "kiro", "sess-crash", &project)
        .await
        .expect("committed Kiro observation cursor");
    assert_eq!(prefix_cursor.position(), 2);
    drop(db);

    std::fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::json!({
            "sessionId": "sess-crash",
            "modelId": "claude-sonnet-4.6",
            "messages": [
                {"role": "user", "content": "Investigate the billing pipeline regression", "timestamp": 1_800_000_000_000_i64},
                {"role": "assistant", "content": "The billing pipeline regression is fixed.", "timestamp": 1_800_000_010_000_i64},
                {"role": "user", "content": "Kiro projection retry suffix", "timestamp": 1_800_000_020_000_i64}
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let failure_runtime = open_project_session_db(&project).await.unwrap();
    set_projection_failure(&failure_runtime, true).await;
    drop(failure_runtime);
    let rejected = open_project_session_db(&project).await.unwrap();
    let _ =
        ingest_global_sources_for_provider(&rejected, &project, Some(SessionProvider::Kiro)).await;
    let committed_cursor = observation_source_cursor(&rejected, "kiro", "sess-crash", &project)
        .await
        .expect("committed Kiro observation cursor");
    assert_ne!(committed_cursor.generation(), prefix_cursor.generation());
    assert_eq!(committed_cursor.position(), 3);
    assert_eq!(rejected.session_message_count().await.unwrap(), 2);
    assert!(
        rejected
            .search_session_messages("kiro", None, "projection retry suffix", 10)
            .await
            .is_empty()
    );
    drop(rejected);

    let recovery_runtime = open_project_session_db(&project).await.unwrap();
    set_projection_failure(&recovery_runtime, false).await;
    drop(recovery_runtime);
    let recovered = open_project_session_db(&project).await.unwrap();
    let _ =
        ingest_global_sources_for_provider(&recovered, &project, Some(SessionProvider::Kiro)).await;
    assert_eq!(recovered.session_message_count().await.unwrap(), 3);
    assert_eq!(
        recovered
            .search_session_messages("kiro", None, "projection retry suffix", 10)
            .await
            .len(),
        1
    );
    assert_eq!(
        observation_source_cursor(&recovered, "kiro", "sess-crash", &project).await,
        Some(committed_cursor)
    );
    assert_eq!(
        ingest_global_sources_for_provider(&recovered, &project, Some(SessionProvider::Kiro))
            .await
            .messages_upserted,
        0
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn kiro_conflicting_native_message_id_does_not_overwrite() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let _home = EnvVarGuard::set("HOME", &home);
    init_git_repo(&project);
    mark_test_project(&project);

    let session_dir = tracedecay::agents::kiro_data_dir(&home)
        .join("User/globalStorage/kiro.kiroagent/workspace-sessions")
        .join(encode_workspace_path(&project));
    std::fs::create_dir_all(&session_dir).unwrap();
    let path = session_dir.join("sess-conflict.json");
    let snapshot = |prompt: &str| {
        serde_json::json!({
            "sessionId": "sess-conflict",
            "modelId": "claude-sonnet-4.6",
            "messages": [
                {
                    "role": "user",
                    "messageId": "kiro-msg-user-1",
                    "content": prompt,
                    "timestamp": 1_800_000_000_000_i64
                },
                {
                    "role": "assistant",
                    "messageId": "kiro-msg-assistant-1",
                    "content": "The billing pipeline regression is fixed.",
                    "timestamp": 1_800_000_010_000_i64
                }
            ]
        })
    };
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&snapshot("Investigate the billing pipeline regression"))
            .unwrap(),
    )
    .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let _ = ingest_global_sources_for_provider(&db, &project, Some(SessionProvider::Kiro)).await;
    assert_eq!(db.session_message_count().await.unwrap(), 2);
    let prefix_cursor = observation_source_cursor(&db, "kiro", "sess-conflict", &project)
        .await
        .expect("committed Kiro observation cursor");
    drop(db);

    std::fs::write(
        &path,
        serde_json::to_string_pretty(&snapshot(
            "The conflicting Kiro identity tried to replace the billing prompt",
        ))
        .unwrap(),
    )
    .unwrap();
    let rejected = open_project_session_db(&project).await.unwrap();
    let _ =
        ingest_global_sources_for_provider(&rejected, &project, Some(SessionProvider::Kiro)).await;
    assert_eq!(rejected.session_message_count().await.unwrap(), 2);
    assert_eq!(
        rejected
            .search_session_messages("kiro", None, "Investigate", 10)
            .await
            .len(),
        1
    );
    assert!(
        rejected
            .search_session_messages("kiro", None, "conflicting", 10)
            .await
            .is_empty()
    );
    assert_eq!(
        observation_source_cursor(&rejected, "kiro", "sess-conflict", &project).await,
        Some(prefix_cursor)
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn kiro_observation_commit_before_ack_survives_reopen() {
    let _env_lock = GLOBAL_DB_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let _home = EnvVarGuard::set("HOME", &home);
    init_git_repo(&project);
    mark_test_project(&project);
    let _path = write_workspace_session_json(&home, &project, "sess-commit-before-ack");

    let failure_runtime = open_project_session_db(&project).await.unwrap();
    set_projection_failure(&failure_runtime, true).await;
    drop(failure_runtime);

    let rejected = open_project_session_db(&project).await.unwrap();
    let _ =
        ingest_global_sources_for_provider(&rejected, &project, Some(SessionProvider::Kiro)).await;
    assert_eq!(rejected.session_message_count().await.unwrap(), 0);
    assert!(
        rejected
            .search_session_messages("kiro", None, "billing pipeline regression", 10)
            .await
            .is_empty()
    );
    let committed_cursor =
        observation_source_cursor(&rejected, "kiro", "sess-commit-before-ack", &project)
            .await
            .expect("Kiro observation frontier commits before projection ack");
    assert_eq!(committed_cursor.position(), 2);
    drop(rejected);

    let durable_runtime = open_project_session_db(&project).await.unwrap();
    let observations = durable_table_count(&durable_runtime, "observations").await;
    let receipts = durable_table_count(&durable_runtime, "sanitization_receipts").await;
    let queued = durable_table_count(&durable_runtime, "projection_queue").await;
    assert!(
        observations >= 1,
        "Kiro observation commits before projection ack"
    );
    assert!(
        receipts >= 1,
        "Kiro sanitization receipts commit with observations"
    );
    assert!(
        queued >= 1,
        "Kiro projection work stays queued across the failed ack"
    );

    set_projection_failure(&durable_runtime, false).await;
    drop(durable_runtime);
    let recovered = open_project_session_db(&project).await.unwrap();
    assert_eq!(
        ingest_global_sources_for_provider(&recovered, &project, Some(SessionProvider::Kiro))
            .await
            .messages_upserted,
        2
    );
    assert_eq!(
        recovered
            .search_session_messages("kiro", None, "fixed", 10)
            .await
            .len(),
        1
    );
    assert_eq!(
        durable_table_count(&recovered, "observations").await,
        observations
    );
    assert_eq!(
        durable_table_count(&recovered, "sanitization_receipts").await,
        receipts
    );
    assert_eq!(durable_table_count(&recovered, "projection_queue").await, 0);
    assert_eq!(
        ingest_global_sources_for_provider(&recovered, &project, Some(SessionProvider::Kiro))
            .await
            .messages_upserted,
        0
    );
}

/// A bounded git timeout (`Unknown` membership) must exclude the session
/// without persisting any cursor, so a later pass can re-resolve it.
#[cfg(unix)]
#[tokio::test]
async fn kiro_unknown_project_membership_defers_persistence_and_offset() {
    const CHILD_ENV: &str = "TRACEDECAY_KIRO_UNKNOWN_MEMBERSHIP_CHILD";
    if std::env::var_os(CHILD_ENV).is_some() {
        let tmp = TempDir::new().unwrap();
        let (home, project) = setup(&tmp);
        let nested = project.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let transcript = write_workspace_session_json(&home, &nested, "unknown-kiro");

        let db = open_project_session_db(&project).await.unwrap();
        let source = KiroSource::with_home(&home).for_user_scope(vec![project]);
        assert_eq!(
            try_ingest_source(&db, &source, tmp.path(), None)
                .await
                .unwrap()
                .messages_upserted,
            0
        );
        assert!(db.get_session("kiro", "unknown-kiro").await.is_none());
        assert!(
            db.get_parse_offset(transcript.to_string_lossy().as_ref())
                .await
                .is_none()
        );
        return;
    }

    crate::vibe::run_unknown_membership_child(
        CHILD_ENV,
        "kiro::kiro_unknown_project_membership_defers_persistence_and_offset",
    );
}
