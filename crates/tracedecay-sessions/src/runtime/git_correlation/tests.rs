use libsql::Builder;

use super::*;

fn observation(session_id: &str, branch: Option<&str>, worktree: &str, ts: i64) -> SpanObservation {
    SpanObservation {
        provider: "claude".to_string(),
        session_id: session_id.to_string(),
        thread_id: None,
        branch: branch.map(str::to_string),
        worktree: worktree.to_string(),
        ts,
        source: SpanSource::HookRoute,
    }
}

async fn test_conn() -> Connection {
    let db = Builder::new_local(":memory:")
        .build()
        .await
        .expect("in-memory db");
    let conn = db.connect().expect("connect");
    ensure_git_correlation_schema(&conn)
        .await
        .expect("schema should apply");
    conn
}

#[test]
fn normalize_worktree_strips_trailing_slashes_and_backslashes() {
    assert_eq!(normalize_worktree("/repo/wt/"), "/repo/wt");
    assert_eq!(normalize_worktree("  /repo/wt  "), "/repo/wt");
    assert_eq!(normalize_worktree("/repo/wt///"), "/repo/wt");
    assert_eq!(normalize_worktree("/"), "/");
    assert_eq!(normalize_worktree("C:\\repo\\wt\\"), "C:/repo/wt");
    assert_eq!(normalize_worktree("//?/C:/repo/wt/"), "C:/repo/wt");
    assert_eq!(normalize_worktree("/private/var/tmp/repo"), "/var/tmp/repo");
}

#[test]
fn git_ref_filter_parses_and_validates_kinds() {
    assert_eq!(
        GitRefFilter::parse("branch", " feature/x "),
        Ok(GitRefFilter::Branch("feature/x".to_string()))
    );
    assert_eq!(
        GitRefFilter::parse("worktree", "/repo/wt/"),
        Ok(GitRefFilter::Worktree("/repo/wt".to_string()))
    );
    assert_eq!(
        GitRefFilter::parse("commit", "ABCDEF12"),
        Ok(GitRefFilter::Commit("abcdef12".to_string()))
    );
    assert!(GitRefFilter::parse("commit", "abc").is_err());
    assert!(GitRefFilter::parse("commit", "not-hex-at-all").is_err());
    assert!(GitRefFilter::parse("tag", "v1.0").is_err());
    assert!(GitRefFilter::parse("branch", "   ").is_err());
}

#[test]
fn observation_extends_span_only_within_gap() {
    assert!(observation_extends_span(100, 200, 150, 60));
    assert!(observation_extends_span(100, 200, 260, 60));
    assert!(observation_extends_span(100, 200, 40, 60));
    assert!(!observation_extends_span(100, 200, 261, 60));
    assert!(!observation_extends_span(100, 200, 39, 60));
}

#[test]
fn git_scope_filter_reports_emptiness_and_validates_commit() {
    let empty = GitScopeFilter::from_args(None, Some("  "), None).unwrap();
    assert!(empty.is_empty());
    let filter = GitScopeFilter::from_args(Some("main"), Some("/repo/"), Some("ABC123")).unwrap();
    assert_eq!(filter.branch.as_deref(), Some("main"));
    assert_eq!(filter.worktree.as_deref(), Some("/repo"));
    assert_eq!(filter.commit.as_deref(), Some("abc123"));
    assert!(GitScopeFilter::from_args(None, None, Some("xyz")).is_err());
}

#[test]
fn direct_commit_candidates_resolve_to_producer_evidence() {
    use std::process::Command;

    let temp = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(temp.path())
            .output()
            .unwrap()
    };
    assert!(git(&["init", "-q"]).status.success());
    assert!(
        git(&["config", "user.email", "test@example.com"])
            .status
            .success()
    );
    assert!(git(&["config", "user.name", "Test"]).status.success());
    std::fs::write(temp.path().join("file.txt"), "one\n").unwrap();
    assert!(git(&["add", "file.txt"]).status.success());
    assert!(git(&["commit", "-q", "-m", "test"]).status.success());
    let sha = String::from_utf8(git(&["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_string();
    let short = &sha[..8];
    let message = SessionMessageRecord {
        provider: "codex".to_string(),
        message_id: "m1".to_string(),
        session_id: "s1".to_string(),
        role: "tool".to_string(),
        timestamp: Some(10),
        ordinal: 1,
        text: "git commit -m test".to_string(),
        kind: Some("tool_call".to_string()),
        model: None,
        tool_names: Some("exec_command".to_string()),
        source_path: None,
        source_offset: None,
        metadata_json: Some(
            serde_json::json!({
                "produced_commit_candidates": [short],
                "codex_git_branch": "main",
                "codex_turn_worktree": temp.path(),
            })
            .to_string(),
        ),
    };
    let records = direct_commit_records(&[message], temp.path());
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].commit_sha, sha);
    assert_eq!(records[0].relation, CommitRelation::Produced);
    assert_eq!(records[0].evidence, CommitEvidence::ToolResult);
    assert_eq!(records[0].evidence_message_id.as_deref(), Some("m1"));
}

