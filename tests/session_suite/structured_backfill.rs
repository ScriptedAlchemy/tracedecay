//! Insert-capable structured-row backfill coverage.

use tempfile::TempDir;
use tracedecay::sessions::claude::ClaudeSource;
use tracedecay::sessions::codex::CodexSource;
use tracedecay::sessions::transcript_backfill::StructuredBackfillTestRuntimeV1;
use tracedecay_domain::ProjectId;

fn init_project(tmp: &TempDir) -> (std::path::PathBuf, std::path::PathBuf) {
    let home = tmp.path().join("home");
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    (home, project)
}

async fn registered_runtime(
    tmp: &TempDir,
    project: &std::path::Path,
) -> StructuredBackfillTestRuntimeV1 {
    StructuredBackfillTestRuntimeV1::project(
        tmp.path().join("home/.tracedecay"),
        project,
        ProjectId::new("project.structured-backfill").unwrap(),
    )
    .await
    .unwrap()
}

async fn count_kind(runtime: &StructuredBackfillTestRuntimeV1, provider: &str, kind: &str) -> i64 {
    runtime.count_kind(provider, kind).await.unwrap()
}

async fn simulate_old_parser_store(
    runtime: &StructuredBackfillTestRuntimeV1,
    provider: &str,
    kind: &str,
) {
    runtime.remove_kind_and_reset(provider, kind).await.unwrap();
}

async fn load_only_goal_row(
    runtime: &StructuredBackfillTestRuntimeV1,
) -> (String, Option<String>, Option<String>) {
    runtime.goal_row().await.unwrap()
}

/// Reads a provider's per-provider structured-backfill marker version, or the
/// retired global marker when `provider` is `None`.
async fn structured_marker_version(
    runtime: &StructuredBackfillTestRuntimeV1,
    provider: Option<&str>,
) -> Option<i64> {
    runtime.marker_version(provider).await.unwrap()
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
    let tmp = TempDir::new().unwrap();
    let (home, project) = init_project(&tmp);
    write_claude_transcript_with_thinking(&home, &project, "claude-thinking");

    let runtime = registered_runtime(&tmp, &project).await;
    let source = ClaudeSource::with_home(&home);
    runtime.seed_source(&source, &project).await.unwrap();
    // Live ingest emits the reasoning row. The structured backfill only sets
    // up its Codex state and must not claim a Claude marker.
    runtime.run().await;
    assert_eq!(count_kind(&runtime, "claude", "reasoning").await, 1);
    // The user + assistant conversational rows (both kind "message") coexist
    // with the reasoning row.
    assert_eq!(count_kind(&runtime, "claude", "message").await, 2);

    // Remove the row and reset legacy backfill state. A second Claude parser
    // would recreate it; the observation pipeline must remain the sole
    // production Claude cursor authority, so the backfill leaves it absent.
    simulate_old_parser_store(&runtime, "claude", "reasoning").await;
    assert_eq!(count_kind(&runtime, "claude", "reasoning").await, 0);
    // Dropping reasoning rows leaves the conversational message rows untouched.
    assert_eq!(count_kind(&runtime, "claude", "message").await, 2);

    runtime.run().await;
    assert_eq!(count_kind(&runtime, "claude", "reasoning").await, 0);
    assert_eq!(count_kind(&runtime, "claude", "message").await, 2);
    assert_eq!(
        structured_marker_version(&runtime, Some("claude")).await,
        None
    );
}

