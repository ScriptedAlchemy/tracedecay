use std::io::Write;

use tempfile::TempDir;
use tokensave::sessions::claude::ClaudeSource;
use tokensave::sessions::cursor::open_project_session_db;
use tokensave::sessions::source::ingest_source;

/// Builds an initialized project dir and returns (home, project_root).
fn setup(tmp: &TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir(project.join(".tokensave")).unwrap();
    std::fs::write(project.join(".tokensave/tokensave.db"), "").unwrap();
    (home, project)
}

/// Writes a Claude Code transcript (one JSON object per line) for `session` whose
/// recorded `cwd` is `project`.
fn write_claude_transcript(
    home: &std::path::Path,
    project: &std::path::Path,
    session: &str,
) -> std::path::PathBuf {
    let dir = home.join(".claude/projects/-some-slug");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{session}.jsonl"));
    let cwd = project.to_string_lossy();
    let contents = format!(
        "{}\n{}\n",
        serde_json::json!({
            "type": "user",
            "cwd": cwd,
            "sessionId": session,
            "uuid": "u1",
            "timestamp": "2026-01-01T00:00:00.000Z",
            "message": {"role": "user", "content": "Investigate the billing pipeline regression"}
        }),
        serde_json::json!({
            "type": "assistant",
            "cwd": cwd,
            "sessionId": session,
            "uuid": "u2",
            "timestamp": "2026-01-01T00:00:05.000Z",
            "message": {
                "id": "msg_claude_1",
                "role": "assistant",
                "model": "claude-opus-4-8",
                "content": [
                    {"type": "text", "text": "The billing pipeline regression is fixed."},
                    {"type": "tool_use", "name": "tokensave_context", "input": {}}
                ]
            }
        }),
    );
    std::fs::write(&path, contents).unwrap();
    path
}

fn write_claude_subagent_transcript(
    home: &std::path::Path,
    parent_session: &str,
    agent_id: &str,
) -> std::path::PathBuf {
    let dir = home
        .join(".claude/projects/-some-slug")
        .join(parent_session)
        .join("subagents");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("agent-{agent_id}.jsonl"));
    std::fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::json!({
                "type": "assistant",
                "sessionId": format!("agent-{agent_id}"),
                "uuid": "child-u1",
                "timestamp": "2026-01-01T00:00:10.000Z",
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "The child worker verified billing fallback evidence."}
                    ]
                }
            })
        ),
    )
    .unwrap();
    path
}

#[tokio::test]
async fn claude_transcript_populates_searchable_messages() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_claude_transcript(&home, &project, "claude-sess");

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);

    let stats = ingest_source(&db, &source, &project, None).await;
    assert_eq!(stats.messages_upserted, 2);
    assert_eq!(stats.sessions_upserted, 1);

    let results = db
        .search_session_messages(
            "claude",
            Some(project.to_string_lossy().as_ref()),
            "billing pipeline",
            10,
        )
        .await;
    assert_eq!(results.len(), 2);
    assert!(results
        .iter()
        .any(|hit| hit.message.tool_names.as_deref() == Some("tokensave_context")));
    assert!(results
        .iter()
        .any(|hit| hit.message.model.as_deref() == Some("claude-opus-4-8")));
}

#[tokio::test]
async fn claude_transcript_ingest_is_incremental() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let path = write_claude_transcript(&home, &project, "claude-sess");

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);

    let first = ingest_source(&db, &source, &project, None).await;
    assert_eq!(first.messages_upserted, 2);
    // Re-ingesting the unchanged file is a no-op.
    let second = ingest_source(&db, &source, &project, None).await;
    assert_eq!(second.messages_upserted, 0);

    // Appending one line ingests only that line.
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(
        f,
        "{}",
        serde_json::json!({
            "type": "user",
            "cwd": project.to_string_lossy(),
            "sessionId": "claude-sess",
            "uuid": "u3",
            "timestamp": "2026-01-01T00:01:00.000Z",
            "message": {"role": "user", "content": "Add a regression test for billing"}
        })
    )
    .unwrap();
    drop(f);

    let third = ingest_source(&db, &source, &project, None).await;
    assert_eq!(third.messages_upserted, 1);
}

#[tokio::test]
async fn claude_transcript_for_other_project_is_skipped() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    let other = tmp.path().join("other-project");
    std::fs::create_dir_all(&other).unwrap();
    // Transcript records a cwd that is NOT the project we ingest for.
    write_claude_transcript(&home, &other, "claude-other");

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);

    let stats = ingest_source(&db, &source, &project, None).await;
    assert_eq!(
        stats.messages_upserted, 0,
        "a transcript whose cwd is a different project must be skipped"
    );
}

#[tokio::test]
async fn claude_subagent_layout_uses_parent_link_and_parent_cwd_fallback() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    write_claude_transcript(&home, &project, "parent-claude");
    write_claude_subagent_transcript(&home, "parent-claude", "worker");

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);

    let stats = ingest_source(&db, &source, &project, None).await;
    assert_eq!(stats.sessions_upserted, 2);
    assert_eq!(stats.messages_upserted, 3);

    let child = db
        .get_session("claude", "agent-worker")
        .await
        .expect("subagent session should be stored");
    assert_eq!(child.parent_session_id.as_deref(), Some("parent-claude"));
    assert!(child.is_subagent);
    assert_eq!(child.agent_id.as_deref(), Some("worker"));

    let results = db
        .search_session_messages("claude", None, "fallback evidence", 10)
        .await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].session.session_id, "agent-worker");
}
