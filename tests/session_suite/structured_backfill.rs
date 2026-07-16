//! Insert-capable structured-row backfill coverage.

use tempfile::TempDir;
use tracedecay::sessions::claude::ClaudeSource;
use tracedecay::sessions::codex::CodexSource;
use tracedecay::sessions::cursor::{open_project_session_db, project_session_db_path};
use tracedecay::sessions::source::try_ingest_source;

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
    // Reset both the retired global marker (older stores) and the per-provider
    // markers so the version-bumped sweep re-enters from the start.
    conn.execute(
        "DELETE FROM session_schema_migrations WHERE name LIKE 'structured_rows_backfill%'",
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

/// Reads a provider's per-provider structured-backfill marker version, or the
/// retired global marker when `provider` is `None`.
async fn structured_marker_version(
    project: &std::path::Path,
    provider: Option<&str>,
) -> Option<i64> {
    let name = match provider {
        Some(provider) => format!("structured_rows_backfill:{provider}"),
        None => "structured_rows_backfill".to_string(),
    };
    let conn = raw_conn(project).await;
    let mut rows = conn
        .query(
            "SELECT version FROM session_schema_migrations WHERE name = ?1",
            libsql::params![name],
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

fn write_claude_transcript_with_thinking(
    home: &std::path::Path,
    project: &std::path::Path,
    session: &str,
) -> std::path::PathBuf {
    let dir = home.join(".claude/projects/-thinking-slug");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{session}.jsonl"));
    let cwd = project.to_string_lossy();
    let contents = format!(
        "{}\n{}\n",
        serde_json::json!({
            "type": "user",
            "cwd": cwd,
            "sessionId": session,
            "uuid": "tu1",
            "timestamp": "2026-01-01T00:00:00.000Z",
            "message": {"role": "user", "content": "Fix the parser"}
        }),
        serde_json::json!({
            "type": "assistant",
            "cwd": cwd,
            "sessionId": session,
            "uuid": "tu2",
            "timestamp": "2026-01-01T00:00:05.000Z",
            "message": {
                "id": "msg_thinking_1",
                "role": "assistant",
                "model": "claude-opus-4-8",
                "content": [
                    {"type": "thinking", "thinking": "Let me trace the ingestion path first."},
                    {"type": "text", "text": "Fixed the parser."}
                ]
            }
        }),
    );
    std::fs::write(&path, contents).unwrap();
    path
}

#[tokio::test]
async fn structured_backfill_never_replays_claude_transcripts() {
    tracedecay::global_db::set_background_structured_backfill_enabled(false);
    let tmp = TempDir::new().unwrap();
    let (home, project) = init_project(&tmp);
    write_claude_transcript_with_thinking(&home, &project, "claude-thinking");

    let db = open_project_session_db(&project).await.unwrap();
    let source = ClaudeSource::with_home(&home);
    try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    // Live ingest emits the reasoning row. The structured backfill only sets
    // up its Codex state and must not claim a Claude marker.
    db.run_structured_backfill().await;
    assert_eq!(count_kind(&project, "claude", "reasoning").await, 1);
    // The user + assistant conversational rows (both kind "message") coexist
    // with the reasoning row.
    assert_eq!(count_kind(&project, "claude", "message").await, 2);
    drop(db);

    // Remove the row and reset legacy backfill state. A second Claude parser
    // would recreate it; the observation pipeline must remain the sole
    // production Claude cursor authority, so the backfill leaves it absent.
    simulate_old_parser_store(&project, "claude", "reasoning").await;
    assert_eq!(count_kind(&project, "claude", "reasoning").await, 0);
    // Dropping reasoning rows leaves the conversational message rows untouched.
    assert_eq!(count_kind(&project, "claude", "message").await, 2);

    let db = open_project_session_db(&project).await.unwrap();
    db.run_structured_backfill().await;
    assert_eq!(count_kind(&project, "claude", "reasoning").await, 0);
    assert_eq!(count_kind(&project, "claude", "message").await, 2);
    drop(db);
    assert_eq!(
        structured_marker_version(&project, Some("claude")).await,
        None
    );
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
    try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
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
    assert_eq!(
        structured_marker_version(&project, Some("codex")).await,
        Some(4)
    );
}

#[tokio::test]
async fn structured_backfill_preserves_existing_rows() {
    tracedecay::global_db::set_background_structured_backfill_enabled(false);
    let tmp = TempDir::new().unwrap();
    let (home, project) = init_project(&tmp);
    write_codex_rollout_with_goal(&home, &project, "codex-preserve");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
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
    try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    db.run_structured_backfill().await; // parses the file, advances the cursor
    db.run_structured_backfill().await; // no candidates: marks complete, clears cursors
    assert_eq!(
        structured_marker_version(&project, Some("codex")).await,
        Some(4)
    );
    drop(db);

    // Drop the structured rows and reset the marker so the sweep re-enters
    // (exactly what bumping codex's entry in `STRUCTURED_BACKFILL_VERSIONS`
    // does). Then plant a stale, *un-versioned* watermark parked at the last
    // transcript path.
    // Scope the raw connection so it is dropped before the final GlobalDb open.
    // Holding it across `run_structured_backfill` contends on Windows (busy_timeout
    // 5s) and can abort the insert with `BEGIN IMMEDIATE` failure.
    {
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
            "DELETE FROM session_schema_migrations WHERE name LIKE 'structured_rows_backfill%'",
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
    }
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

/// Migration: a store carrying the retired global `structured_rows_backfill`
/// marker at version N seeds every provider's marker to N, retires the global
/// marker and its legacy cursor rows, and triggers no spurious re-sweep.
#[tokio::test]
async fn structured_backfill_migrates_legacy_global_marker() {
    tracedecay::global_db::set_background_structured_backfill_enabled(false);
    let tmp = TempDir::new().unwrap();
    let (home, project) = init_project(&tmp);
    write_codex_rollout_with_goal(&home, &project, "codex-migrate");

    let db = open_project_session_db(&project).await.unwrap();
    try_ingest_source(&db, &CodexSource::with_home(&home), &project, None)
        .await
        .unwrap();
    // Ensure the meta table exists (production creates it via the sweep).
    db.run_structured_backfill().await;
    drop(db);

    // Rewrite the store into the legacy shape: a single global marker at v3
    // (a store that already finished the global sweep), no per-provider markers,
    // plus stale legacy cursor rows (un-versioned and global-versioned).
    // Drop the raw connection before reopening GlobalDb — a held writer blocks
    // `BEGIN IMMEDIATE` on Windows/Linux under busy_timeout and can leave the
    // codex marker stuck at the seeded legacy baseline.
    {
        let conn = raw_conn(&project).await;
        conn.execute(
            "DELETE FROM session_schema_migrations WHERE name LIKE 'structured_rows_backfill%'",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO session_schema_migrations(name, version) VALUES ('structured_rows_backfill', 3)",
            (),
        )
        .await
        .unwrap();
        conn.execute(
            "DELETE FROM session_backfill_meta WHERE key LIKE 'structured_backfill_cursor%'",
            (),
        )
        .await
        .unwrap();
        for key in [
            "structured_backfill_cursor",
            "structured_backfill_cursor:v3",
        ] {
            conn.execute(
                "INSERT INTO session_backfill_meta(key, value) VALUES (?1, 'legacy/path.jsonl')",
                libsql::params![key],
            )
            .await
            .unwrap();
        }
    }
    // Sanity: the legacy global marker is present, per-provider markers are not.
    assert_eq!(structured_marker_version(&project, None).await, Some(3));
    assert_eq!(
        structured_marker_version(&project, Some("claude")).await,
        None
    );
    assert_eq!(
        structured_marker_version(&project, Some("codex")).await,
        None
    );

    // The bounded sweep seeds the tracked Codex provider to N=3 and retires
    // the global marker/cursors. Codex then parses at its v4 custom-exec target;
    // Claude remains outside this legacy cursor authority entirely.
    let db = open_project_session_db(&project).await.unwrap();
    db.run_structured_backfill().await;
    db.run_structured_backfill().await;
    drop(db);

    assert_eq!(
        structured_marker_version(&project, Some("claude")).await,
        None,
        "legacy migration must not create a Claude backfill authority"
    );
    assert_eq!(
        structured_marker_version(&project, Some("codex")).await,
        Some(4),
        "codex marker advances from the legacy baseline to its current target"
    );
    assert_eq!(
        structured_marker_version(&project, None).await,
        None,
        "the retired global marker row is gone"
    );

    // The legacy cursor rows were cleaned; no spurious full re-sweep occurred.
    let conn = raw_conn(&project).await;
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM session_backfill_meta WHERE key LIKE 'structured_backfill_cursor%'",
            (),
        )
        .await
        .unwrap();
    let leftover_cursors: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(
        leftover_cursors, 0,
        "legacy cursor rows are retired on migration"
    );
    assert_eq!(count_kind(&project, "codex", "goal").await, 1);
}

// --- Process-safety coverage (adversarial-review findings on #357) ---

/// A one-shot process (CLI/hook) must never schedule the detached sweep, even
/// with the background switch on: it would drop the sweep mid-parse on exit.
#[tokio::test]
async fn structured_backfill_one_shot_process_never_spawns() {
    let current_exe = std::env::current_exe().expect("resolve the session-suite test binary");
    let output = std::process::Command::new(current_exe)
        .args([
            "--exact",
            "structured_backfill_fresh_child_probe",
            "--ignored",
            "--nocapture",
        ])
        .output()
        .expect("launch the fresh-process child probe");

    assert!(
        output.status.success(),
        "fresh-process child probe failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Two concurrent openers of the same store contend on the sibling lock file:
/// exactly one may sweep at a time; the loser is excluded and reacquires only
/// after the winner releases. Models the cross-process race between short-lived
/// hook processes (advisory `flock` excludes across open file descriptions).
#[tokio::test]
async fn structured_backfill_lock_excludes_concurrent_openers() {
    use tracedecay::sessions::transcript_backfill::try_acquire_structured_backfill_lock;

    let tmp = TempDir::new().unwrap();
    let (_home, project) = init_project(&tmp);
    // Ensure the session store (and its parent dir) exists.
    let _db = open_project_session_db(&project).await.unwrap();
    let db_path = project_session_db_path(&project);

    let winner = try_acquire_structured_backfill_lock(&db_path);
    assert!(winner.is_some(), "first opener acquires the sweep lock");
    let loser = try_acquire_structured_backfill_lock(&db_path);
    assert!(
        loser.is_none(),
        "a concurrent opener must be excluded while the lock is held"
    );

    drop(winner);
    let reacquired = try_acquire_structured_backfill_lock(&db_path);
    assert!(
        reacquired.is_some(),
        "the lock must be reusable once the holder releases it"
    );
}

/// Two sweeps driven concurrently against the same store: the lock lets exactly
/// one do the work (insert the missing row) while the other skips with an empty
/// result — no duplicate whole-file re-parse, no double insert.
#[tokio::test]
async fn structured_backfill_concurrent_sweeps_run_once() {
    tracedecay::global_db::set_background_structured_backfill_enabled(false);
    let tmp = TempDir::new().unwrap();
    let (home, project) = init_project(&tmp);
    write_codex_rollout_with_goal(&home, &project, "codex-concurrent");

    let db = open_project_session_db(&project).await.unwrap();
    let source = CodexSource::with_home(&home);
    try_ingest_source(&db, &source, &project, None)
        .await
        .unwrap();
    db.run_structured_backfill().await;
    drop(db);

    // Drop the one goal row so a sweep has exactly one row to re-insert.
    simulate_old_parser_store(&project, "codex", "goal").await;
    assert_eq!(count_kind(&project, "codex", "goal").await, 0);

    // Two independent openers (separate connections) sweep the same store at
    // once. The cross-process lock admits one; the other returns empty stats.
    let db_a = open_project_session_db(&project).await.unwrap();
    let db_b = open_project_session_db(&project).await.unwrap();
    let (a, b) = tokio::join!(
        db_a.run_structured_backfill(),
        db_b.run_structured_backfill()
    );

    let a = a.expect("sweep a returns stats");
    let b = b.expect("sweep b returns stats");
    assert!(
        (a > 0) ^ (b > 0),
        "exactly one concurrent sweep inserts the missing row; the other is locked out (a={a}, b={b})"
    );
    assert_eq!(
        count_kind(&project, "codex", "goal").await,
        1,
        "the store converges to a single goal row"
    );
}

/// The watermark write is compare-and-set: it only ever moves forward, so a
/// slower concurrent sweep writing an earlier path cannot regress the cursor
/// and re-queue already-covered files.
#[tokio::test]
async fn structured_backfill_watermark_never_regresses() {
    use tracedecay::sessions::transcript_backfill::{
        read_structured_backfill_cursor_for_test, write_structured_backfill_cursor_for_test,
    };

    let tmp = TempDir::new().unwrap();
    let (_home, project) = init_project(&tmp);
    let db = open_project_session_db(&project).await.unwrap();

    write_structured_backfill_cursor_for_test(&db, "codex/aaa.jsonl")
        .await
        .unwrap();
    assert_eq!(
        read_structured_backfill_cursor_for_test(&db).await,
        "codex/aaa.jsonl"
    );

    // A forward move advances the cursor.
    write_structured_backfill_cursor_for_test(&db, "codex/zzz.jsonl")
        .await
        .unwrap();
    assert_eq!(
        read_structured_backfill_cursor_for_test(&db).await,
        "codex/zzz.jsonl"
    );

    // A backwards move (an earlier path from a slower/racing sweep) is a no-op.
    write_structured_backfill_cursor_for_test(&db, "codex/mmm.jsonl")
        .await
        .unwrap();
    assert_eq!(
        read_structured_backfill_cursor_for_test(&db).await,
        "codex/zzz.jsonl",
        "the watermark must never move backwards"
    );
}
