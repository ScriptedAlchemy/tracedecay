use tempfile::TempDir;
use tracedecay::sessions::cursor::open_project_session_db;
use tracedecay::sessions::kiro::KiroSource;
use tracedecay::sessions::source::ingest_source;

fn setup(tmp: &TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir(project.join(".tracedecay")).unwrap();
    std::fs::write(project.join(".tracedecay/tracedecay.db"), "").unwrap();
    (home, project)
}

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

#[tokio::test]
async fn kiro_legacy_chat_populates_searchable_messages() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_legacy_chat(
        &home,
        &project,
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
    assert_eq!(session.started_at, Some(1_800_000_000));
    assert_eq!(session.ended_at, Some(1_800_000_001));
    let first = db
        .get_session_message("kiro", "kiro-workflow-1:0")
        .await
        .unwrap();
    assert_eq!(first.timestamp, Some(1_800_000_000));
    let second = db
        .get_session_message("kiro", "kiro-workflow-1:1")
        .await
        .unwrap();
    assert_eq!(second.timestamp, Some(1_800_000_001));

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
    write_workspace_session_json(&home, &project, "sess-modern");

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
    assert_eq!(session.started_at, Some(1_800_000_000));
    assert_eq!(session.ended_at, Some(1_800_000_010));
    let first = db
        .get_session_message("kiro", "sess-modern:0")
        .await
        .unwrap();
    assert_eq!(first.timestamp, Some(1_800_000_000));
    let second = db
        .get_session_message("kiro", "sess-modern:1")
        .await
        .unwrap();
    assert_eq!(second.timestamp, Some(1_800_000_010));
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
