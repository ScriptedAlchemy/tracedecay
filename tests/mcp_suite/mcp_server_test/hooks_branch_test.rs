//! `hooks_branch_test` domain tests, split mechanically from `mcp_server_test.rs`.

use crate::mcp_server_test::support::*;
use serde_json::json;
use std::fs;
use tempfile::TempDir;
use tracedecay::mcp::McpServer;
use tracedecay::tracedecay::{TraceDecay, TraceDecayOpenOptions};

#[tokio::test]
async fn hook_event_workspace_context_routes_followup_graph_reads() {
    let home = TempDir::new().unwrap();
    let profile_root = home
        .path()
        .canonicalize()
        .expect("temporary home canonicalizes")
        .join(".tracedecay");
    let global_db_path = profile_root.join("global.db");
    let options = TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: Some(global_db_path.clone()),
    };

    let active_dir = TempDir::new().unwrap();
    let active_project = active_dir.path();
    fs::create_dir_all(active_project.join("src")).unwrap();
    fs::write(
        active_project.join("src/active_only.rs"),
        "fn active_only() -> i32 { 1 }\n",
    )
    .unwrap();
    let active_cg = TraceDecay::init_with_options(active_project, options.clone())
        .await
        .unwrap();
    active_cg.index_all().await.unwrap();

    let target_dir = TempDir::new().unwrap();
    let target_project = target_dir.path();
    fs::create_dir_all(target_project.join("src")).unwrap();
    fs::write(
        target_project.join("src/target_only.rs"),
        "fn target_only() -> i32 { 2 }\n",
    )
    .unwrap();
    let target_cg = TraceDecay::init_with_options(target_project, options)
        .await
        .unwrap();
    target_cg.index_all().await.unwrap();

    let active_project_id = active_cg
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .and_then(|value| tracedecay_domain::ProjectId::new(value.to_string()).ok())
        .expect("active project identity");
    let registry_db = tracedecay::application::host_admission::HostAdmissionTestRuntimeV1::project(
        &profile_root,
        active_project,
        active_project_id,
    )
    .await
    .expect("registered project runtime opens");
    registry_db
        .upsert_code_project("proj_hook_active", active_project, None, None, Some("main"))
        .await
        .expect("active project registers");
    registry_db
        .upsert_code_project("proj_hook_target", target_project, None, None, Some("main"))
        .await
        .expect("target project registers");
    let server =
        McpServer::new_with_host_admission_test_runtime_for_test(active_cg, None, registry_db)
            .await;

    let responses = run_server_with_messages(
        server.clone(),
        vec![
            jsonrpc_notification_with_params(
                "tracedecay/hookEvent",
                json!({
                    "agent": "codex",
                    "event": "workspaceOpen",
                    "cwd": target_project.join("src").to_string_lossy()
                }),
            ),
            jsonrpc_request(
                json!(1),
                "tools/call",
                json!({
                    "name": "tracedecay_files",
                    "arguments": {}
                }),
            ),
        ],
    )
    .await;

    let response = response_with_id(&responses, json!(1));
    let text = extract_tool_text(&response["result"]);
    assert!(
        text.contains("target_only.rs"),
        "hook workspace context should route graph reads to target project, got: {text}; response: {response}"
    );
    assert!(
        !text.contains("active_only.rs"),
        "ambient hook route should not fall back to active project when the hook cwd resolves"
    );

    let responses = run_server_with_messages(
        server,
        vec![jsonrpc_request(
            json!(2),
            "tools/call",
            json!({
                "name": "tracedecay_files",
                "arguments": {}
            }),
        )],
    )
    .await;

    let response = response_with_id(&responses, json!(2));
    let text = extract_tool_text(&response["result"]);
    assert!(
        text.contains("active_only.rs"),
        "hook workspace context must not leak across socket clients, got: {text}"
    );
    assert!(
        !text.contains("target_only.rs"),
        "new socket client without a hook should use the active project"
    );
}

