use std::io::Write;

use tempfile::TempDir;
use tracedecay::sessions::claude::ClaudeSource;
use tracedecay::sessions::cursor::open_project_session_db;
use tracedecay::sessions::source::ingest_source;

use crate::support::{assert_metadata_path_eq, init_git_repo, init_project_at, setup};

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
                "usage": {
                    "input_tokens": 1200,
                    "output_tokens": 340,
                    "cache_creation_input_tokens": 500,
                    "cache_read_input_tokens": 8000,
                    "service_tier": "standard"
                },
                "content": [
                    {"type": "text", "text": "The billing pipeline regression is fixed."},
                    {"type": "tool_use", "name": "tracedecay_context", "input": {}}
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
    init_git_repo(&project);
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
        .any(|hit| hit.message.tool_names.as_deref() == Some("tracedecay_context")));
    assert!(results
        .iter()
        .any(|hit| hit.message.model.as_deref() == Some("claude-opus-4-8")));
    // The structured ISO-8601 timestamps land as epoch seconds (2026-01-01).
    assert!(results
        .iter()
        .any(|hit| hit.message.timestamp == Some(1_767_225_600)));
    assert!(results
        .iter()
        .any(|hit| hit.message.timestamp == Some(1_767_225_605)));

    // Anthropic-style `message.usage` counters land in metadata under the
    // keys the savings dashboard reads; non-counter fields are dropped.
    let assistant = results
        .iter()
        .find(|hit| hit.message.role == "assistant")
        .expect("assistant message should be searchable");
    let metadata: serde_json::Value =
        serde_json::from_str(assistant.message.metadata_json.as_deref().unwrap()).unwrap();
    assert_metadata_path_eq(&metadata["claude_message_cwd"], &project);
    assert_metadata_path_eq(&metadata["claude_message_worktree"], &project);
    assert_eq!(
        metadata["claude_message_location_provenance"].as_str(),
        Some("transcript_record")
    );
    assert!(metadata.get("claude_git_branch").is_none());
    assert_eq!(metadata["usage"]["input_tokens"], 1200);
    assert_eq!(metadata["usage"]["output_tokens"], 340);
    assert_eq!(metadata["usage"]["cache_creation_input_tokens"], 500);
    assert_eq!(metadata["usage"]["cache_read_input_tokens"], 8000);
    assert!(metadata["usage"].get("service_tier").is_none());
    let user = results
        .iter()
        .find(|hit| hit.message.role == "user")
        .expect("user message should be searchable");
    let user_metadata: serde_json::Value =
        serde_json::from_str(user.message.metadata_json.as_deref().unwrap()).unwrap();
    assert_metadata_path_eq(&user_metadata["claude_message_cwd"], &project);
    assert_metadata_path_eq(&user_metadata["claude_message_worktree"], &project);
    assert_eq!(
        user_metadata["claude_message_location_provenance"].as_str(),
        Some("transcript_record")
    );
    assert!(user_metadata.get("usage").is_none());
    let session_metadata: serde_json::Value =
        serde_json::from_str(results[0].session.metadata_json.as_deref().unwrap()).unwrap();
    assert_metadata_path_eq(&session_metadata["claude_session_cwd"], &project);
    assert_metadata_path_eq(&session_metadata["claude_session_worktree"], &project);
    assert_eq!(
        session_metadata["claude_session_location_provenance"].as_str(),
        Some("transcript_session")
    );

    let expected_content = serde_json::json!([
        {"type": "text", "text": "The billing pipeline regression is fixed."},
        {"type": "tool_use", "name": "tracedecay_context", "input": {}}
    ]);
    let raw = db
        .lcm_load_raw_message("claude", "msg_claude_1")
        .await
        .expect("structured Claude content should be in raw LCM storage");
    assert_eq!(
        raw.content,
        serde_json::to_string(&expected_content).unwrap()
    );
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
    let path = write_claude_transcript(&home, &other, "claude-other");

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);

    let stats = ingest_source(&db, &source, &project, None).await;
    assert_eq!(
        stats.messages_upserted, 0,
        "a transcript whose cwd is a different project must be skipped"
    );

    // The cursor must still advance past the filtered-out content, or every
    // future sweep re-reads and re-filters the whole foreign transcript.
    let file_size = std::fs::metadata(&path).unwrap().len();
    let path_str = path.to_string_lossy();
    let mut offset = db.get_parse_offset(path_str.as_ref()).await;
    if offset.is_none() && cfg!(windows) {
        // The scanner stores native separators; the helper built this path
        // with embedded forward slashes.
        offset = db.get_parse_offset(&path_str.replace('/', "\\")).await;
    }
    let offset = offset.expect("skipped foreign transcript should persist a parse offset");
    assert_eq!(
        offset.byte_offset, file_size,
        "parse cursor should sit at EOF for a fully filtered transcript"
    );
}

