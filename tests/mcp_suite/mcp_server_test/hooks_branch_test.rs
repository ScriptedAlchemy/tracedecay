use crate::mcp_server_test::support::*;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;
use tracedecay::application::host_admission::{
    HostAdmissionTestRuntimeV1, ProjectScopedTestRuntimeV1,
};
use tracedecay::mcp::McpServer;
use tracedecay::tracedecay::{TraceDecay, TraceDecayOpenOptions};

/// Two indexed projects inside one throwaway profile: the project the server
/// serves by default ("active"), and the project a hook route must select
/// instead ("target"). Each carries a source file named after it, so a routed
/// read is only correct when it contains one marker and not the other.
struct RoutedProjects {
    _home: TempDir,
    profile_root: PathBuf,
    active_dir: TempDir,
    target_dir: TempDir,
    active: TraceDecay,
    target: TraceDecay,
    active_project_id: String,
    target_project_id: String,
}

async fn routed_projects() -> RoutedProjects {
    let home = TempDir::new().unwrap();
    let profile_root = home
        .path()
        .canonicalize()
        .expect("temporary home canonicalizes")
        .join(".tracedecay");
    let options = TraceDecayOpenOptions {
        profile_root: Some(profile_root.clone()),
        global_db_path: Some(profile_root.join("global.db")),
    };

    let active_dir = TempDir::new().unwrap();
    fs::create_dir_all(active_dir.path().join("src")).unwrap();
    fs::write(
        active_dir.path().join("src/active_only.rs"),
        "fn active_only() -> i32 { 1 }\n",
    )
    .unwrap();
    let active = TraceDecay::init_with_options(active_dir.path(), options.clone())
        .await
        .unwrap();
    active.index_all().await.unwrap();

    let target_dir = TempDir::new().unwrap();
    fs::create_dir_all(target_dir.path().join("src")).unwrap();
    fs::write(
        target_dir.path().join("src/target_only.rs"),
        "fn target_only() -> i32 { 2 }\n",
    )
    .unwrap();
    let target = TraceDecay::init_with_options(target_dir.path(), options)
        .await
        .unwrap();
    target.index_all().await.unwrap();

    let active_project_id = active
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("active project identity");
    let target_project_id = target
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("target project identity");

    RoutedProjects {
        _home: home,
        profile_root,
        active_dir,
        target_dir,
        active,
        target,
        active_project_id,
        target_project_id,
    }
}

impl RoutedProjects {
    fn active_root(&self) -> &Path {
        self.active_dir.path()
    }

    fn target_root(&self) -> &Path {
        self.target_dir.path()
    }

    /// Registers both projects under the identities their stores already
    /// carry and returns the runtime the server uses as its registry.
    ///
    /// `TraceDecay::init` registered each store under its own identity id, so
    /// re-registering under a synthetic id would leave the root's alias
    /// pointing at a project row that owns no store — the ambiguity
    /// [`hook_route_to_ambiguously_registered_project_fails_closed`] covers
    /// deliberately.
    async fn registered_runtime(&self) -> ProjectScopedTestRuntimeV1 {
        let active_project_id = tracedecay_domain::ProjectId::new(self.active_project_id.clone())
            .expect("typed active project identity");
        let runtime = HostAdmissionTestRuntimeV1::project_scoped(
            &self.profile_root,
            self.active_root(),
            active_project_id,
        )
        .await
        .expect("registered project runtime opens");
        runtime
            .upsert_code_project(
                &self.active_project_id,
                self.active_root(),
                None,
                None,
                Some("main"),
            )
            .await
            .expect("active project registers");
        runtime
            .upsert_code_project(
                &self.target_project_id,
                self.target_root(),
                None,
                None,
                Some("main"),
            )
            .await
            .expect("target project registers");
        runtime
    }
}

/// A `workspaceOpen` notification naming the workspace `cwd` a host just
/// opened, with no route identity — it can only steer follow-up calls on the
/// same connection.
fn workspace_open(cwd: &Path) -> String {
    jsonrpc_notification_with_params(
        "tracedecay/hookEvent",
        json!({
            "agent": "codex",
            "event": "workspaceOpen",
            "cwd": cwd.to_string_lossy()
        }),
    )
}

