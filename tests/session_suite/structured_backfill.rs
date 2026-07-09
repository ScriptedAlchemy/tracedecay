//! Insert-capable structured-row backfill coverage.

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
    let _ = conn
        .execute(
            "DELETE FROM session_backfill_meta WHERE key LIKE 'structured_backfill_cursor%'",
            (),
        )
        .await;
}

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
    // `open_at` schedules the sweep on a detached background task; drive it
    // synchronously here so the assertions observe a deterministic store.
    tracedecay::global_db::set_background_structured_backfill_enabled(false);
    let tmp = TempDir::new().unwrap();
    let (home, project) = init_project(&tmp);
    write_codex_rollout_with_goal(&home, &project, "codex-backfill");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    ingest_source(&db, &source, &project, None).await;
    // Drive one sweep so the backfill meta table exists (production creates it
    // via the detached sweep `open_at` schedules); the goal row from live
    // ingest is already present, so this inserts nothing.
    db.run_structured_backfill().await;
    assert_eq!(count_kind(&project, "codex", "goal").await, 1);
    drop(db);

    simulate_old_parser_store(&project, "codex", "goal").await;
    assert_eq!(count_kind(&project, "codex", "goal").await, 0);

    let db = open_project_session_db(&project).await.unwrap();
    db.run_structured_backfill().await;
    assert_eq!(count_kind(&project, "codex", "goal").await, 1);
    let goal = load_only_goal_row(&project).await;
    assert_eq!(goal.0, "ship the ingestion backfill", "goal text");
    assert_eq!(goal.1.as_deref(), Some("goal"), "goal kind");
    assert!(
        goal.2.is_some_and(|meta| meta.contains("objective")),
        "goal metadata should round-trip through the parser"
    );
    drop(db);

    // A second sweep finds no candidates past the watermark and marks the
    // whole history complete.
    let db = open_project_session_db(&project).await.unwrap();
    db.run_structured_backfill().await;
    assert_eq!(count_kind(&project, "codex", "goal").await, 1);
    drop(db);
    assert_eq!(structured_marker_version(&project).await, Some(2));
}

#[tokio::test]
async fn structured_backfill_preserves_existing_rows() {
    tracedecay::global_db::set_background_structured_backfill_enabled(false);
    let tmp = TempDir::new().unwrap();
    let (home, project) = init_project(&tmp);
    write_codex_rollout_with_goal(&home, &project, "codex-preserve");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    ingest_source(&db, &source, &project, None).await;
    // Create the backfill meta table up front (see the note in the goal test).
    db.run_structured_backfill().await;

    let before = db.get_session("codex", "codex-preserve").await.unwrap();
    drop(db);

    simulate_old_parser_store(&project, "codex", "goal").await;
    let db = open_project_session_db(&project).await.unwrap();
    db.run_structured_backfill().await;
    assert_eq!(count_kind(&project, "codex", "goal").await, 1);

    let after = db.get_session("codex", "codex-preserve").await.unwrap();
    assert_eq!(before.started_at, after.started_at);
    assert_eq!(before.ended_at, after.ended_at);
    assert_eq!(before.title, after.title);
}

#[tokio::test]
async fn structured_backfill_inserts_claude_marker_rows_once() {
    tracedecay::global_db::set_background_structured_backfill_enabled(false);
    let tmp = TempDir::new().unwrap();
    let (home, project) = init_project(&tmp);
    write_claude_transcript_with_pr_link(&home, &project, "claude-backfill");

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);
    ingest_source(&db, &source, &project, None).await;
    // Create the backfill meta table up front (see the note in the goal test).
    db.run_structured_backfill().await;
    assert_eq!(count_kind(&project, "claude", "pr_link").await, 1);
    let assistant_before = load_row_by_role(&project, "claude", "assistant").await;
    drop(db);

    simulate_old_parser_store(&project, "claude", "pr_link").await;
    assert_eq!(count_kind(&project, "claude", "pr_link").await, 0);

    let db = open_project_session_db(&project).await.unwrap();
    db.run_structured_backfill().await;
    assert_eq!(count_kind(&project, "claude", "pr_link").await, 1);
    let assistant_after = load_row_by_role(&project, "claude", "assistant").await;
    assert_eq!(assistant_before, assistant_after);
    drop(db);

    let db = open_project_session_db(&project).await.unwrap();
    db.run_structured_backfill().await;
    assert_eq!(count_kind(&project, "claude", "pr_link").await, 1);
    drop(db);
    assert_eq!(structured_marker_version(&project).await, Some(2));
}

/// Regression for the stale-cursor-vs-version-bump defect: the sweep's path
/// watermark is namespaced by marker version, so re-entering the sweep (as a
/// version bump does by resetting the marker) reads a *fresh* cursor and
/// re-parses from the start. A leftover, un-versioned cursor parked at the last
/// path — the shape a pre-namespacing build wrote — must be ignored instead of
/// zeroing out the candidate set.
#[tokio::test]
async fn structured_backfill_version_bump_reparses_from_start() {
    tracedecay::global_db::set_background_structured_backfill_enabled(false);
    let tmp = TempDir::new().unwrap();
    let (home, project) = init_project(&tmp);
    write_codex_rollout_with_goal(&home, &project, "codex-versionbump");

    // Live ingest, then run the sweep to completion for the current version.
    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    ingest_source(&db, &source, &project, None).await;
    db.run_structured_backfill().await; // parses the file, advances the cursor
    db.run_structured_backfill().await; // no candidates: marks complete, clears cursors
    assert_eq!(structured_marker_version(&project).await, Some(2));
    drop(db);

    // Drop the structured rows and reset the marker so the sweep re-enters
    // (exactly what a `STRUCTURED_MARKER_VERSION` bump does). Then plant a
    // stale, *un-versioned* watermark parked at the last transcript path.
    let conn = raw_conn(&project).await;
    conn.execute(
        "DELETE FROM lcm_raw_messages
         WHERE provider = 'codex'
           AND message_id IN (
               SELECT message_id FROM session_messages
               WHERE provider = 'codex' AND kind = 'goal')",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "DELETE FROM session_messages WHERE provider = 'codex' AND kind = 'goal'",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "DELETE FROM session_schema_migrations WHERE name = 'structured_rows_backfill'",
        (),
    )
    .await
    .unwrap();
    let mut rows = conn
        .query(
            "SELECT MAX(source_path) FROM session_messages WHERE provider = 'codex'",
            (),
        )
        .await
        .unwrap();
    let last_path = rows
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap();
    conn.execute(
        "INSERT INTO session_backfill_meta(key, value) VALUES ('structured_backfill_cursor', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        libsql::params![last_path],
    )
    .await
    .unwrap();
    assert_eq!(count_kind(&project, "codex", "goal").await, 0);

    // The version-namespaced cursor key has never been written, so the sweep
    // starts from the beginning and re-parses the whole history — the stale
    // un-versioned cursor parked at the last path is ignored. (A regression to
    // an un-versioned key would resume past `last_path`, see zero candidates,
    // and leave the goal row missing.)
    let db = open_project_session_db(&project).await.unwrap();
    db.run_structured_backfill().await;
    assert_eq!(
        count_kind(&project, "codex", "goal").await,
        1,
        "a stale un-versioned cursor must not block a fresh version-bumped sweep"
    );
}