#[tokio::test]
async fn tool_calls_reopen_branch_db_after_mid_session_checkout() {
    let _branch_lock = BRANCH_DRIFT_TEST_LOCK.lock().await;
    let (_dir, project, server) = setup_branch_drift_fixture().await;

    // While on main, the feature-only symbol must be invisible.
    let resp = search_via_transport(server.clone(), 1, "feature_only").await;
    assert!(resp["error"].is_null(), "search on main should not error");
    assert!(
        !resp["result"]["content"]
            .to_string()
            .contains("feature_only"),
        "main's DB must not contain the feature-only symbol"
    );

    // Mid-session checkout. The next tool call must detect the drift,
    // reopen onto feature's DB, and serve the feature-only symbol.
    git(&project, &["checkout", "feature"]);
    let resp = search_via_transport(server.clone(), 2, "feature_only").await;
    assert!(
        resp["error"].is_null(),
        "search after checkout should not error: {resp}"
    );
    assert!(
        resp["result"]["content"]
            .to_string()
            .contains("feature_only"),
        "after the checkout, reads must serve the feature branch's DB: {resp}"
    );

    let cg_now = server.cg().await;
    assert_eq!(
        cg_now.serving_branch(),
        Some("feature"),
        "the served instance must have been swapped onto the live branch"
    );
    assert!(
        !cg_now.branch_drifted(),
        "drift must be cleared after the reopen"
    );
}

#[tokio::test]
async fn cross_branch_tools_keep_using_explicit_branch_dbs_after_drift_reopen() {
    let _branch_lock = BRANCH_DRIFT_TEST_LOCK.lock().await;
    let (_dir, project, server) = setup_branch_drift_fixture().await;

    git(&project, &["checkout", "feature"]);

    let warm = search_via_transport(server.clone(), 1, "feature_only").await;
    assert!(
        warm["error"].is_null(),
        "drift warm-up search should succeed: {warm}"
    );
    assert!(
        warm["result"]["content"]
            .to_string()
            .contains("feature_only"),
        "warm-up search should reopen the server onto the feature DB: {warm}"
    );

    let main_search = tool_call_via_transport(
        server.clone(),
        2,
        "tracedecay_branch_search",
        json!({"branch": "main", "query": "feature_only", "limit": 10}),
    )
    .await;
    assert!(
        main_search["error"].is_null(),
        "explicit main branch search should not error after drift: {main_search}"
    );
    let main_search_text = main_search["result"]["content"][0]["text"]
        .as_str()
        .expect("branch_search should return text content");
    assert!(
        !main_search_text.contains("feature_only"),
        "explicit main branch search must ignore the live feature branch DB: {main_search_text}"
    );
}

