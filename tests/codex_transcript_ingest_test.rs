use std::io::Write;

use tempfile::TempDir;
use tokensave::sessions::codex::CodexSource;
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

/// Writes a Codex rollout JSONL whose `session_meta.cwd` is `project`. Includes a
/// `response_item` line that must be ignored (it duplicates the agent_message).
fn write_codex_rollout(
    home: &std::path::Path,
    project: &std::path::Path,
    session: &str,
) -> std::path::PathBuf {
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("rollout-2026-01-01T00-00-00-{session}.jsonl"));
    let contents = format!(
        "{}\n{}\n{}\n{}\n",
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:00.000Z",
            "type": "session_meta",
            "payload": {"id": session, "cwd": project.to_string_lossy(), "model": "gpt-5.5"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "event_msg",
            "payload": {"type": "user_message", "message": "Investigate the billing pipeline regression"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:02.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "The billing pipeline regression is fixed."}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:02.500Z",
            "type": "response_item",
            "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "duplicate"}]}
        }),
    );
    std::fs::write(&path, contents).unwrap();
    path
}

fn write_codex_subagent_rollout(
    home: &std::path::Path,
    project: &std::path::Path,
    parent_session: &str,
    child_session: &str,
) -> std::path::PathBuf {
    let dir = home.join(".codex/sessions/2026/01/01");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("rollout-2026-01-01T00-00-10-{child_session}.jsonl"));
    let contents = format!(
        "{}\n{}\n",
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:10.000Z",
            "type": "session_meta",
            "payload": {
                "id": child_session,
                "cwd": project.to_string_lossy(),
                "model_provider": "openai",
                "thread_source": "subagent",
                "forked_from_id": parent_session,
                "agent_nickname": "Euler",
                "agent_role": "explorer",
                "source": {
                    "subagent": {
                        "thread_spawn": {
                            "parent_thread_id": parent_session,
                            "agent_nickname": "Euler",
                            "agent_role": "explorer",
                            "depth": 1
                        }
                    }
                }
            }
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:11.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "The child worker verified Codex layout evidence."}
        }),
    );
    std::fs::write(&path, contents).unwrap();
    path
}

#[tokio::test]
async fn codex_rollout_populates_user_and_agent_messages_only() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_codex_rollout(&home, &project, "codex-sess");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);

    let stats = ingest_source(&db, &source, &project, None).await;
    // user_message + agent_message; the response_item duplicate is skipped.
    assert_eq!(stats.messages_upserted, 2);
    assert_eq!(stats.sessions_upserted, 1);

    let results = db
        .search_session_messages(
            "codex",
            Some(project.to_string_lossy().as_ref()),
            "billing pipeline",
            10,
        )
        .await;
    assert_eq!(results.len(), 2);
    assert!(results.iter().any(|hit| hit.message.role == "user"));
    assert!(results.iter().any(|hit| hit.message.role == "assistant"));
    assert!(results
        .iter()
        .all(|hit| hit.message.model.as_deref() == Some("gpt-5.5")));
}

#[tokio::test]
async fn codex_rollout_ingest_is_incremental() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let path = write_codex_rollout(&home, &project, "codex-sess");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);

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

    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(
        f,
        "{}",
        serde_json::json!({
            "timestamp": "2026-01-01T00:01:00.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "Added a regression test."}
        })
    )
    .unwrap();
    drop(f);

    assert_eq!(
        ingest_source(&db, &source, &project, None)
            .await
            .messages_upserted,
        1
    );
}

#[tokio::test]
async fn codex_subagent_rollout_uses_parent_link_from_session_meta() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_codex_rollout(&home, &project, "codex-parent");
    write_codex_subagent_rollout(&home, &project, "codex-parent", "codex-child");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);

    let stats = ingest_source(&db, &source, &project, None).await;
    assert_eq!(stats.sessions_upserted, 2);
    assert_eq!(stats.messages_upserted, 3);

    let child = db
        .get_session("codex", "codex-child")
        .await
        .expect("subagent session should be stored");
    assert_eq!(child.parent_session_id.as_deref(), Some("codex-parent"));
    assert!(child.is_subagent);
    assert_eq!(child.agent_id.as_deref(), Some("codex-child"));

    let results = db
        .search_session_messages("codex", None, "layout evidence", 10)
        .await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].session.session_id, "codex-child");
}
