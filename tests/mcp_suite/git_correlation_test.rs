//! End-to-end tests for the session↔git correlation query surface:
//! `tracedecay_sessions_for` plus the git-scope filters on
//! `tracedecay_message_search` / `tracedecay_lcm_grep`, driven through the
//! real `handle_tool_call` dispatch against a temp project with a linked git
//! worktree and a seeded `sessions.db`.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

use tracedecay::global_db::GlobalDb;
use tracedecay::sessions::git_correlation::{
    CommitEvidence, CommitRelation, CommitSessionRecord, DEFAULT_SPAN_MERGE_GAP_SECS,
    SpanObservation, SpanOverlapKind, SpanSource,
};
use tracedecay::sessions::{SessionMessageRecord, SessionRecord};
use tracedecay::tracedecay::{TraceDecay, TraceDecayOpenOptions};

use crate::common;

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new(common::git_program())
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| panic!("git {args:?} should spawn: {e}"));
    assert!(status.success(), "git {args:?} should succeed");
}

/// Initializes a project repo (`main`) plus a linked worktree checked out on
/// `feature/session` under `base`. Returns `(project_root, worktree)`.
fn setup_linked_worktree_under(base: &Path) -> (PathBuf, PathBuf) {
    let project_root = base.join("project");
    let worktree_root = base.join("session-worktree");
    std::fs::create_dir_all(project_root.join("src"))
        .unwrap_or_else(|e| panic!("project dirs: {e}"));
    std::fs::write(project_root.join("src/lib.rs"), "pub fn marker() {}\n")
        .unwrap_or_else(|e| panic!("source: {e}"));
    run_git(&project_root, &["init", "-b", "main"]);
    run_git(&project_root, &["config", "user.email", "test@test.com"]);
    run_git(&project_root, &["config", "user.name", "Test"]);
    run_git(&project_root, &["add", "."]);
    run_git(&project_root, &["commit", "-m", "initial"]);
    let worktree_arg = worktree_root.to_string_lossy();
    run_git(
        &project_root,
        &[
            "worktree",
            "add",
            worktree_arg.as_ref(),
            "-b",
            "feature/session",
        ],
    );
    (project_root, worktree_root)
}

fn session(session_id: &str, project_key: &str, started_at: i64) -> SessionRecord {
    SessionRecord {
        provider: "claude".to_string(),
        session_id: session_id.to_string(),
        project_key: project_key.to_string(),
        project_path: project_key.to_string(),
        title: Some(format!("Session {session_id}")),
        started_at: Some(started_at),
        ended_at: None,
        transcript_path: Some(format!("{session_id}.jsonl")),
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    }
}

fn message(session_id: &str, message_id: &str, ts: i64, text: &str) -> SessionMessageRecord {
    SessionMessageRecord {
        provider: "claude".to_string(),
        message_id: message_id.to_string(),
        session_id: session_id.to_string(),
        role: "assistant".to_string(),
        timestamp: Some(ts),
        ordinal: 1,
        text: text.to_string(),
        kind: Some("message".to_string()),
        model: Some("test-model".to_string()),
        tool_names: None,
        source_path: Some(format!("{session_id}.jsonl")),
        source_offset: Some(0),
        metadata_json: None,
    }
}

fn span(session_id: &str, branch: Option<&str>, worktree: &str, ts: i64) -> SpanObservation {
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

async fn record_span(db: &GlobalDb, observation: &SpanObservation) {
    db.git_record_span_observation(observation, DEFAULT_SPAN_MERGE_GAP_SECS)
        .await
        .unwrap_or_else(|e| panic!("record span: {e}"));
}

fn extract_json(result: &tracedecay::mcp::ToolResult) -> Value {
    let text = result.value["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tool result should carry text content: {}", result.value));
    serde_json::from_str(text).unwrap_or_else(|e| panic!("tool result should be JSON: {e}\n{text}"))
}

async fn call(cg: &TraceDecay, tool: &str, mut args: Value) -> Value {
    if let Some(obj) = args.as_object_mut() {
        obj.entry("format".to_string())
            .or_insert_with(|| json!("json"));
    }
    let result = tracedecay::mcp::handle_tool_call(cg, tool, args, None, None)
        .await
        .unwrap_or_else(|e| panic!("{tool} should succeed: {e}"));
    extract_json(&result)
}