/// Proves live span recording from hook route notifications and the
/// commit-attribution sweep at ingest time:
///  (1) a hook notification with route metadata creates a span row in the
///      resolved project's `sessions.db`;
///  (2) a mid-session branch switch creates a second span row;
///  (3) a commit made inside the feature span is attributed to the session by
///      the ingest sweep.
#[tokio::test]
async fn hook_route_records_spans_and_ingest_attributes_commits() {
    use tracedecay::sessions::git_correlation::{
        CommitRelationFilter, GitRefFilter, SessionsForQuery, SpanOverlapKind,
    };

    let (_env, project) = crate::common::IsolatedEnv::acquire().await;
    let worktree = project.with_file_name("feature-worktree");

    // Real repo on `main`, plus a linked worktree checked out on `feature`.
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn marker() {}\n").unwrap();
    fs::write(project.join(".gitignore"), ".tracedecay/\n").unwrap();
    git(&project, &["init", "-b", "main"]);
    git(&project, &["config", "user.email", "test@test.com"]);
    git(&project, &["config", "user.name", "Test"]);
    git(&project, &["add", "."]);
    git(&project, &["commit", "-m", "initial"]);
    git(
        &project,
        &[
            "worktree",
            "add",
            worktree.to_string_lossy().as_ref(),
            "-b",
            "feature",
        ],
    );

    let cg = crate::fixture::init_project_from_template(&project)
        .await
        .unwrap();
    cg.index_all().await.unwrap();
    let project_root = cg.project_root().to_path_buf();

    // Register with the git common dir so the linked-worktree hook route (a
    // sibling path the parent-alias walk never reaches) resolves back to this
    // project by git-common-dir identity.
    let git_common_dir = tracedecay::worktree::git_common_dir(&project_root);
    let project_id = cg
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .and_then(|value| tracedecay_domain::ProjectId::new(value.to_string()).ok())
        .expect("project identity");
    let profile_root = tracedecay::storage::default_profile_root().unwrap();
    let registry = tracedecay::application::host_admission::HostAdmissionTestRuntimeV1::project(
        &profile_root,
        &project_root,
        project_id.clone(),
    )
    .await
    .unwrap();
    registry
        .upsert_code_project(
            "proj_live",
            &project_root,
            git_common_dir.as_deref(),
            None,
            Some("main"),
        )
        .await
        .unwrap();
    let server = McpServer::new_with_host_admission_test_runtime_for_test(cg, None, registry).await;

    let session_id = "sess-live";
    let main_worktree = project_root.to_string_lossy().to_string();
    let feature_worktree =
        tracedecay::sessions::git_correlation::normalize_worktree(&worktree.to_string_lossy());

    // (1) Hook notification on `main` in the project worktree.
    run_server_with_messages(
        server.clone(),
        vec![jsonrpc_notification_with_params(
            "tracedecay/hookEvent",
            json!({
                "agent": "claude",
                "event": "postToolUse",
                "cwd": project_root.to_string_lossy(),
                "route": {
                    "session_id": session_id,
                    "cwd": project_root.to_string_lossy(),
                    "worktree": project_root.to_string_lossy(),
                    "branch": "main",
                }
            }),
        )],
    )
    .await;
    server.ledger_writes_settled().await;

    // (2) Mid-session branch switch: same session, `feature` in the worktree.
    run_server_with_messages(
        server.clone(),
        vec![jsonrpc_notification_with_params(
            "tracedecay/hookEvent",
            json!({
                "agent": "claude",
                "event": "postToolUse",
                "cwd": worktree.to_string_lossy(),
                "route": {
                    "session_id": session_id,
                    "cwd": worktree.to_string_lossy(),
                    "worktree": worktree.to_string_lossy(),
                    "branch": "feature",
                }
            }),
        )],
    )
    .await;
    server.ledger_writes_settled().await;

    let db = tracedecay::application::host_admission::HostAdmissionTestRuntimeV1::project(
        &profile_root,
        &project_root,
        project_id,
    )
    .await
    .expect("reopen registered project runtime");

    // Span on main exists (scenario 1).
    let on_main = db
        .git_sessions_for_for_test(
            &SessionsForQuery {
                git_ref: GitRefFilter::Branch("main".to_string()),
                since: None,
                until: None,
                limit: 10,
            },
            CommitRelationFilter::Produced,
        )
        .await
        .unwrap();
    assert!(
        on_main.iter().any(|hit| hit.session_id == session_id),
        "main span should exist: {on_main:?}"
    );

    // A distinct span on feature exists (scenario 2: branch switch).
    let on_feature = db
        .git_sessions_for_for_test(
            &SessionsForQuery {
                git_ref: GitRefFilter::Branch("feature".to_string()),
                since: None,
                until: None,
                limit: 10,
            },
            CommitRelationFilter::Produced,
        )
        .await
        .unwrap();
    assert!(
        on_feature.iter().any(|hit| hit.session_id == session_id),
        "feature span should exist after branch switch: {on_feature:?}"
    );

    // Both branches recorded distinct worktrees for the same session.
    assert_ne!(main_worktree, feature_worktree);

    // (3) Make a commit on `feature` inside the live span window, then run the
    // ingest sweep and confirm it is attributed to the session.
    fs::write(
        worktree.join("src/lib.rs"),
        "pub fn marker() { /* edit */ }\n",
    )
    .unwrap();
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "-m", "feature change"]);
    let sha = git_capture(&worktree, &["rev-parse", "HEAD"]);

    db.run_incremental_git_backfill_for_test(
        &tracedecay::sessions::git_correlation::SystemGit,
        tracedecay::sessions::git_correlation::DEFAULT_AUTO_BACKFILL_SESSIONS_PER_PASS,
    )
    .await
    .unwrap();

    let by_commit = db
        .git_sessions_for_for_test(
            &SessionsForQuery {
                git_ref: GitRefFilter::Commit(sha.clone()),
                since: None,
                until: None,
                limit: 10,
            },
            CommitRelationFilter::Observed,
        )
        .await
        .unwrap();
    assert_eq!(
        by_commit.len(),
        1,
        "commit {sha} should be attributed once: {by_commit:?}"
    );
    assert_eq!(by_commit[0].session_id, session_id);
    assert_eq!(by_commit[0].commit_sha.as_deref(), Some(sha.as_str()));
    assert_eq!(by_commit[0].branch.as_deref(), Some("feature"));
    assert!(matches!(
        by_commit[0].span_overlap_kind,
        Some(SpanOverlapKind::WithinSpan) | Some(SpanOverlapKind::ExtendedWindow)
    ));

    drop(db);
}
