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

#[tokio::test]
async fn schema_is_idempotent() {
    let conn = test_conn().await;
    ensure_git_correlation_schema(&conn)
        .await
        .expect("second ensure should be a no-op");
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

    // A within-window commit is attributed to both concurrent main spans.
    let records = match_commit_to_spans("deadbeef", Some("main"), "/repo", 150, &spans, 60);
    assert_eq!(records.len(), 2);
    let ids: Vec<i64> = records.iter().filter_map(|r| r.span_id).collect();
    assert_eq!(ids, vec![1, 2]);
    assert!(records
        .iter()
        .all(|r| r.span_overlap_kind == SpanOverlapKind::WithinSpan));
    assert_eq!(records[0].session_id, "s1");
    assert_eq!(records[0].worktree.as_deref(), Some("/repo"));

    // A just-past-the-edge commit lands in the extended window only.
    let extended = match_commit_to_spans("cafef00d", Some("main"), "/repo", 250, &spans, 60);
    assert_eq!(extended.len(), 2);
    assert!(extended
        .iter()
        .all(|r| r.span_overlap_kind == SpanOverlapKind::ExtendedWindow));

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

    // The commit is now queryable by prefix.
    let hits = sessions_for(
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
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session_id, "s1");
    assert_eq!(hits[0].span_overlap_kind, Some(SpanOverlapKind::WithinSpan));

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