fn session_ids(payload: &Value) -> Vec<String> {
    payload["results"]
        .as_array()
        .unwrap_or_else(|| panic!("results should be an array: {payload}"))
        .iter()
        .map(|hit| hit["session_id"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Seeds a project store with three sessions on `main`/`feature/session` plus
/// a mid-session branch switch, then drives the query surface end to end.
#[tokio::test]
async fn sessions_for_and_scoped_search_end_to_end() {
    let dir = common::tempdir_or_panic();
    #[cfg(windows)]
    let base = dir.path().to_path_buf();
    #[cfg(not(windows))]
    let base = dir.path().canonicalize().unwrap();
    let (project_root, worktree_root) = setup_linked_worktree_under(&base);

    let profile_root = base.join("profile");
    std::fs::create_dir_all(&profile_root).unwrap_or_else(|e| panic!("create profile root: {e}"));
    let profile_root = profile_root
        .canonicalize()
        .unwrap_or_else(|e| panic!("canonicalize profile root: {e}"));
    let cg = TraceDecay::init_with_options(
        &project_root,
        TraceDecayOpenOptions {
            profile_root: Some(profile_root),
            global_db_path: Some(base.join("global.db")),
        },
    )
    .await
    .unwrap_or_else(|e| panic!("init project: {e}"));
    let project_key = cg.project_root().to_string_lossy().to_string();
    let main_worktree = project_root.to_string_lossy().to_string();
    let feature_worktree = worktree_root.to_string_lossy().to_string();

    let db_path = cg.store_layout().sessions_db_path.clone();
    let db = GlobalDb::open_at(&db_path)
        .await
        .unwrap_or_else(|| panic!("open sessions.db"));

    // s1: two observations on main (one merged span). s2: on main in the
    // linked worktree, most recent. s3: on feature/session.
    for record in [
        session("s1", &project_key, 1_000),
        session("s2", &project_key, 5_000),
        session("s3", &project_key, 2_000),
        session("switcher", &project_key, 3_000),
    ] {
        assert!(db.upsert_session(&record).await);
    }
    for msg in [
        message("s1", "s1-m1", 1_050, "alpha correlation evidence on main"),
        message(
            "s2",
            "s2-m1",
            5_050,
            "beta correlation evidence in worktree",
        ),
        message(
            "s3",
            "s3-m1",
            2_050,
            "gamma correlation evidence on feature",
        ),
        message(
            "switcher",
            "sw-m1",
            3_050,
            "delta correlation evidence switching",
        ),
        message(
            "switcher",
            "sw-m2",
            9_000,
            "delta correlation evidence after switch",
        ),
    ] {
        assert!(db.upsert_session_message(&msg).await);
    }

    record_span(&db, &span("s1", Some("main"), &main_worktree, 1_000)).await;
    record_span(&db, &span("s1", Some("main"), &main_worktree, 1_200)).await;
    record_span(&db, &span("s2", Some("main"), &feature_worktree, 5_000)).await;
    record_span(
        &db,
        &span("s3", Some("feature/session"), &feature_worktree, 2_000),
    )
    .await;
    // Mid-session branch switch: `switcher` was on main, then feature.
    record_span(&db, &span("switcher", Some("main"), &main_worktree, 3_000)).await;
    record_span(
        &db,
        &span(
            "switcher",
            Some("feature/session"),
            &feature_worktree,
            8_800,
        ),
    )
    .await;

    // A commit made inside the `switcher` feature span is attributed to
    // feature/session only.
    assert!(
        db.git_upsert_commit_session(&CommitSessionRecord {
            commit_sha: "abcdef1234567890abcdef1234567890abcdef12".to_string(),
            provider: "claude".to_string(),
            session_id: "switcher".to_string(),
            branch: Some("feature/session".to_string()),
            worktree: Some(feature_worktree.clone()),
            committed_at: 8_850,
            span_overlap_kind: SpanOverlapKind::Direct,
            span_id: None,
            relation: CommitRelation::Produced,
            evidence: CommitEvidence::ToolResult,
            confidence: 100,
            evidence_message_id: Some("commit-tool-result".to_string()),
        })
        .await
        .unwrap()
    );
    assert!(
        db.git_upsert_commit_session(&CommitSessionRecord {
            commit_sha: "abcdef1234567890abcdef1234567890abcdef12".to_string(),
            provider: "claude".to_string(),
            session_id: "s2".to_string(),
            branch: Some("feature/session".to_string()),
            worktree: Some(feature_worktree.clone()),
            committed_at: 8_850,
            span_overlap_kind: SpanOverlapKind::WithinSpan,
            span_id: None,
            relation: CommitRelation::Observed,
            evidence: CommitEvidence::TimeOverlap,
            confidence: 20,
            evidence_message_id: None,
        })
        .await
        .unwrap()
    );

    // (a) sessions_for by branch: main matches s1, s2, switcher, ordered by
    // recency (most recent span first: switcher@... vs s2@5000 vs s1@1200).
    let by_main = call(
        &cg,
        "tracedecay_sessions_for",
        json!({ "git_ref": "branch", "value": "main" }),
    )
    .await;
    assert_eq!(by_main["git_ref"], "branch");
    let main_ids = session_ids(&by_main);
    assert!(main_ids.contains(&"s1".to_string()), "{by_main}");
    assert!(main_ids.contains(&"s2".to_string()), "{by_main}");
    assert!(main_ids.contains(&"switcher".to_string()), "{by_main}");
    assert!(!main_ids.contains(&"s3".to_string()), "{by_main}");
    // Recency ordering: s2 (last_ts 5000) precedes s1 (last_ts 1200).
    let s2_pos = main_ids.iter().position(|id| id == "s2").unwrap();
    let s1_pos = main_ids.iter().position(|id| id == "s1").unwrap();
    assert!(s2_pos < s1_pos, "most recent first: {main_ids:?}");

    // (b) sessions_for by worktree path, trailing slash normalized.
    let by_worktree = call(
        &cg,
        "tracedecay_sessions_for",
        json!({ "git_ref": "worktree", "value": format!("{feature_worktree}/") }),
    )
    .await;
    let wt_ids = session_ids(&by_worktree);
    assert!(wt_ids.contains(&"s2".to_string()), "{by_worktree}");
    assert!(wt_ids.contains(&"s3".to_string()), "{by_worktree}");
    assert!(wt_ids.contains(&"switcher".to_string()), "{by_worktree}");
    assert!(!wt_ids.contains(&"s1".to_string()), "{by_worktree}");

    // (c) sessions_for by full and 8-char commit sha.
    for sha in ["abcdef1234567890abcdef1234567890abcdef12", "abcdef12"] {
        let by_commit = call(
            &cg,
            "tracedecay_sessions_for",
            json!({ "git_ref": "commit", "value": sha }),
        )
        .await;
        let commit_ids = session_ids(&by_commit);
        assert_eq!(
            commit_ids,
            vec!["switcher".to_string()],
            "sha {sha}: {by_commit}"
        );
        assert_eq!(
            by_commit["results"][0]["commit_sha"],
            "abcdef1234567890abcdef1234567890abcdef12"
        );
        assert_eq!(by_commit["results"][0]["relation"], "produced");
    }
    let observed_commit = call(
        &cg,
        "tracedecay_sessions_for",
        json!({ "git_ref": "commit", "value": "abcdef12", "relation": "observed" }),
    )
    .await;
    assert_eq!(session_ids(&observed_commit), vec!["s2".to_string()]);
    assert_eq!(observed_commit["results"][0]["evidence"], "time_overlap");
    let all_commit = call(
        &cg,
        "tracedecay_sessions_for",
        json!({ "git_ref": "commit", "value": "abcdef12", "relation": "all" }),
    )
    .await;
    assert_eq!(all_commit["count"], 2);

    // (d) mid-session branch switch: sessions_for(main) and (feature) both
    // return `switcher`.
    let by_feature = call(
        &cg,
        "tracedecay_sessions_for",
        json!({ "git_ref": "branch", "value": "feature/session" }),
    )
    .await;
    assert!(
        session_ids(&by_feature).contains(&"switcher".to_string()),
        "{by_feature}"
    );
    assert!(main_ids.contains(&"switcher".to_string()));

    // message_search with branch=main and branch=feature/session both return
    // the switcher's messages; branch=absent returns none.
    for branch in ["main", "feature/session"] {
        let scoped = call(
            &cg,
            "tracedecay_message_search",
            json!({
                "query": "delta correlation",
                "provider": "claude",
                "catch_up": false,
                "branch": branch,
            }),
        )
        .await;
        assert_eq!(scoped["git_filter_applied"], true, "{scoped}");
        assert_eq!(scoped["git_filter"]["branch"], branch);
        let ids = session_ids_from_search(&scoped);
        assert!(
            ids.contains(&"switcher".to_string()),
            "branch {branch}: {scoped}"
        );
    }
    let unmatched = call(
        &cg,
        "tracedecay_message_search",
        json!({
            "query": "delta correlation",
            "provider": "claude",
            "catch_up": false,
            "branch": "does-not-exist",
        }),
    )
    .await;
    assert_eq!(unmatched["git_filter_applied"], true, "{unmatched}");
    assert_eq!(unmatched["count"], 0, "{unmatched}");

    // (e) since/until narrowing on sessions_for(main): only s2's span
    // ([5000,5000]) overlaps [4000, 6000].
    let scoped_time = call(
        &cg,
        "tracedecay_sessions_for",
        json!({ "git_ref": "branch", "value": "main", "since": 4_000, "until": 6_000 }),
    )
    .await;
    assert_eq!(
        session_ids(&scoped_time),
        vec!["s2".to_string()],
        "{scoped_time}"
    );

    // (f) commit-scoped message_search returns only the switcher.
    let by_commit_msg = call(
        &cg,
        "tracedecay_message_search",
        json!({
            "query": "delta correlation",
            "provider": "claude",
            "catch_up": false,
            "commit": "abcdef12",
        }),
    )
    .await;
    assert_eq!(by_commit_msg["git_filter_applied"], true, "{by_commit_msg}");
    let commit_msg_sessions = unique_sorted(session_ids_from_search(&by_commit_msg));
    assert_eq!(
        commit_msg_sessions,
        vec!["switcher".to_string()],
        "{by_commit_msg}"
    );

    // worktree-scoped message_search: alpha (s1 on main worktree) excluded when
    // scoping to the feature worktree; beta (s2) included.
    let by_worktree_msg = call(
        &cg,
        "tracedecay_message_search",
        json!({
            "query": "correlation evidence",
            "provider": "claude",
            "catch_up": false,
            "worktree": feature_worktree,
        }),
    )
    .await;
    let wt_msg_ids = session_ids_from_search(&by_worktree_msg);
    assert!(wt_msg_ids.contains(&"s2".to_string()), "{by_worktree_msg}");
    assert!(!wt_msg_ids.contains(&"s1".to_string()), "{by_worktree_msg}");

    // (f) lcm_grep with a branch filter: the raw messages are written to
    // lcm_raw_messages by upsert_session_message, so a feature-scoped grep
    // surfaces only the switcher's hits.
    let grep = call(
        &cg,
        "tracedecay_lcm_grep",
        json!({
            "query": "delta",
            "provider": "claude",
            "branch": "feature/session",
        }),
    )
    .await;
    assert_eq!(grep["git_filter_applied"], true, "{grep}");
    assert_eq!(grep["git_filter"]["branch"], "feature/session");
    let grep_sessions = grep["hits"]
        .as_array()
        .unwrap_or_else(|| panic!("hits should be an array: {grep}"))
        .iter()
        .map(|hit| hit["session_id"].as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    assert!(
        grep_sessions.iter().all(|id| id == "switcher"),
        "feature-scoped grep should only surface the switcher: {grep}"
    );
    assert!(!grep_sessions.is_empty(), "{grep}");

    // A grep scoped to a branch nothing ran on returns no hits.
    let grep_none = call(
        &cg,
        "tracedecay_lcm_grep",
        json!({
            "query": "delta",
            "provider": "claude",
            "branch": "does-not-exist",
        }),
    )
    .await;
    assert_eq!(grep_none["git_filter_applied"], true, "{grep_none}");
    assert_eq!(grep_none["count"], 0, "{grep_none}");

    drop(db);
    cg.close();
}

/// An empty correlation index (sessions present, but no spans recorded) must be
/// reported distinctly from "no sessions matched", both through
/// `tracedecay_sessions_for` and the `tracedecay_diagnostics` health block, so
/// callers never mistake an unpopulated index for an answered-and-empty query.
#[tokio::test]
async fn sessions_for_and_diagnostics_flag_empty_correlation_index() {
    let dir = common::tempdir_or_panic();
    #[cfg(windows)]
    let base = dir.path().to_path_buf();
    #[cfg(not(windows))]
    let base = dir.path().canonicalize().unwrap();
    let (project_root, _worktree_root) = setup_linked_worktree_under(&base);

    let profile_root = base.join("profile");
    std::fs::create_dir_all(&profile_root).unwrap_or_else(|e| panic!("create profile root: {e}"));
    let profile_root = profile_root
        .canonicalize()
        .unwrap_or_else(|e| panic!("canonicalize profile root: {e}"));
    let cg = TraceDecay::init_with_options(
        &project_root,
        TraceDecayOpenOptions {
            profile_root: Some(profile_root),
            global_db_path: Some(base.join("global.db")),
        },
    )
    .await
    .unwrap_or_else(|e| panic!("init project: {e}"));
    let project_key = cg.project_root().to_string_lossy().to_string();
    let main_worktree = project_root.to_string_lossy().to_string();

    // Seed sessions and messages but record NO git spans: the correlation
    // index exists (schema is ensured on open) yet holds nothing.
    let db_path = cg.store_layout().sessions_db_path.clone();
    let db = GlobalDb::open_at(&db_path)
        .await
        .unwrap_or_else(|| panic!("open sessions.db"));
    assert!(db.upsert_session(&session("s1", &project_key, 1_000)).await);
    assert!(
        db.upsert_session_message(&message("s1", "s1-m1", 1_050, "work on main"))
            .await
    );

    // sessions_for on an empty index: no results, explicitly flagged empty.
    let empty = call(
        &cg,
        "tracedecay_sessions_for",
        json!({ "git_ref": "branch", "value": "main" }),
    )
    .await;
    assert_eq!(empty["count"], 0, "{empty}");
    assert_eq!(empty["index_empty"], true, "{empty}");
    assert_eq!(empty["index"]["span_count"], 0, "{empty}");
    assert!(
        empty["message"]
            .as_str()
            .is_some_and(|m| m.contains("empty")),
        "empty-index message should say the index is empty: {empty}"
    );

    // diagnostics surfaces the same empty-index health.
    let diag_empty = call(&cg, "tracedecay_diagnostics", json!({})).await;
    assert_eq!(
        diag_empty["session_correlation"]["index_empty"], true,
        "{diag_empty}"
    );
    assert_eq!(
        diag_empty["session_correlation"]["span_count"], 0,
        "{diag_empty}"
    );

    // Record one span on main; the index is no longer empty.
    record_span(&db, &span("s1", Some("main"), &main_worktree, 1_000)).await;

    // A ref with no matching span now reads as "no match", not "empty index".
    let no_match = call(
        &cg,
        "tracedecay_sessions_for",
        json!({ "git_ref": "branch", "value": "does-not-exist" }),
    )
    .await;
    assert_eq!(no_match["count"], 0, "{no_match}");
    assert_eq!(no_match["index_empty"], false, "{no_match}");
    assert_eq!(no_match["index"]["spans_present"], true, "{no_match}");
    assert_eq!(no_match["index"]["span_count"], Value::Null, "{no_match}");
    assert_eq!(
        no_match["index"]["count_mode"], "presence_only",
        "{no_match}"
    );
    assert!(
        no_match["message"]
            .as_str()
            .is_some_and(|m| m.contains("no sessions matched")),
        "populated index should report no-match, not empty: {no_match}"
    );

    // diagnostics now reports a populated index.
    let diag_populated = call(&cg, "tracedecay_diagnostics", json!({})).await;
    assert_eq!(
        diag_populated["session_correlation"]["index_empty"], false,
        "{diag_populated}"
    );
    assert_eq!(
        diag_populated["session_correlation"]["span_count"], 1,
        "{diag_populated}"
    );
    assert_eq!(
        diag_populated["session_correlation"]["tables_present"], true,
        "{diag_populated}"
    );
}

fn unique_sorted(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids.dedup();
    ids
}

fn session_ids_from_search(payload: &Value) -> Vec<String> {
    payload["results"]
        .as_array()
        .unwrap_or_else(|| panic!("search results should be an array: {payload}"))
        .iter()
        .map(|hit| {
            hit["session"]["session_id"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}