#[tokio::test]
async fn structured_backfill_inserts_codex_goal_rows_once() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = init_project(&tmp);
    write_codex_rollout_with_goal(&home, &project, "codex-backfill");

    let runtime = registered_runtime(&tmp, &project).await;
    let source = CodexSource::with_home(&home);
    runtime.seed_source(&source, &project).await.unwrap();
    // Drive one sweep so the backfill meta table exists; the goal row from
    // fixture ingest is already present, so this inserts nothing.
    runtime.run().await;
    assert_eq!(count_kind(&runtime, "codex", "goal").await, 1);

    simulate_old_parser_store(&runtime, "codex", "goal").await;
    assert_eq!(count_kind(&runtime, "codex", "goal").await, 0);

    runtime.run().await;
    assert_eq!(count_kind(&runtime, "codex", "goal").await, 1);
    let goal = load_only_goal_row(&runtime).await;
    assert_eq!(goal.0, "ship the ingestion backfill", "goal text");
    assert_eq!(goal.1.as_deref(), Some("goal"), "goal kind");
    assert!(
        goal.2.is_some_and(|meta| meta.contains("objective")),
        "goal metadata should round-trip through the parser"
    );

    // A second sweep finds no candidates past the watermark and marks the
    // whole history complete.
    runtime.run().await;
    assert_eq!(count_kind(&runtime, "codex", "goal").await, 1);
    assert_eq!(
        structured_marker_version(&runtime, Some("codex")).await,
        Some(4)
    );
}

#[tokio::test]
async fn structured_backfill_preserves_existing_rows() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = init_project(&tmp);
    write_codex_rollout_with_goal(&home, &project, "codex-preserve");

    let runtime = registered_runtime(&tmp, &project).await;
    let source = CodexSource::with_home(&home);
    runtime.seed_source(&source, &project).await.unwrap();
    // Create the backfill meta table up front (see the note in the goal test).
    runtime.run().await;

    let before = runtime
        .session("codex", "codex-preserve")
        .await
        .unwrap()
        .unwrap();

    simulate_old_parser_store(&runtime, "codex", "goal").await;
    runtime.run().await;
    assert_eq!(count_kind(&runtime, "codex", "goal").await, 1);

    let after = runtime
        .session("codex", "codex-preserve")
        .await
        .unwrap()
        .unwrap();
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
    let tmp = TempDir::new().unwrap();
    let (home, project) = init_project(&tmp);
    write_codex_rollout_with_goal(&home, &project, "codex-versionbump");

    // Live ingest, then run the sweep to completion for the current version.
    let runtime = registered_runtime(&tmp, &project).await;
    let source = CodexSource::with_home(&home);
    runtime.seed_source(&source, &project).await.unwrap();
    runtime.run().await; // parses the file, advances the cursor
    runtime.run().await; // no candidates: marks complete, clears cursors
    assert_eq!(
        structured_marker_version(&runtime, Some("codex")).await,
        Some(4)
    );

    // Drop the structured rows and reset the marker so the sweep re-enters
    // (exactly what bumping codex's entry in `STRUCTURED_BACKFILL_VERSIONS`
    // does). Then plant a stale, *un-versioned* watermark parked at the last
    // transcript path.
    runtime.seed_stale_unversioned_cursor().await.unwrap();
    assert_eq!(count_kind(&runtime, "codex", "goal").await, 0);

    // The version-namespaced cursor key has never been written, so the sweep
    // starts from the beginning and re-parses the whole history — the stale
    // un-versioned cursor parked at the last path is ignored. (A regression to
    // an un-versioned key would resume past `last_path`, see zero candidates,
    // and leave the goal row missing.)
    runtime.run().await;
    assert_eq!(
        count_kind(&runtime, "codex", "goal").await,
        1,
        "a stale un-versioned cursor must not block a fresh version-bumped sweep"
    );
}

