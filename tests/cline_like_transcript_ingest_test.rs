use tempfile::TempDir;
use tokensave::sessions::cline_like::ClineLikeSource;
use tokensave::sessions::cursor::open_project_session_db;
use tokensave::sessions::source::ingest_source;

fn setup(tmp: &TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir(project.join(".tokensave")).unwrap();
    std::fs::write(project.join(".tokensave/tokensave.db"), "").unwrap();
    (home, project)
}

fn vscode_storage_root(home: &std::path::Path, extension_id: &str) -> std::path::PathBuf {
    tokensave::agents::vscode_data_dir(home)
        .join("User/globalStorage")
        .join(extension_id)
        .join("tasks")
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
    api
}

async fn assert_provider_ingests(
    provider: &str,
    source: ClineLikeSource,
    db: &tokensave::global_db::GlobalDb,
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
