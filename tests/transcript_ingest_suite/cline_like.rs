use tempfile::TempDir;
use tracedecay::global_db::{GlobalDb, ParseOffset};
use tracedecay::sessions::cline_like::ClineLikeSource;
use tracedecay::sessions::cursor::{open_project_session_db, project_session_db_path};
use tracedecay::sessions::source::ingest_source;

use crate::support::setup;

fn vscode_storage_root(home: &std::path::Path, extension_id: &str) -> std::path::PathBuf {
    tracedecay::agents::vscode_data_dir(home)
        .join("User/globalStorage")
        .join(extension_id)
        .join("tasks")
}

async fn parse_offset_for_path(db: &GlobalDb, path: &std::path::Path) -> Option<ParseOffset> {
    let path = path.to_string_lossy();
    if let Some(offset) = db.get_parse_offset(path.as_ref()).await {
        return Some(offset);
    }

    #[cfg(windows)]
    {
        let alternate = if path.contains('/') {
            path.replace('/', "\\")
        } else {
            path.replace('\\', "/")
        };
        if alternate != path {
            return db.get_parse_offset(&alternate).await;
        }
    }

    None
}

async fn parse_offset_for_task_history(
    db: &GlobalDb,
    project: &std::path::Path,
    path: &std::path::Path,
) -> Option<ParseOffset> {
    if let Some(offset) = parse_offset_for_path(db, path).await {
        return Some(offset);
    }

    let task_dir = path.parent()?.file_name()?.to_string_lossy();
    let file_name = path.file_name()?.to_string_lossy();
    let expected_suffix = format!("{task_dir}/{file_name}");
    let raw_db = libsql::Builder::new_local(project_session_db_path(project))
        .build()
        .await
        .ok()?;
    let conn = raw_db.connect().ok()?;
    let mut rows = conn
        .query(
            "SELECT file_path, byte_offset, mtime, file_id FROM parse_offsets",
            (),
        )
        .await
        .ok()?;
    while let Some(row) = rows.next().await.ok()? {
        let file_path: String = row.get(0).ok()?;
        let normalized = file_path.replace('\\', "/");
        if normalized.ends_with(&expected_suffix) {
            let offset: i64 = row.get(1).ok()?;
            let mtime: i64 = row.get(2).ok()?;
            let file_id: i64 = row.get(3).ok()?;
            return Some(ParseOffset {
                byte_offset: offset as u64,
                mtime: mtime as u64,
                file_id: file_id as u64,
            });
        }
    }
    None
}

fn write_task(
    root: &std::path::Path,
    project: &std::path::Path,
    task_id: &str,
) -> std::path::PathBuf {
    let dir = root.join(task_id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("task_metadata.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "task": "Investigate the billing pipeline regression",
            "workspacePath": project
        }))
        .unwrap(),
    )
    .unwrap();
    let api = dir.join("api_conversation_history.json");
    std::fs::write(
        &api,
        serde_json::to_string_pretty(&serde_json::json!([
            {
                "role": "user",
                "content": "Investigate the billing pipeline regression",
                "ts": 1_800_000_000_i64
            },
            {
                "role": "assistant",
                "model": "claude-sonnet-4.6",
                "content": [
                    {"type": "text", "text": "The billing pipeline regression is fixed."},
                    {"type": "tool_use", "name": "read_file"}
                ],
                "ts": 1_800_000_010_i64
            }
        ]))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.join("ui_messages.json"),
        serde_json::to_string_pretty(&serde_json::json!([
            {
                "type": "say",
                "say": "api_req_started",
                "ts": 1_800_000_005_i64,
                "text": serde_json::json!({
                    "tokensIn": 1200,
                    "tokensOut": 350,
                    "cacheReads": 8000,
                    "cacheWrites": 500,
                    "cost": 0.12
                }).to_string()
            }
        ]))
        .unwrap(),
    )
    .unwrap();
    api
}