#[test]
fn transcript_locations_become_durable_ingest_spans() {
    let message = SessionMessageRecord {
        provider: "codex".to_string(),
        message_id: "m1".to_string(),
        session_id: "s1".to_string(),
        role: "assistant".to_string(),
        timestamp: Some(1_234),
        ordinal: 1,
        text: "done".to_string(),
        kind: Some("message".to_string()),
        model: None,
        tool_names: None,
        source_path: None,
        source_offset: None,
        metadata_json: Some(
            serde_json::json!({
                "codex_session_worktree": "/stale/session",
                "codex_turn_worktree": "/moved/repo/",
                "codex_git_branch": "feature/history",
                "turn_id": "turn-1"
            })
            .to_string(),
        ),
    };
    let observations = ingest_span_observations(&[message]);
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].worktree, "/moved/repo");
    assert_eq!(observations[0].branch.as_deref(), Some("feature/history"));
    assert_eq!(observations[0].thread_id.as_deref(), Some("turn-1"));
    assert_eq!(observations[0].source, SpanSource::Ingest);
}

#[tokio::test]
async fn schema_is_idempotent() {
    let conn = test_conn().await;
    ensure_git_correlation_schema(&conn)
        .await
        .expect("second ensure should be a no-op");
}

#[tokio::test]
async fn schema_v2_commit_rows_migrate_to_observed_without_becoming_producers() {
    let db = Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute_batch(
        "CREATE TABLE session_schema_migrations (
            name TEXT PRIMARY KEY,
            version INTEGER NOT NULL,
            applied_at INTEGER NOT NULL DEFAULT (unixepoch())
         );
         INSERT INTO session_schema_migrations(name, version)
            VALUES ('git_correlation', 2);
         CREATE TABLE session_git_spans (
            span_id INTEGER PRIMARY KEY AUTOINCREMENT,
            provider TEXT NOT NULL DEFAULT '',
            session_id TEXT NOT NULL,
            thread_id TEXT,
            branch TEXT,
            worktree TEXT NOT NULL,
            first_ts INTEGER NOT NULL,
            last_ts INTEGER NOT NULL,
            event_count INTEGER NOT NULL DEFAULT 1,
            source TEXT NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
         );
         CREATE TABLE commit_sessions (
            commit_sha TEXT NOT NULL,
            provider TEXT NOT NULL DEFAULT '',
            session_id TEXT NOT NULL,
            branch TEXT,
            worktree TEXT,
            committed_at INTEGER NOT NULL,
            span_overlap_kind TEXT NOT NULL,
            span_id INTEGER,
            created_at INTEGER NOT NULL DEFAULT (unixepoch()),
            PRIMARY KEY(commit_sha, provider, session_id)
         );
         CREATE TABLE git_correlation_meta (
            key TEXT PRIMARY KEY,
            value INTEGER NOT NULL,
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
         );
         INSERT INTO commit_sessions(
            commit_sha, provider, session_id, branch, worktree,
            committed_at, span_overlap_kind, span_id
         ) VALUES (
            'abcdef1234567890abcdef1234567890abcdef12',
            'claude', 'observed-session', 'main', '/repo', 1150,
            'within_span', NULL
         );",
    )
    .await
    .unwrap();

    ensure_git_correlation_schema(&conn).await.unwrap();
    let query = SessionsForQuery {
        git_ref: GitRefFilter::Commit("abcdef12".to_string()),
        since: None,
        until: None,
        limit: 10,
    };
    assert!(
        sessions_for(&conn, &query).await.unwrap().is_empty(),
        "migrated overlap rows must not satisfy producer-default queries"
    );
    let observed = sessions_for_with_relation(&conn, &query, CommitRelationFilter::Observed)
        .await
        .unwrap();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].relation, Some(CommitRelation::Observed));
    assert_eq!(observed[0].evidence, Some(CommitEvidence::TimeOverlap));
    assert_eq!(observed[0].confidence, Some(20));

    let produced = CommitSessionRecord {
        commit_sha: "1111111111111111111111111111111111111111".to_string(),
        provider: "codex".to_string(),
        session_id: "producer-session".to_string(),
        branch: Some("main".to_string()),
        worktree: Some("/repo".to_string()),
        committed_at: 1200,
        span_overlap_kind: SpanOverlapKind::Direct,
        span_id: None,
        relation: CommitRelation::Produced,
        evidence: CommitEvidence::ToolResult,
        confidence: 100,
        evidence_message_id: Some("m1".to_string()),
    };
    assert!(upsert_commit_session(&conn, &produced).await.unwrap());
    let produced_hits = sessions_for(
        &conn,
        &SessionsForQuery {
            git_ref: GitRefFilter::Commit("11111111".to_string()),
            since: None,
            until: None,
            limit: 10,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        produced_hits[0].span_overlap_kind,
        Some(SpanOverlapKind::Direct)
    );

    ensure_git_correlation_schema(&conn)
        .await
        .expect("v3 migration must be repeat-safe");
}

#[tokio::test]
async fn future_git_correlation_schema_is_rejected_without_rewrite() {
    let db = Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute_batch(
        "CREATE TABLE session_schema_migrations (
            name TEXT PRIMARY KEY,
            version INTEGER NOT NULL,
            applied_at INTEGER NOT NULL DEFAULT (unixepoch())
         );
         INSERT INTO session_schema_migrations(name, version)
            VALUES ('git_correlation', 99);",
    )
    .await
    .unwrap();

    let err = ensure_git_correlation_schema(&conn).await.unwrap_err();
    assert!(err.to_string().contains("newer git correlation schema 99"));
    assert_eq!(schema_version(&conn).await.unwrap(), Some(99));
}

