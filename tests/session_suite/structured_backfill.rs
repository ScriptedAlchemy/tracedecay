//! Coverage for the insert-capable structured-row backfill: transcripts
//! ingested before the current parsers shipped are missing the newly-added row
//! kinds (Codex `goal`/telemetry rows, Claude `pr_link`/marker rows) because
//! append-only ingest never revisits bytes past its offset. The backfill
//! re-parses each already-ingested Claude/Codex transcript from byte 0 through
//! the current parser and inserts only the rows whose `(provider, message_id)`
//! key is absent — existing rows keep their content, and a second run inserts
//! nothing new.

use tempfile::TempDir;
use tracedecay::sessions::claude::ClaudeSource;
use tracedecay::sessions::codex::CodexSource;
use tracedecay::sessions::cursor::{open_project_session_db, project_session_db_path};
use tracedecay::sessions::source::ingest_source;

fn init_project(tmp: &TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(project.join(".tracedecay")).unwrap();
    std::fs::write(project.join(".tracedecay/tracedecay.db"), "").unwrap();
    (home, project)
}

/// Raw connection to the project session DB for the low-level surgery these
/// tests need (deleting rows to simulate an old-parser store, resetting the
/// backfill marker/watermark, counting rows by kind).
async fn raw_conn(project: &std::path::Path) -> libsql::Connection {
    let raw = libsql::Builder::new_local(project_session_db_path(project))
        .build()
        .await
        .unwrap();
    raw.connect().unwrap()
}

async fn count_kind(project: &std::path::Path, provider: &str, kind: &str) -> i64 {
    let conn = raw_conn(project).await;
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM session_messages WHERE provider = ?1 AND kind = ?2",
            libsql::params![provider, kind],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

/// Deletes every row of `kind` from both the searchable projection and the raw
/// store (simulating an ingest by a parser that never emitted that kind) and
/// clears the structured-backfill marker + watermark so the next store open
/// re-runs the sweep against the damaged store.
async fn simulate_old_parser_store(project: &std::path::Path, provider: &str, kind: &str) {
    let conn = raw_conn(project).await;
    conn.execute(
        "DELETE FROM lcm_raw_messages
         WHERE provider = ?1
           AND message_id IN (
               SELECT message_id FROM session_messages
               WHERE provider = ?1 AND kind = ?2)",
        libsql::params![provider, kind],
    )
    .await
    .unwrap();
    conn.execute(
        "DELETE FROM session_messages WHERE provider = ?1 AND kind = ?2",
        libsql::params![provider, kind],
    )
    .await
    .unwrap();
    conn.execute(
        "DELETE FROM session_schema_migrations WHERE name = 'structured_rows_backfill'",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "DELETE FROM session_backfill_meta WHERE key = 'structured_backfill_cursor'",
        (),
    )
    .await
    .unwrap();
}

/// Returns `(text, kind, metadata_json)` for the single codex `goal` row.
async fn load_only_goal_row(project: &std::path::Path) -> (String, Option<String>, Option<String>) {
    let conn = raw_conn(project).await;
    let mut rows = conn
        .query(
            "SELECT text, kind, metadata_json FROM session_messages
             WHERE provider = 'codex' AND kind = 'goal'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().expect("one goal row");
    (
        row.get::<String>(0).unwrap(),
        row.get::<Option<String>>(1).unwrap(),
        row.get::<Option<String>>(2).unwrap(),
    )
}

/// Returns `(timestamp, text, metadata_json)` for the single row with `role`.
async fn load_row_by_role(
    project: &std::path::Path,
    provider: &str,
    role: &str,
) -> (Option<i64>, String, Option<String>) {
    let conn = raw_conn(project).await;
    let mut rows = conn
        .query(
            "SELECT timestamp, text, metadata_json FROM session_messages
             WHERE provider = ?1 AND role = ?2",
            libsql::params![provider, role],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().expect("one row for role");
    (
        row.get::<Option<i64>>(0).unwrap(),
        row.get::<String>(1).unwrap(),
        row.get::<Option<String>>(2).unwrap(),
    )
}

async fn structured_marker_version(project: &std::path::Path) -> Option<i64> {
    let conn = raw_conn(project).await;
    let mut rows = conn
        .query(
            "SELECT version FROM session_schema_migrations WHERE name = 'structured_rows_backfill'",
            (),
        )
        .await
        .unwrap();
    rows.next().await.unwrap().and_then(|row| row.get(0).ok())
}

fn write_codex_rollout_with_goal(
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
            "payload": {"type": "user_message", "message": "Ship the ingestion backfill"}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:02.000Z",
            "type": "event_msg",
            "payload": {"type": "agent_message", "message": "On it."}
        }),
        serde_json::json!({
            "timestamp": "2026-01-01T00:00:03.000Z",
            "type": "event_msg",
            "payload": {
                "type": "thread_goal_updated",
                "threadId": "thread-1",
                "goal": {
                    "threadId": "thread-1",
                    "objective": "ship the ingestion backfill",
                    "status": "active",
                    "tokensUsed": 42,
                    "timeUsedSeconds": 7,
                    "createdAt": 1_783_500_569i64,
                    "updatedAt": 1_783_500_600i64
                }
            }
        }),
    );
    std::fs::write(&path, contents).unwrap();
    path
}