/// Migration: a store carrying the retired global `structured_rows_backfill`
/// marker at version N seeds every provider's marker to N, retires the global
/// marker and its legacy cursor rows, and triggers no spurious re-sweep.
#[tokio::test]
async fn structured_backfill_migrates_legacy_global_marker() {
    let tmp = TempDir::new().unwrap();
    let (home, project) = init_project(&tmp);
    write_codex_rollout_with_goal(&home, &project, "codex-migrate");

    let runtime = registered_runtime(&tmp, &project).await;
    runtime
        .seed_source(&CodexSource::with_home(&home), &project)
        .await
        .unwrap();
    // Ensure the meta table exists (production creates it via the sweep).
    runtime.run().await;

    // Rewrite the store into the legacy shape: a single global marker at v3
    // (a store that already finished the global sweep), no per-provider markers,
    // plus stale legacy cursor rows (un-versioned and global-versioned).
    runtime.seed_legacy_global_marker(3).await.unwrap();
    // Sanity: the legacy global marker is present, per-provider markers are not.
    assert_eq!(structured_marker_version(&runtime, None).await, Some(3));
    assert_eq!(
        structured_marker_version(&runtime, Some("claude")).await,
        None
    );
    assert_eq!(
        structured_marker_version(&runtime, Some("codex")).await,
        None
    );

    // The bounded sweep seeds the tracked Codex provider to N=3 and retires
    // the global marker/cursors. Codex then parses at its v4 custom-exec target;
    // Claude remains outside this legacy cursor authority entirely.
    runtime.run().await;
    runtime.run().await;

    assert_eq!(
        structured_marker_version(&runtime, Some("claude")).await,
        None,
        "legacy migration must not create a Claude backfill authority"
    );
    assert_eq!(
        structured_marker_version(&runtime, Some("codex")).await,
        Some(4),
        "codex marker advances from the legacy baseline to its current target"
    );
    assert_eq!(
        structured_marker_version(&runtime, None).await,
        None,
        "the retired global marker row is gone"
    );

    // The legacy cursor rows were cleaned; no spurious full re-sweep occurred.
    let leftover_cursors = runtime.cursor_count().await.unwrap();
    assert_eq!(
        leftover_cursors, 0,
        "legacy cursor rows are retired on migration"
    );
    assert_eq!(count_kind(&runtime, "codex", "goal").await, 1);
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
    let runtime = registered_runtime(&tmp, &project).await;
    let db_path = runtime.database_path();

    let winner = try_acquire_structured_backfill_lock(db_path);
    assert!(winner.is_some(), "first opener acquires the sweep lock");
    let loser = try_acquire_structured_backfill_lock(db_path);
    assert!(
        loser.is_none(),
        "a concurrent opener must be excluded while the lock is held"
    );

    drop(winner);
    let reacquired = try_acquire_structured_backfill_lock(db_path);
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
    let tmp = TempDir::new().unwrap();
    let (home, project) = init_project(&tmp);
    write_codex_rollout_with_goal(&home, &project, "codex-concurrent");

    let runtime = registered_runtime(&tmp, &project).await;
    let source = CodexSource::with_home(&home);
    runtime.seed_source(&source, &project).await.unwrap();
    runtime.run().await;

    // Drop the one goal row so a sweep has exactly one row to re-insert.
    simulate_old_parser_store(&runtime, "codex", "goal").await;
    assert_eq!(count_kind(&runtime, "codex", "goal").await, 0);

    // Two concurrent tasks borrow the same retained runtime authority. The
    // cross-process lock admits one; the other returns empty stats.
    let (a, b) = tokio::join!(runtime.run(), runtime.run());

    let a = a.expect("sweep a returns stats");
    let b = b.expect("sweep b returns stats");
    assert!(
        (a > 0) ^ (b > 0),
        "exactly one concurrent sweep inserts the missing row; the other is locked out (a={a}, b={b})"
    );
    assert_eq!(
        count_kind(&runtime, "codex", "goal").await,
        1,
        "the store converges to a single goal row"
    );
}

/// The watermark write is compare-and-set: it only ever moves forward, so a
/// slower concurrent sweep writing an earlier path cannot regress the cursor
/// and re-queue already-covered files.
#[tokio::test]
async fn structured_backfill_watermark_never_regresses() {
    let tmp = TempDir::new().unwrap();
    let (_home, project) = init_project(&tmp);
    let runtime = registered_runtime(&tmp, &project).await;

    runtime.write_cursor("codex/aaa.jsonl").await.unwrap();
    assert_eq!(runtime.read_cursor().await, "codex/aaa.jsonl");

    // A forward move advances the cursor.
    runtime.write_cursor("codex/zzz.jsonl").await.unwrap();
    assert_eq!(runtime.read_cursor().await, "codex/zzz.jsonl");

    // A backwards move (an earlier path from a slower/racing sweep) is a no-op.
    runtime.write_cursor("codex/mmm.jsonl").await.unwrap();
    assert_eq!(
        runtime.read_cursor().await,
        "codex/zzz.jsonl",
        "the watermark must never move backwards"
    );
}