#[tokio::test]
async fn observations_merge_within_gap_and_split_on_branch_switch() {
    let conn = test_conn().await;
    let first =
        record_span_observation(&conn, &observation("s1", Some("main"), "/repo", 1_000), 600)
            .await
            .unwrap();
    let merged =
        record_span_observation(&conn, &observation("s1", Some("main"), "/repo", 1_300), 600)
            .await
            .unwrap();
    assert_eq!(first, merged, "in-gap observation should extend the span");

    // Branch switch mid-session opens a second span; switching back
    // within the gap of the first span extends it again (A → B → A).
    let switched =
        record_span_observation(&conn, &observation("s1", Some("feat"), "/repo", 1_400), 600)
            .await
            .unwrap();
    assert_ne!(first, switched);
    let back =
        record_span_observation(&conn, &observation("s1", Some("main"), "/repo", 1_700), 600)
            .await
            .unwrap();
    assert_eq!(first, back);

    // Out-of-gap observation on the same branch opens a new span.
    let idle_gap =
        record_span_observation(&conn, &observation("s1", Some("main"), "/repo", 9_000), 600)
            .await
            .unwrap();
    assert_ne!(first, idle_gap);

    // Detached HEAD (branch = NULL) never merges into a named span.
    let detached = record_span_observation(&conn, &observation("s1", None, "/repo", 1_750), 600)
        .await
        .unwrap();
    assert_ne!(first, detached);
}

#[tokio::test]
async fn sessions_for_branch_worktree_and_commit_round_trip() {
    let conn = test_conn().await;
    record_span_observation(&conn, &observation("s1", Some("main"), "/repo", 1_000), 600)
        .await
        .unwrap();
    record_span_observation(&conn, &observation("s1", Some("main"), "/repo", 1_200), 600)
        .await
        .unwrap();
    record_span_observation(
        &conn,
        &observation("s2", Some("main"), "/repo/wt", 5_000),
        600,
    )
    .await
    .unwrap();
    record_span_observation(&conn, &observation("s3", Some("feat"), "/repo", 2_000), 600)
        .await
        .unwrap();

    let hits = sessions_for(
        &conn,
        &SessionsForQuery {
            git_ref: GitRefFilter::Branch("main".to_string()),
            since: None,
            until: None,
            limit: 10,
        },
    )
    .await
    .unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].session_id, "s2", "most recent activity first");
    assert_eq!(hits[1].session_id, "s1");
    assert_eq!(hits[1].event_count, 2);
    assert_eq!(hits[1].span_count, 1);
    assert_eq!(hits[1].first_ts, Some(1_000));
    assert_eq!(hits[1].last_ts, Some(1_200));
    assert_eq!(hits[1].sources, vec!["hook_route".to_string()]);
    assert_eq!(hits[1].branch.as_deref(), Some("main"));
    assert_eq!(hits[1].worktree.as_deref(), Some("/repo"));

    // Time-scoped: only the span overlapping [4000, 6000] survives.
    let scoped = sessions_for(
        &conn,
        &SessionsForQuery {
            git_ref: GitRefFilter::Branch("main".to_string()),
            since: Some(4_000),
            until: Some(6_000),
            limit: 10,
        },
    )
    .await
    .unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].session_id, "s2");

    let by_worktree = sessions_for(
        &conn,
        &SessionsForQuery {
            git_ref: GitRefFilter::Worktree("/repo/wt".to_string()),
            since: None,
            until: None,
            limit: 10,
        },
    )
    .await
    .unwrap();
    assert_eq!(by_worktree.len(), 1);
    assert_eq!(by_worktree[0].session_id, "s2");

    let record = CommitSessionRecord {
        commit_sha: "abcdef1234567890abcdef1234567890abcdef12".to_string(),
        provider: "claude".to_string(),
        session_id: "s1".to_string(),
        branch: Some("main".to_string()),
        worktree: Some("/repo".to_string()),
        committed_at: 1_150,
        span_overlap_kind: SpanOverlapKind::WithinSpan,
        span_id: None,
        relation: CommitRelation::Produced,
        evidence: CommitEvidence::ToolResult,
        confidence: 100,
        evidence_message_id: Some("tool-result-1".to_string()),
    };
    assert!(upsert_commit_session(&conn, &record).await.unwrap());
    assert!(
        !upsert_commit_session(&conn, &record).await.unwrap(),
        "second upsert should be an idempotent no-op"
    );

    let by_commit = sessions_for(
        &conn,
        &SessionsForQuery {
            git_ref: GitRefFilter::Commit("abcdef12".to_string()),
            since: None,
            until: None,
            limit: 10,
        },
    )
    .await
    .unwrap();
    assert_eq!(by_commit.len(), 1);
    assert_eq!(by_commit[0].session_id, "s1");
    assert_eq!(
        by_commit[0].commit_sha.as_deref(),
        Some("abcdef1234567890abcdef1234567890abcdef12")
    );
    assert_eq!(by_commit[0].committed_at, Some(1_150));
    assert_eq!(by_commit[0].relation, Some(CommitRelation::Produced));
    assert_eq!(by_commit[0].evidence, Some(CommitEvidence::ToolResult));
    assert_eq!(by_commit[0].confidence, Some(100));
    assert_eq!(
        by_commit[0].evidence_message_id.as_deref(),
        Some("tool-result-1")
    );
    assert_eq!(
        by_commit[0].span_overlap_kind,
        Some(SpanOverlapKind::WithinSpan)
    );
}