#[tokio::test]
async fn claude_transcript_crossing_worktrees_is_split_by_record_cwd() {
    let tmp = TempDir::new().unwrap();
    let (home, project_a) = setup(&tmp);
    init_git_repo(&project_a);
    let project_b = tmp.path().join("project-b");
    init_project_at(&project_b);
    init_git_repo(&project_b);

    let dir = home.join(".claude/projects/-mixed-worktree");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mixed-worktree-session.jsonl");
    std::fs::write(
        &path,
        format!(
            "{}\n{}\n",
            serde_json::json!({
                "type": "user",
                "cwd": project_a.to_string_lossy(),
                "sessionId": "mixed-worktree-session",
                "uuid": "mixed-a",
                "timestamp": "2026-01-01T00:00:00.000Z",
                "message": {"role": "user", "content": "alpha worktree marker"}
            }),
            serde_json::json!({
                "type": "user",
                "cwd": project_b.to_string_lossy(),
                "sessionId": "mixed-worktree-session",
                "uuid": "mixed-b",
                "timestamp": "2026-01-01T00:00:05.000Z",
                "message": {"role": "user", "content": "beta worktree marker"}
            })
        ),
    )
    .unwrap();

    let source = ClaudeSource::with_home(&home);
    let db_a = open_project_session_db(&project_a).await.unwrap();
    let stats_a = ingest_source(&db_a, &source, &project_a, None).await;
    assert_eq!(stats_a.messages_upserted, 1);
    let hits_a = db_a
        .search_session_messages("claude", None, "worktree marker", 10)
        .await;
    assert_eq!(hits_a.len(), 1);
    assert!(hits_a[0].message.text.contains("alpha worktree marker"));
    let metadata_a: serde_json::Value =
        serde_json::from_str(hits_a[0].message.metadata_json.as_deref().unwrap()).unwrap();
    assert_metadata_path_eq(&metadata_a["claude_message_cwd"], &project_a);
    assert_metadata_path_eq(&metadata_a["claude_message_worktree"], &project_a);
    assert_eq!(
        metadata_a["claude_message_location_provenance"].as_str(),
        Some("transcript_record")
    );

    let db_b = open_project_session_db(&project_b).await.unwrap();
    let stats_b = ingest_source(&db_b, &source, &project_b, None).await;
    assert_eq!(stats_b.messages_upserted, 1);
    let hits_b = db_b
        .search_session_messages("claude", None, "worktree marker", 10)
        .await;
    assert_eq!(hits_b.len(), 1);
    assert!(hits_b[0].message.text.contains("beta worktree marker"));
    let metadata_b: serde_json::Value =
        serde_json::from_str(hits_b[0].message.metadata_json.as_deref().unwrap()).unwrap();
    assert_metadata_path_eq(&metadata_b["claude_message_cwd"], &project_b);
    assert_metadata_path_eq(&metadata_b["claude_message_worktree"], &project_b);
    assert_eq!(
        metadata_b["claude_message_location_provenance"].as_str(),
        Some("transcript_record")
    );
}

/// The real machine has `~/.claude` but no `projects/` dir (no Claude Code
/// sessions); the scan must be a silent no-op, not an error.
#[tokio::test]
async fn claude_missing_projects_dir_is_silent_noop() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = setup(&tmp);
    // `~/.claude` exists but holds no `projects/` subdir, like a machine
    // where Claude Code never ran (only backups or settings live there).
    std::fs::create_dir_all(home.join(".claude/backups")).unwrap();

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);

    let stats = ingest_source(&db, &source, &project, None).await;
    assert_eq!(stats.sessions_upserted, 0);
    assert_eq!(stats.messages_upserted, 0);
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