/// A `workspaceOpen` carrying route identity, the shape every real host sends.
/// The session id is what lets the route survive to the agent's own socket.
fn workspace_open_for_session(cwd: &Path, session_id: &str) -> String {
    jsonrpc_notification_with_params(
        "tracedecay/hookEvent",
        json!({
            "agent": "codex",
            "event": "workspaceOpen",
            "cwd": cwd.to_string_lossy(),
            "route": {
                "session_id": session_id,
                "cwd": cwd.to_string_lossy(),
                "worktree": cwd.to_string_lossy(),
            }
        }),
    )
}

fn files_call(id: i64) -> String {
    jsonrpc_request(
        json!(id),
        "tools/call",
        json!({ "name": "tracedecay_files", "arguments": {} }),
    )
}

fn files_call_for_session(id: i64, session_id: &str) -> String {
    jsonrpc_request(
        json!(id),
        "tools/call",
        json!({
            "name": "tracedecay_files",
            "arguments": { "session_id": session_id }
        }),
    )
}

/// Asserts a routed read refused rather than answering, and that it did not
/// hand back the active project's files as if the route had resolved.
fn assert_route_failed_closed(response: &Value, label: &str, expected_detail: &str) {
    assert!(
        response["result"].is_null(),
        "{label} must not return a tool result: {response}"
    );
    let message = response["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("{label} must return a JSON-RPC error: {response}"));
    assert!(
        message.contains(expected_detail),
        "{label} must report `{expected_detail}`: {response}"
    );
}

/// Asserts a routed read served exactly one project: `present` appears,
/// `absent` does not, and the call itself succeeded with real text.
fn assert_served_project(response: &Value, label: &str, present: &str, absent: &str) {
    let text = successful_tool_text(response, label);
    assert!(
        text.contains(present),
        "{label} must serve the routed project ({present} missing): {response}"
    );
    assert!(
        !text.contains(absent),
        "{label} must not serve the other project ({absent} present): {response}"
    );
}

#[tokio::test]
async fn hook_event_workspace_context_routes_followup_graph_reads() {
    let projects = routed_projects().await;
    let target_workspace = projects.target_root().to_path_buf();
    let registry_db = projects.registered_runtime().await;
    // Tools that name another project reach it only through the retained
    // resolver, so the target project's graph must be handed to the server
    // (the daemon equivalent: the target project is mounted).
    let server = McpServer::new_with_retained_test_graphs_for_test(
        projects.active,
        None,
        registry_db,
        vec![Arc::new(projects.target)],
    )
    .await
    .expect("registered test server");

    let responses = run_server_with_messages(
        server.clone(),
        vec![workspace_open(&target_workspace), files_call(1)],
    )
    .await;
    assert_served_project(
        &response_with_id(&responses, json!(1)),
        "hook-routed files read",
        "target_only.rs",
        "active_only.rs",
    );

    let responses = run_server_with_messages(server, vec![files_call(2)]).await;
    assert_served_project(
        &response_with_id(&responses, json!(2)),
        "unrouted files read on a fresh client",
        "active_only.rs",
        "target_only.rs",
    );
}

/// A hook arriving from a linked worktree must reach the registered checkout
/// through git-common-dir identity, and must carry that route to the agent's
/// own socket.
///
/// This is the production shape the single-connection test above cannot
/// reproduce: the host's hook client and the agent's tool client are separate
/// sockets correlated only by session id, and a linked worktree lives at a
/// sibling path, so the parent-directory alias walk from it never reaches the
/// registered project. Fall back to alias-only resolution and this read
/// silently serves the active project.
#[tokio::test]
async fn linked_worktree_hook_route_reaches_target_across_client_sockets() {
    let projects = routed_projects().await;
    let target_project = projects.target_root().to_path_buf();

    // Turn the target into a real repository and attach a linked worktree at a
    // sibling path outside every ancestor of the registered checkout.
    fs::write(target_project.join(".gitignore"), ".tracedecay/\n").unwrap();
    git(&target_project, &["init", "-b", "main"]);
    git(&target_project, &["config", "user.email", "test@test.com"]);
    git(&target_project, &["config", "user.name", "Test"]);
    git(&target_project, &["add", "."]);
    git(&target_project, &["commit", "-m", "initial"]);
    let worktree_parent = TempDir::new().unwrap();
    let linked_worktree = worktree_parent.path().join("linked-feature");
    git(
        &target_project,
        &[
            "worktree",
            "add",
            linked_worktree.to_string_lossy().as_ref(),
            "-b",
            "feature",
        ],
    );
    let git_common_dir = tracedecay::worktree::git_common_dir(&target_project);
    assert!(
        git_common_dir.is_some(),
        "the linked-worktree fixture needs a resolvable git common dir"
    );

    let registry_db = projects.registered_runtime().await;
    // Re-register the target with its git common dir. The repository was
    // created after the store, so there is no identity marker to fall back on:
    // this alias is the only bridge from the linked worktree back to the
    // registered checkout.
    registry_db
        .upsert_code_project(
            &projects.target_project_id,
            &target_project,
            git_common_dir.as_deref(),
            None,
            Some("main"),
        )
        .await
        .expect("target project registers with its git common dir");
    let server = McpServer::new_with_retained_test_graphs_for_test(
        projects.active,
        None,
        registry_db,
        vec![Arc::new(projects.target)],
    )
    .await
    .expect("registered test server");

    let session_id = "sess-linked-worktree";
    // The host's hook socket: closes as soon as the notification is delivered.
    run_client_connection_with_messages(
        server.clone(),
        vec![workspace_open_for_session(&linked_worktree, session_id)],
    )
    .await;

    // The agent's socket: a different connection whose only tie to the hook
    // above is the session id it reports.
    let responses =
        run_client_connection_with_messages(server, vec![files_call_for_session(1, session_id)])
            .await;
    assert_served_project(
        &response_with_id(&responses, json!(1)),
        "linked-worktree hook route on a separate tool socket",
        "target_only.rs",
        "active_only.rs",
    );
}

/// A route that resolves to a registered project the daemon has not mounted
/// must surface a typed failure. Serving the active project instead would
/// present the wrong project's files as a successful answer.
#[tokio::test]
async fn hook_route_to_registered_but_unmounted_project_fails_closed() {
    let projects = routed_projects().await;
    let target_workspace = projects.target_root().to_path_buf();
    let registry_db = projects.registered_runtime().await;
    // No retained graph for the target: registered, but not mounted.
    let server = McpServer::new_with_host_admission_test_runtime_for_test(
        projects.active,
        None,
        registry_db,
    )
    .await
    .expect("registered test server");

    let session_id = "sess-unmounted-target";
    let responses = run_server_with_messages(
        server,
        vec![
            workspace_open_for_session(&target_workspace, session_id),
            files_call_for_session(1, session_id),
        ],
    )
    .await;

    assert_route_failed_closed(
        &response_with_id(&responses, json!(1)),
        "read routed to a registered but unmounted project",
        "not mounted",
    );
}

/// Registering one root under a second project id leaves that root's alias
/// pointing at a project row that owns no store. Selector resolution must
/// refuse rather than pick a registration, and must not quietly answer from
/// the active project.
#[tokio::test]
async fn hook_route_to_ambiguously_registered_project_fails_closed() {
    let projects = routed_projects().await;
    let target_root = projects.target_root().to_path_buf();
    let registry_db = projects.registered_runtime().await;
    registry_db
        .upsert_code_project(
            "proj_duplicate_target",
            &target_root,
            None,
            None,
            Some("main"),
        )
        .await
        .expect("duplicate registration of the target root is accepted by the registry");
    // The target graph *is* mounted, so a failure here can only come from the
    // ambiguous registration.
    let server = McpServer::new_with_retained_test_graphs_for_test(
        projects.active,
        None,
        registry_db,
        vec![Arc::new(projects.target)],
    )
    .await
    .expect("registered test server");

    let session_id = "sess-ambiguous-target";
    let responses = run_server_with_messages(
        server,
        vec![
            workspace_open_for_session(&target_root, session_id),
            files_call_for_session(1, session_id),
        ],
    )
    .await;

    assert_route_failed_closed(
        &response_with_id(&responses, json!(1)),
        "read routed to an ambiguously registered project",
        "not found for selector",
    );
}

/// An identity-free hook whose workspace belongs to no registered project
/// leaves follow-up reads on the active project. That is the contract, and it
/// is only sound because the answer is the active project's own data — never
/// another project's, and never the active project's presented as if the route
/// had resolved somewhere else.
#[tokio::test]
async fn hook_route_from_unregistered_cwd_serves_the_active_project() {
    let projects = routed_projects().await;
    let unregistered = TempDir::new().unwrap();
    let unregistered_workspace = unregistered.path().to_path_buf();
    let registry_db = projects.registered_runtime().await;
    let server = McpServer::new_with_retained_test_graphs_for_test(
        projects.active,
        None,
        registry_db,
        vec![Arc::new(projects.target)],
    )
    .await
    .expect("registered test server");

    let responses = run_server_with_messages(
        server,
        vec![workspace_open(&unregistered_workspace), files_call(1)],
    )
    .await;
    assert_served_project(
        &response_with_id(&responses, json!(1)),
        "unresolvable hook route",
        "active_only.rs",
        "target_only.rs",
    );
}

/// Hosts report the workspace path the user opened, which is often a symlink
/// to the checkout. Route resolution canonicalizes before matching aliases, so
/// the symlinked spelling must reach the same registered project rather than
/// missing and falling back to the active one.
#[cfg(unix)]
#[tokio::test]
async fn hook_route_through_symlinked_workspace_reaches_target() {
    let projects = routed_projects().await;
    let link_parent = TempDir::new().unwrap();
    let link = link_parent.path().join("target-link");
    std::os::unix::fs::symlink(projects.target_root(), &link)
        .expect("symlink alias for the target checkout");
    let registry_db = projects.registered_runtime().await;
    let server = McpServer::new_with_retained_test_graphs_for_test(
        projects.active,
        None,
        registry_db,
        vec![Arc::new(projects.target)],
    )
    .await
    .expect("registered test server");

    let responses =
        run_server_with_messages(server, vec![workspace_open(&link), files_call(1)]).await;
    assert_served_project(
        &response_with_id(&responses, json!(1)),
        "symlinked hook workspace",
        "target_only.rs",
        "active_only.rs",
    );
}

#[tokio::test]
async fn tool_calls_reopen_branch_db_after_mid_session_checkout() {
    let fixture = setup_branch_drift_fixture().await;
    let project = fixture.project.clone();
    let server = Arc::clone(&fixture.server);

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
    let fixture = setup_branch_drift_fixture().await;
    let project = fixture.project.clone();
    let server = Arc::clone(&fixture.server);

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
    let registry =
        tracedecay::application::host_admission::HostAdmissionTestRuntimeV1::project_scoped(
            &profile_root,
            &project_root,
            project_id.clone(),
        )
        .await
        .unwrap();
    // Register under the project's real identity-marker id: identity
    // resolution for the linked-worktree route reads the marker first, so a
    // synthetic id would shadow the row the marker points at.
    registry
        .upsert_code_project(
            project_id.as_str(),
            &project_root,
            git_common_dir.as_deref(),
            None,
            Some("main"),
        )
        .await
        .unwrap();
    // Path-selector resolution (the authority the hook route now shares with
    // tool reads) resolves identity to a *store*, not just a project row; a
    // template-seeded fixture skips enrollment, so register the store the
    // graph actually opened.
    registry
        .upsert_store_instance(tracedecay::global_db::StoreInstanceUpsert {
            store_id: format!("store_{}", project_id.as_str()),
            project_id: project_id.as_str().to_string(),
            store_kind: "code_project".to_string(),
            storage_mode: "profile_sharded".to_string(),
            store_relpath: format!("projects/{}", project_id.as_str()),
            manifest_relpath: None,
            last_verified_at: None,
            last_write_at: None,
        })
        .await
        .expect("project store registers");
    let server = McpServer::new_with_host_admission_test_runtime_for_test(cg, None, registry)
        .await
        .expect("registered test server");

    let session_id = "sess-live";

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
    let main_span = on_main
        .iter()
        .find(|hit| hit.session_id == session_id)
        .unwrap_or_else(|| panic!("main span should exist: {on_main:?}"));

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
    let feature_span = on_feature
        .iter()
        .find(|hit| hit.session_id == session_id)
        .unwrap_or_else(|| panic!("feature span should exist after branch switch: {on_feature:?}"));

    // The two spans must be two real observations of the same session in two
    // checkouts. Comparing the recorded worktrees (rather than the paths the
    // test itself sent) is what makes this fail if span recording is skipped
    // and only one row is ever written.
    assert_ne!(
        main_span.worktree, feature_span.worktree,
        "each checkout must record its own span worktree: {main_span:?} / {feature_span:?}"
    );
    assert!(
        feature_span
            .worktree
            .as_deref()
            .is_some_and(|recorded| recorded.ends_with("feature-worktree")),
        "the feature span must record the linked worktree, not the main checkout: {feature_span:?}"
    );

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