#[test]
fn branch_timeline_parses_checkouts_and_detached_and_skips_noise() {
    let reflog = concat!(
        "def456 HEAD@{1700000300}: commit: work on feat\n",
        "def456 HEAD@{1700000200}: checkout: moving from main to feat\n",
        "abc123 HEAD@{1700000150}: checkout: moving from feat to a1b2c3d4e5f6\n",
        "abc123 HEAD@{1700000100}: checkout: moving from a1b2c3d4e5f6 to main\n",
        "abc123 HEAD@{1700000050}: clone: from origin\n",
    );
    let timeline = branch_timeline_from_reflog(reflog);
    // Oldest-first, only checkout lines, detached HEAD → None.
    assert_eq!(
        timeline,
        vec![
            (1_700_000_100, Some("main".to_string())),
            (1_700_000_150, None),
            (1_700_000_200, Some("feat".to_string())),
        ]
    );
}

#[test]
fn branch_timeline_ignores_unparseable_lines() {
    assert!(branch_timeline_from_reflog("").is_empty());
    assert!(branch_timeline_from_reflog("garbage without a head marker").is_empty());
    // Non-checkout HEAD entries do not advance the timeline.
    assert!(branch_timeline_from_reflog("abc HEAD@{1700000000}: reset: moving to HEAD").is_empty());
}

#[test]
fn window_segments_split_on_mid_window_branch_switch() {
    // Session ran [100, 300]; HEAD switched main→feat at 200.
    let timeline = vec![
        (150, Some("main".to_string())),
        (200, Some("feat".to_string())),
    ];
    let segments = window_branch_segments(100, 300, &timeline, Some("main"));
    assert_eq!(
        segments,
        vec![
            WindowBranchSegment {
                branch: Some("main".to_string()),
                start: 100,
                end: 200,
            },
            WindowBranchSegment {
                branch: Some("feat".to_string()),
                start: 200,
                end: 300,
            },
        ]
    );
}

#[test]
fn window_segments_use_initial_branch_before_first_entry() {
    // No timeline entry inside the window → whole window is initial_branch.
    let segments = window_branch_segments(100, 300, &[], Some("main"));
    assert_eq!(
        segments,
        vec![WindowBranchSegment {
            branch: Some("main".to_string()),
            start: 100,
            end: 300,
        }]
    );
    // An entry at or before win_start sets the floor branch.
    let timeline = vec![(50, Some("feat".to_string()))];
    let segments = window_branch_segments(100, 300, &timeline, Some("main"));
    assert_eq!(segments[0].branch.as_deref(), Some("feat"));
}

#[test]
fn window_segments_empty_when_start_after_end() {
    assert!(window_branch_segments(300, 100, &[], Some("main")).is_empty());
}

#[test]
fn session_activity_row_window_spans_widest_bounds() {
    let row = SessionActivityRow {
        provider: "claude".to_string(),
        session_id: "s1".to_string(),
        project_path: "/repo".to_string(),
        started_at: Some(200),
        ended_at: None,
        message_min_ts: Some(150),
        message_max_ts: Some(400),
    };
    assert_eq!(row.window(), Some((150, 400)));
    let empty = SessionActivityRow {
        started_at: None,
        ended_at: None,
        message_min_ts: None,
        message_max_ts: None,
        ..row
    };
    assert_eq!(empty.window(), None);
}

#[test]
fn session_activity_row_window_normalizes_millis_bounds() {
    // Legacy/mixed stores can hold millisecond-scale message timestamps
    // (see `latest_session_activity_secs`). Left un-normalized the window
    // would be ~1000x too wide, so a seconds-scale git commit time could
    // never fall inside it. `window()` must collapse to the seconds scale.
    let commit_ts = 1_700_000_500; // seconds-scale git %ct
    let row = SessionActivityRow {
        provider: "claude".to_string(),
        session_id: "s1".to_string(),
        project_path: "/repo".to_string(),
        started_at: None,
        ended_at: None,
        message_min_ts: Some(1_700_000_000_000), // millis
        message_max_ts: Some(1_700_001_000_000), // millis
    };
    let (start, end) = row.window().expect("window from millis bounds");
    assert_eq!((start, end), (1_700_000_000, 1_700_001_000));
    // The seconds-scale commit now lands inside the seconds-scale span,
    // which a millis-scale window could never contain.
    assert_eq!(
        commit_overlap_kind(start, end, commit_ts, 600),
        Some(SpanOverlapKind::WithinSpan)
    );
}

#[test]
fn parse_commit_log_skips_malformed_and_caps() {
    let log = concat!(
        "ABCDEF1234 1700000000\n",
        "not-a-sha 1700000100\n",
        "deadbeef xyz\n",
        "cafebabe 1700000200\n",
    );
    let commits = parse_commit_log(log, 10);
    assert_eq!(
        commits,
        vec![
            ("abcdef1234".to_string(), 1_700_000_000),
            ("cafebabe".to_string(), 1_700_000_200),
        ]
    );
    assert_eq!(parse_commit_log(log, 1).len(), 1);
}