fn write_claude_transcript_with_pr_link(
    home: &std::path::Path,
    project: &std::path::Path,
    session: &str,
) -> std::path::PathBuf {
    let dir = home.join(".claude/projects/-backfill-slug");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{session}.jsonl"));
    let cwd = project.to_string_lossy();
    let contents = format!(
        "{}\n{}\n{}\n",
        serde_json::json!({
            "type": "user",
            "cwd": cwd,
            "sessionId": session,
            "uuid": "u1",
            "timestamp": "2026-01-01T00:00:00.000Z",
            "message": {"role": "user", "content": "Open the PR"}
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
                "content": [{"type": "text", "text": "Opened it."}]
            }
        }),
        serde_json::json!({
            "type": "pr-link",
            "cwd": cwd,
            "sessionId": session,
            "uuid": "pr-1",
            "timestamp": "2026-01-01T00:00:06.000Z",
            "prNumber": 321,
            "prUrl": "https://github.com/ScriptedAlchemy/tracedecay/pull/321",
            "prRepository": "ScriptedAlchemy/tracedecay"
        }),
    );
    std::fs::write(&path, contents).unwrap();
    path
}

#[tokio::test]
async fn structured_backfill_inserts_codex_goal_rows_once() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = init_project(&tmp);
    write_codex_rollout_with_goal(&home, &project, "codex-backfill");

    // Live ingest through the current parser writes the goal row.
    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    ingest_source(&db, &source, &project, None).await;
    assert_eq!(count_kind(&project, "codex", "goal").await, 1);
    drop(db);

    // Simulate a store ingested before the goal parser existed: drop the goal
    // rows and clear the marker so the next open re-runs the sweep.
    simulate_old_parser_store(&project, "codex", "goal").await;
    assert_eq!(count_kind(&project, "codex", "goal").await, 0);

    // First re-open runs the backfill and re-inserts exactly one goal row,
    // re-derived by the real parser (text/metadata intact).
    let db = open_project_session_db(&project).await.unwrap();
    assert_eq!(count_kind(&project, "codex", "goal").await, 1);
    let goal = load_only_goal_row(&project).await;
    assert_eq!(goal.0, "ship the ingestion backfill", "goal text");
    assert_eq!(goal.1.as_deref(), Some("goal"), "goal kind");
    assert!(
        goal.2.is_some_and(|meta| meta.contains("objective")),
        "goal metadata should round-trip through the parser"
    );
    drop(db);

    // Second open: nothing new to insert, and the sweep marks itself complete.
    let db = open_project_session_db(&project).await.unwrap();
    assert_eq!(count_kind(&project, "codex", "goal").await, 1);
    drop(db);
    assert_eq!(structured_marker_version(&project).await, Some(1));
}

#[tokio::test]
async fn structured_backfill_preserves_existing_rows() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = init_project(&tmp);
    write_codex_rollout_with_goal(&home, &project, "codex-preserve");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    ingest_source(&db, &source, &project, None).await;

    // Snapshot the assistant conversational row before the backfill.
    let before = db.get_session("codex", "codex-preserve").await.unwrap();
    drop(db);

    simulate_old_parser_store(&project, "codex", "goal").await;
    let db = open_project_session_db(&project).await.unwrap();
    assert_eq!(count_kind(&project, "codex", "goal").await, 1);

    // The session window and existing conversational rows are unchanged.
    let after = db.get_session("codex", "codex-preserve").await.unwrap();
    assert_eq!(before.started_at, after.started_at);
    assert_eq!(before.ended_at, after.ended_at);
    assert_eq!(before.title, after.title);
}

#[tokio::test]
async fn structured_backfill_inserts_claude_marker_rows_once() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = init_project(&tmp);
    write_claude_transcript_with_pr_link(&home, &project, "claude-backfill");

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);
    ingest_source(&db, &source, &project, None).await;
    assert_eq!(count_kind(&project, "claude", "pr_link").await, 1);
    let assistant_before = load_row_by_role(&project, "claude", "assistant").await;
    drop(db);

    simulate_old_parser_store(&project, "claude", "pr_link").await;
    assert_eq!(count_kind(&project, "claude", "pr_link").await, 0);

    // Re-open: backfill re-inserts the marker row exactly once...
    let db = open_project_session_db(&project).await.unwrap();
    assert_eq!(count_kind(&project, "claude", "pr_link").await, 1);
    // ...without disturbing the surviving conversational row.
    let assistant_after = load_row_by_role(&project, "claude", "assistant").await;
    assert_eq!(assistant_before, assistant_after);
    drop(db);

    // Running the sweep again is a no-op.
    let db = open_project_session_db(&project).await.unwrap();
    assert_eq!(count_kind(&project, "claude", "pr_link").await, 1);
    drop(db);
    assert_eq!(structured_marker_version(&project).await, Some(1));
}