async fn assert_provider_ingests(
    provider: &str,
    source: ClineLikeSource,
    db: &tracedecay::global_db::GlobalDb,
    project: &std::path::Path,
) {
    let stats = ingest_source(db, &source, project, None).await;
    assert_eq!(stats.messages_upserted, 2);

    let results = db
        .search_session_messages(
            provider,
            Some(project.to_string_lossy().as_ref()),
            "billing pipeline",
            10,
        )
        .await;
    assert_eq!(results.len(), 2);
    assert!(results
        .iter()
        .any(|hit| hit.message.tool_names.as_deref() == Some("read_file")));
    // The `ts` fields land as per-message timestamps.
    assert!(results
        .iter()
        .any(|hit| hit.message.timestamp == Some(1_800_000_000)));
    assert!(results
        .iter()
        .any(|hit| hit.message.timestamp == Some(1_800_000_010)));
    let assistant = results
        .iter()
        .find(|hit| hit.message.tool_names.as_deref() == Some("read_file"))
        .expect("assistant tool-use message should be searchable");
    let metadata: serde_json::Value =
        serde_json::from_str(assistant.message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["usage"]["input_tokens"], 1200);
    assert_eq!(metadata["usage"]["output_tokens"], 350);
    assert_eq!(metadata["usage"]["cache_read_input_tokens"], 8000);
    assert_eq!(metadata["usage"]["cache_creation_input_tokens"], 500);
    let expected_content = serde_json::json!([
        {"type": "text", "text": "The billing pipeline regression is fixed."},
        {"type": "tool_use", "name": "read_file"}
    ]);
    let raw = db
        .lcm_load_raw_message(provider, &assistant.message.message_id)
        .await
        .expect("structured Cline-like content should be in raw LCM storage");
    assert_eq!(
        raw.content,
        serde_json::to_string(&expected_content).unwrap()
    );

    // ContentHash: unchanged full-rewrite file is a no-op.
    assert_eq!(
        ingest_source(db, &source, project, None)
            .await
            .messages_upserted,
        0
    );
}

#[tokio::test]
async fn cline_task_history_populates_searchable_messages() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_task(
        &vscode_storage_root(&home, "saoudrizwan.claude-dev"),
        &project,
        "cline-task",
    );

    let db = open_project_session_db(&project).await.unwrap();
    assert_provider_ingests(
        "cline",
        ClineLikeSource::cline_with_home(&home),
        &db,
        &project,
    )
    .await;
}

#[tokio::test]
async fn roo_code_task_history_populates_searchable_messages() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_task(
        &vscode_storage_root(&home, "rooveterinaryinc.roo-cline"),
        &project,
        "roo-task",
    );

    let db = open_project_session_db(&project).await.unwrap();
    assert_provider_ingests(
        "roo-code",
        ClineLikeSource::roo_code_with_home(&home),
        &db,
        &project,
    )
    .await;
}

#[tokio::test]
async fn kilo_task_history_populates_searchable_messages() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_task(
        &vscode_storage_root(&home, "kilocode.kilo-code"),
        &project,
        "kilo-task",
    );

    let db = open_project_session_db(&project).await.unwrap();
    assert_provider_ingests(
        "kilo",
        ClineLikeSource::kilo_with_home(&home),
        &db,
        &project,
    )
    .await;
}

#[tokio::test]
async fn cline_ui_messages_only_change_triggers_usage_refresh() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let api = write_task(
        &vscode_storage_root(&home, "saoudrizwan.claude-dev"),
        &project,
        "cline-ui-usage",
    );

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClineLikeSource::cline_with_home(&home);
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

    let ui_path = api.parent().unwrap().join("ui_messages.json");
    std::fs::write(
        &ui_path,
        serde_json::to_string_pretty(&serde_json::json!([
            {
                "type": "say",
                "say": "api_req_started",
                "ts": 1_800_000_005_i64,
                "text": serde_json::json!({
                    "tokensIn": 2200,
                    "tokensOut": 450,
                    "cacheReads": 9000,
                    "cacheWrites": 600
                }).to_string()
            }
        ]))
        .unwrap(),
    )
    .unwrap();

    assert_eq!(
        ingest_source(&db, &source, &project, None)
            .await
            .messages_upserted,
        2
    );
    let results = db
        .search_session_messages(
            "cline",
            Some(project.to_string_lossy().as_ref()),
            "billing",
            10,
        )
        .await;
    let assistant = results
        .iter()
        .find(|hit| hit.message.tool_names.as_deref() == Some("read_file"))
        .expect("assistant message");
    let metadata: serde_json::Value =
        serde_json::from_str(assistant.message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["usage"]["input_tokens"], 2200);
}