#[test]
fn debounce_suppresses_bursts_but_admits_after_interval() {
    let mut debounce = SpanObservationDebounce::new();
    let key = span_debounce_key("", "s1", Some("main"), "/repo");
    // First observation always writes; a burst inside the interval is
    // suppressed; a later observation past the interval writes again.
    assert!(debounce.should_record(&key, 1_000, 30));
    assert!(!debounce.should_record(&key, 1_005, 30));
    assert!(!debounce.should_record(&key, 1_029, 30));
    assert!(debounce.should_record(&key, 1_030, 30));
    assert!(!debounce.should_record(&key, 1_040, 30));
}

#[test]
fn debounce_keys_separate_branch_and_worktree_and_detached() {
    let mut debounce = SpanObservationDebounce::new();
    let main = span_debounce_key("", "s1", Some("main"), "/repo");
    let feat = span_debounce_key("", "s1", Some("feat"), "/repo");
    let other_wt = span_debounce_key("", "s1", Some("main"), "/repo/wt");
    let detached = span_debounce_key("", "s1", None, "/repo");
    // A branch switch is never debounced away by a prior branch's write.
    assert!(debounce.should_record(&main, 1_000, 30));
    assert!(debounce.should_record(&feat, 1_001, 30));
    assert!(debounce.should_record(&other_wt, 1_002, 30));
    assert!(debounce.should_record(&detached, 1_003, 30));
    // Distinct keys for the four cases.
    assert_ne!(main, feat);
    assert_ne!(main, other_wt);
    assert_ne!(main, detached);
}

#[test]
fn debounce_admits_out_of_order_older_observation() {
    let mut debounce = SpanObservationDebounce::new();
    let key = span_debounce_key("", "s1", Some("main"), "/repo");
    assert!(debounce.should_record(&key, 1_000, 30));
    // An out-of-order (older) timestamp is never suppressed.
    assert!(debounce.should_record(&key, 900, 30));
}

#[test]
fn commit_overlap_kind_classifies_within_extended_and_outside() {
    assert_eq!(
        commit_overlap_kind(100, 200, 150, 60),
        Some(SpanOverlapKind::WithinSpan)
    );
    assert_eq!(
        commit_overlap_kind(100, 200, 100, 60),
        Some(SpanOverlapKind::WithinSpan)
    );
    assert_eq!(
        commit_overlap_kind(100, 200, 260, 60),
        Some(SpanOverlapKind::ExtendedWindow)
    );
    assert_eq!(
        commit_overlap_kind(100, 200, 40, 60),
        Some(SpanOverlapKind::ExtendedWindow)
    );
    assert_eq!(commit_overlap_kind(100, 200, 261, 60), None);
    assert_eq!(commit_overlap_kind(100, 200, 39, 60), None);
}

#[test]
fn match_commit_to_spans_filters_by_branch_worktree_and_window() {
    let spans = vec![
        SpanWindow {
            span_id: 1,
            provider: "claude".to_string(),
            session_id: "s1".to_string(),
            branch: Some("main".to_string()),
            worktree: "/repo".to_string(),
            first_ts: 100,
            last_ts: 200,
        },
        // Concurrent session on the same branch/worktree.
        SpanWindow {
            span_id: 2,
            provider: String::new(),
            session_id: "s2".to_string(),
            branch: Some("main".to_string()),
            worktree: "/repo".to_string(),
            first_ts: 120,
            last_ts: 190,
        },
        // Different branch — must not match a main commit.
        SpanWindow {
            span_id: 3,
            provider: "claude".to_string(),
            session_id: "s3".to_string(),
            branch: Some("feat".to_string()),
            worktree: "/repo".to_string(),
            first_ts: 100,
            last_ts: 200,
        },
        // Different worktree — must not match.
        SpanWindow {
            span_id: 4,
            provider: "claude".to_string(),
            session_id: "s4".to_string(),
            branch: Some("main".to_string()),
            worktree: "/repo/wt".to_string(),
            first_ts: 100,
            last_ts: 200,
        },
    ];

    // A within-window commit is observed by both concurrent main spans. Time
    // overlap is never direct evidence that either session produced it.
    let records = match_commit_to_spans("deadbeef", Some("main"), "/repo", 150, &spans, 60);
    assert_eq!(records.len(), 2);
    let ids: Vec<i64> = records.iter().filter_map(|r| r.span_id).collect();
    assert_eq!(ids, vec![1, 2]);
    assert!(
        records
            .iter()
            .all(|r| r.span_overlap_kind == SpanOverlapKind::WithinSpan)
    );
    assert!(
        records
            .iter()
            .all(|r| r.relation == CommitRelation::Observed)
    );
    assert!(
        records
            .iter()
            .all(|r| r.evidence == CommitEvidence::TimeOverlap && r.confidence == 20)
    );
    assert_eq!(records[0].session_id, "s1");
    assert_eq!(records[0].worktree.as_deref(), Some("/repo"));

    // A just-past-the-edge commit lands in the extended window only.
    let extended = match_commit_to_spans("cafef00d", Some("main"), "/repo", 250, &spans, 60);
    assert_eq!(extended.len(), 2);
    assert!(
        extended
            .iter()
            .all(|r| r.span_overlap_kind == SpanOverlapKind::ExtendedWindow)
    );

    // A commit outside every window attributes nothing.
    assert!(match_commit_to_spans("beefcafe", Some("main"), "/repo", 500, &spans, 60).is_empty());
    // A commit on an unrecorded branch attributes nothing.
    assert!(match_commit_to_spans("beefcafe", Some("other"), "/repo", 150, &spans, 60).is_empty());
}

