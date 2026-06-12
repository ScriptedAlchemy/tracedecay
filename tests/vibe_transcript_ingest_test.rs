use std::io::Write;

use tempfile::TempDir;
use tokensave::sessions::cursor::open_project_session_db;
use tokensave::sessions::source::ingest_source;
use tokensave::sessions::vibe::VibeSource;

fn setup(tmp: &TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir(project.join(".tokensave")).unwrap();
    std::fs::write(project.join(".tokensave/tokensave.db"), "").unwrap();
    (home, project)
}

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
    write_vibe_session(&home, &project, "vibe-sess");

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
    assert!(results
        .iter()
        .any(|hit| hit.message.tool_names.as_deref() == Some("read_file")));
    assert!(results
        .iter()
        .all(|hit| hit.message.model.as_deref() == Some("mistral-medium-3.5")));

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