#[tokio::test]
async fn cline_usage_index_skips_unemitted_assistant_entries() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let root = vscode_storage_root(&home, "saoudrizwan.claude-dev");
    let dir = root.join("cline-skipped-assistant");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("task_metadata.json"),
        serde_json::json!({
            "task": "Usage indexing",
            "workspacePath": project
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.join("api_conversation_history.json"),
        serde_json::json!([
            {"role": "assistant", "content": ""},
            {"role": "assistant", "content": "Emitted assistant usage target"}
        ])
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.join("ui_messages.json"),
        serde_json::json!([
            {
                "type": "say",
                "say": "api_req_started",
                "text": serde_json::json!({"tokensIn": 777}).to_string()
            }
        ])
        .to_string(),
    )
    .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClineLikeSource::cline_with_home(&home);
    assert_eq!(
        ingest_source(&db, &source, &project, None)
            .await
            .messages_upserted,
        1
    );
    let hits = db
        .search_session_messages("cline", None, "usage target", 10)
        .await;
    assert_eq!(hits.len(), 1);
    let metadata: serde_json::Value =
        serde_json::from_str(hits[0].message.metadata_json.as_deref().unwrap()).unwrap();
    assert_eq!(metadata["usage"]["input_tokens"], 777);
}

#[tokio::test]
async fn cline_parse_failures_advance_content_hash_cursor() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let root = vscode_storage_root(&home, "saoudrizwan.claude-dev");
    let dir = root.join("cline-invalid-json");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("task_metadata.json"),
        serde_json::json!({"workspacePath": project}).to_string(),
    )
    .unwrap();
    let api = dir.join("api_conversation_history.json");
    std::fs::write(&api, "{not json").unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClineLikeSource::cline_with_home(&home);
    let stats = ingest_source(&db, &source, &project, None).await;
    assert_eq!(stats.messages_upserted, 0);

    let offset = parse_offset_for_task_history(&db, &project, &api)
        .await
        .expect("invalid changed task history should still advance its cursor");
    assert_ne!(offset.byte_offset, 0);
}

#[tokio::test]
async fn cline_missing_metadata_waits_for_later_metadata_before_advancing_cursor() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let root = vscode_storage_root(&home, "saoudrizwan.claude-dev");
    let dir = root.join("cline-missing-metadata");
    std::fs::create_dir_all(&dir).unwrap();
    let api = dir.join("api_conversation_history.json");
    std::fs::write(
        &api,
        serde_json::json!([
            {"role": "user", "content": "Metadata missing prompt"}
        ])
        .to_string(),
    )
    .unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClineLikeSource::cline_with_home(&home);
    let stats = ingest_source(&db, &source, &project, None).await;
    assert_eq!(stats.messages_upserted, 0);

    assert!(
        parse_offset_for_task_history(&db, &project, &api)
            .await
            .is_none(),
        "metadata-less task should not advance its cursor"
    );

    std::fs::write(
        dir.join("task_metadata.json"),
        serde_json::json!({
            "task": "Metadata arrived later",
            "workspacePath": project
        })
        .to_string(),
    )
    .unwrap();

    let stats = ingest_source(&db, &source, &project, None).await;
    assert_eq!(stats.messages_upserted, 1);

    let offset = parse_offset_for_task_history(&db, &project, &api)
        .await
        .expect("task should advance once metadata is available");
    assert_ne!(offset.byte_offset, 0);
}

#[tokio::test]
async fn cline_like_task_for_other_project_is_skipped() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let other = tmp.path().join("other-project");
    std::fs::create_dir_all(&other).unwrap();
    write_task(
        &vscode_storage_root(&home, "saoudrizwan.claude-dev"),
        &other,
        "other-task",
    );

    let db = open_project_session_db(&project).await.unwrap();
    let stats = ingest_source(
        &db,
        &ClineLikeSource::cline_with_home(&home),
        &project,
        None,
    )
    .await;
    assert_eq!(stats.messages_upserted, 0);
}