#[tokio::test]
async fn commit_attribution_sweep_attributes_and_advances_watermark() {
    let conn = test_conn().await;
    // One session active on main in /repo over [1000, 2000]. The 1000-wide
    // gap between the two observations exceeds the 600 merge gap, so a third
    // in-window observation keeps them a single contiguous span.
    record_span_observation(&conn, &observation("s1", Some("main"), "/repo", 1_000), 600)
        .await
        .unwrap();
    record_span_observation(&conn, &observation("s1", Some("main"), "/repo", 1_500), 600)
        .await
        .unwrap();
    record_span_observation(&conn, &observation("s1", Some("main"), "/repo", 2_000), 600)
        .await
        .unwrap();

    // Sweep with an injected scan returning one in-window commit.
    let inserted = run_commit_attribution_sweep(&conn, 600, |target| {
        assert_eq!(target.branch.as_deref(), Some("main"));
        assert_eq!(target.worktree, "/repo");
        vec![ScannedCommit {
            sha: "abcdef1234567890abcdef1234567890abcdef12".to_string(),
            committed_at: 1_500,
        }]
    })
    .await
    .unwrap();
    assert_eq!(inserted, 1);

    // Time-overlap sweep rows are not producer evidence. They remain available
    // only through an explicit observed/all query.
    let query = SessionsForQuery {
        git_ref: GitRefFilter::Commit("abcdef12".to_string()),
        since: None,
        until: None,
        limit: 10,
    };
    assert!(sessions_for(&conn, &query).await.unwrap().is_empty());
    let hits = sessions_for_with_relation(&conn, &query, CommitRelationFilter::Observed)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session_id, "s1");
    assert_eq!(hits[0].span_overlap_kind, Some(SpanOverlapKind::WithinSpan));
    assert_eq!(hits[0].relation, Some(CommitRelation::Observed));

    // Re-running the sweep is idempotent: even if the boundary span is
    // re-scanned (watermark uses `>=` so nothing is ever missed), the
    // commit is already attributed and the upsert inserts nothing more.
    let again = run_commit_attribution_sweep(&conn, 600, |_| {
        vec![ScannedCommit {
            sha: "abcdef1234567890abcdef1234567890abcdef12".to_string(),
            committed_at: 1_500,
        }]
    })
    .await
    .unwrap();
    assert_eq!(again, 0, "re-attribution of the same commit is a no-op");
}

fn span_with(
    provider: &str,
    session_id: &str,
    branch: Option<&str>,
    worktree: &str,
    ts: i64,
    source: SpanSource,
) -> SpanObservation {
    SpanObservation {
        provider: provider.to_string(),
        session_id: session_id.to_string(),
        thread_id: None,
        branch: branch.map(str::to_string),
        worktree: worktree.to_string(),
        ts,
        source,
    }
}

#[test]
fn head_observation_candidates_record_observed_not_produced() {
    use std::process::Command;

    let temp = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(temp.path())
            .output()
            .unwrap()
    };
    assert!(git(&["init", "-q"]).status.success());
    assert!(
        git(&["config", "user.email", "test@example.com"])
            .status
            .success()
    );
    assert!(git(&["config", "user.name", "Test"]).status.success());
    std::fs::write(temp.path().join("file.txt"), "one\n").unwrap();
    assert!(git(&["add", "file.txt"]).status.success());
    assert!(git(&["commit", "-q", "-m", "test"]).status.success());
    let sha = String::from_utf8(git(&["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_string();

    let message = SessionMessageRecord {
        provider: "codex".to_string(),
        message_id: "m1".to_string(),
        session_id: "s1".to_string(),
        role: "tool".to_string(),
        timestamp: Some(10),
        ordinal: 1,
        text: "git rev-parse HEAD".to_string(),
        kind: Some("tool_call".to_string()),
        model: None,
        tool_names: Some("exec_command".to_string()),
        source_path: None,
        source_offset: None,
        metadata_json: Some(
            serde_json::json!({
                "observed_commit_candidates": [sha],
                "codex_git_branch": "main",
                "codex_turn_worktree": temp.path(),
            })
            .to_string(),
        ),
    };
    let records = direct_commit_records(&[message], temp.path());
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].commit_sha, sha);
    assert_eq!(records[0].relation, CommitRelation::Observed);
    assert_eq!(records[0].evidence, CommitEvidence::HeadObservation);
    assert_eq!(records[0].confidence, HEAD_OBSERVATION_CONFIDENCE);
}

#[test]
fn producer_evidence_wins_over_head_observation_of_same_commit() {
    use std::process::Command;

    let temp = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(temp.path())
            .output()
            .unwrap()
    };
    assert!(git(&["init", "-q"]).status.success());
    assert!(
        git(&["config", "user.email", "test@example.com"])
            .status
            .success()
    );
    assert!(git(&["config", "user.name", "Test"]).status.success());
    std::fs::write(temp.path().join("file.txt"), "one\n").unwrap();
    assert!(git(&["add", "file.txt"]).status.success());
    assert!(git(&["commit", "-q", "-m", "test"]).status.success());
    let sha = String::from_utf8(git(&["rev-parse", "HEAD"]).stdout)
        .unwrap()
        .trim()
        .to_string();
    let short = &sha[..8];

    // The same exec both created the commit (bracket) and printed HEAD, so the
    // one commit appears in both candidate lists. Only one producer record must
    // survive.
    let message = SessionMessageRecord {
        provider: "codex".to_string(),
        message_id: "m1".to_string(),
        session_id: "s1".to_string(),
        role: "tool".to_string(),
        timestamp: Some(10),
        ordinal: 1,
        text: "git commit && git rev-parse HEAD".to_string(),
        kind: Some("tool_call".to_string()),
        model: None,
        tool_names: Some("exec_command".to_string()),
        source_path: None,
        source_offset: None,
        metadata_json: Some(
            serde_json::json!({
                "produced_commit_candidates": [short],
                "observed_commit_candidates": [sha],
                "codex_git_branch": "main",
                "codex_turn_worktree": temp.path(),
            })
            .to_string(),
        ),
    };
    let records = direct_commit_records(&[message], temp.path());
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].relation, CommitRelation::Produced);
    assert_eq!(records[0].confidence, 100);
}

#[tokio::test]
async fn mixed_provider_spans_collapse_to_one_session_in_branch_query() {
    let conn = test_conn().await;
    // A live session observed by the provider-agnostic hook route (provider '')
    // and again by transcript ingest (provider 'claude').
    record_span_observation(
        &conn,
        &span_with(
            "",
            "s1",
            Some("main"),
            "/repo",
            1_000,
            SpanSource::HookRoute,
        ),
        600,
    )
    .await
    .unwrap();
    record_span_observation(
        &conn,
        &span_with(
            "claude",
            "s1",
            Some("main"),
            "/repo",
            1_050,
            SpanSource::Ingest,
        ),
        600,
    )
    .await
    .unwrap();

    let hits = sessions_for(
        &conn,
        &SessionsForQuery {
            git_ref: GitRefFilter::Branch("main".to_string()),
            since: None,
            until: None,
            limit: 10,
        },
    )
    .await
    .unwrap();
    assert_eq!(hits.len(), 1, "one session must not split into two rows");
    assert_eq!(hits[0].session_id, "s1");
    assert_eq!(hits[0].provider, "claude", "real provider is canonical");
    assert_eq!(hits[0].event_count, 2, "counts are summed, not split");
    assert_eq!(hits[0].span_count, 2);
}

#[tokio::test]
async fn mixed_provider_commit_rows_collapse_to_one_hit() {
    let conn = test_conn().await;
    let sha = "abcdef1234567890abcdef1234567890abcdef12";
    for provider in ["", "claude"] {
        upsert_commit_session(
            &conn,
            &CommitSessionRecord {
                commit_sha: sha.to_string(),
                provider: provider.to_string(),
                session_id: "s1".to_string(),
                branch: Some("main".to_string()),
                worktree: Some("/repo".to_string()),
                committed_at: 1_500,
                span_overlap_kind: SpanOverlapKind::WithinSpan,
                span_id: None,
                relation: CommitRelation::Observed,
                evidence: CommitEvidence::TimeOverlap,
                confidence: 20,
                evidence_message_id: None,
            },
        )
        .await
        .unwrap();
    }

    let hits = sessions_for_with_relation(
        &conn,
        &SessionsForQuery {
            git_ref: GitRefFilter::Commit("abcdef12".to_string()),
            since: None,
            until: None,
            limit: 10,
        },
        CommitRelationFilter::All,
    )
    .await
    .unwrap();
    assert_eq!(hits.len(), 1, "one session must not be double-counted");
    assert_eq!(hits[0].session_id, "s1");
    assert_eq!(hits[0].provider, "claude");
}

#[tokio::test]
async fn attribution_sweep_writes_one_row_for_mixed_provider_session() {
    let conn = test_conn().await;
    // Same session, two provider identities, both overlapping the commit.
    record_span_observation(
        &conn,
        &span_with(
            "",
            "s1",
            Some("main"),
            "/repo",
            1_500,
            SpanSource::HookRoute,
        ),
        600,
    )
    .await
    .unwrap();
    record_span_observation(
        &conn,
        &span_with(
            "claude",
            "s1",
            Some("main"),
            "/repo",
            1_500,
            SpanSource::Ingest,
        ),
        600,
    )
    .await
    .unwrap();

    let sha = "abcdef1234567890abcdef1234567890abcdef12";
    run_commit_attribution_sweep(&conn, 600, |_| {
        vec![ScannedCommit {
            sha: sha.to_string(),
            committed_at: 1_500,
        }]
    })
    .await
    .unwrap();

    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM commit_sessions WHERE commit_sha = ?1",
            libsql::params![sha],
        )
        .await
        .unwrap();
    let count: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(
        count, 1,
        "canonical provider yields a single attribution row"
    );
}

#[tokio::test]
async fn commit_scope_falls_back_to_observed_when_no_producer_exists() {
    let conn = test_conn().await;
    // Only an observed row exists (the state of every migrated v2 store).
    upsert_commit_session(
        &conn,
        &CommitSessionRecord {
            commit_sha: "abcdef1234567890abcdef1234567890abcdef12".to_string(),
            provider: "claude".to_string(),
            session_id: "observer".to_string(),
            branch: Some("main".to_string()),
            worktree: Some("/repo".to_string()),
            committed_at: 1_500,
            span_overlap_kind: SpanOverlapKind::WithinSpan,
            span_id: None,
            relation: CommitRelation::Observed,
            evidence: CommitEvidence::TimeOverlap,
            confidence: 20,
            evidence_message_id: None,
        },
    )
    .await
    .unwrap();

    let filter = GitScopeFilter::from_args(None, None, Some("abcdef12")).unwrap();
    let ids = session_ids_for_scope(&conn, &filter)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        ids,
        vec![("claude".to_string(), "observer".to_string())],
        "observed-only commit rows must still resolve for commit scope"
    );

    // Once a producer is known, the observer no longer resolves — producer
    // evidence takes precedence.
    upsert_commit_session(
        &conn,
        &CommitSessionRecord {
            commit_sha: "abcdef1234567890abcdef1234567890abcdef12".to_string(),
            provider: "codex".to_string(),
            session_id: "producer".to_string(),
            branch: Some("main".to_string()),
            worktree: Some("/repo".to_string()),
            committed_at: 1_500,
            span_overlap_kind: SpanOverlapKind::Direct,
            span_id: None,
            relation: CommitRelation::Produced,
            evidence: CommitEvidence::ToolResult,
            confidence: 100,
            evidence_message_id: Some("m1".to_string()),
        },
    )
    .await
    .unwrap();
    let ids = session_ids_for_scope(&conn, &filter)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ids, vec![("codex".to_string(), "producer".to_string())]);
}

#[tokio::test]
async fn sweep_watermark_tracks_ingest_time_so_late_history_is_attributed() {
    let conn = test_conn().await;
    // A recent session fixes the watermark near "now".
    record_span_observation(
        &conn,
        &span_with(
            "claude",
            "recent",
            Some("main"),
            "/repo",
            10_000,
            SpanSource::Ingest,
        ),
        600,
    )
    .await
    .unwrap();
    run_commit_attribution_sweep(&conn, 600, |_| Vec::new())
        .await
        .unwrap();
    let watermark = read_meta_value(&conn, "commit_attribution_watermark")
        .await
        .unwrap()
        .unwrap();

    // A historical session is ingested late: its event time (500) is far below
    // the watermark, but its write time is now.
    record_span_observation(
        &conn,
        &span_with(
            "claude",
            "historical",
            Some("feat"),
            "/repo",
            500,
            SpanSource::Ingest,
        ),
        600,
    )
    .await
    .unwrap();
    assert!(
        500 < watermark,
        "historical event time must sit below the watermark to exercise the fix"
    );

    let inserted = run_commit_attribution_sweep(&conn, 600, |target| {
        if target.branch.as_deref() == Some("feat") {
            vec![ScannedCommit {
                sha: "abcdef1234567890abcdef1234567890abcdef12".to_string(),
                committed_at: 500,
            }]
        } else {
            Vec::new()
        }
    })
    .await
    .unwrap();
    assert_eq!(
        inserted, 1,
        "a late-ingested historical span must still be attributed"
    );
}

#[test]
fn activity_sort_key_prefers_message_then_end_then_start() {
    let row = |started, ended, msg_max| SessionActivityRow {
        provider: "claude".to_string(),
        session_id: "s".to_string(),
        project_path: "/p".to_string(),
        started_at: started,
        ended_at: ended,
        message_min_ts: None,
        message_max_ts: msg_max,
    };
    assert_eq!(row(Some(1), Some(2), Some(3)).activity_sort_key(), Some(3));
    assert_eq!(row(Some(1), Some(2), None).activity_sort_key(), Some(2));
    assert_eq!(row(Some(1), None, None).activity_sort_key(), Some(1));
    assert_eq!(row(None, None, None).activity_sort_key(), None);
}

#[tokio::test]
async fn correlation_index_health_reports_empty_then_populated() {
    let conn = test_conn().await;

    // Freshly-migrated store: tables exist but hold nothing.
    let empty = correlation_index_health(&conn).await.unwrap();
    assert!(empty.tables_present);
    assert_eq!(empty.span_count, 0);
    assert_eq!(empty.commit_count, 0);
    assert_eq!(empty.last_span_write, None);
    assert_eq!(empty.backfill_watermark, None);
    assert!(empty.is_empty(), "no spans means the index is empty");

    // One recorded observation flips the index to populated with a write time.
    record_span_observation(&conn, &observation("s1", Some("main"), "/repo", 1_000), 600)
        .await
        .unwrap();
    let populated = correlation_index_health(&conn).await.unwrap();
    assert_eq!(populated.span_count, 1);
    assert!(populated.last_span_write.is_some());
    assert!(!populated.is_empty());

    // The auto-backfill watermark surfaces once a pass has written it.
    write_meta_value(&conn, AUTO_BACKFILL_WATERMARK_KEY, 4_242)
        .await
        .unwrap();
    let with_watermark = correlation_index_health(&conn).await.unwrap();
    assert_eq!(with_watermark.backfill_watermark, Some(4_242));
}

#[tokio::test]
async fn correlation_index_health_without_tables_is_empty() {
    // A store predating the correlation schema (no DDL run) must report an
    // absent, empty index rather than erroring on missing tables.
    let db = Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();
    let health = correlation_index_health(&conn).await.unwrap();
    assert!(!health.tables_present);
    assert!(health.is_empty());
    assert_eq!(health.span_count, 0);
    assert_eq!(health.commit_count, 0);
    assert_eq!(health.backfill_watermark, None);
}
