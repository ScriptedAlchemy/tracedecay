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

/// Two registered projects inside one throwaway profile for fail-closed route
/// tests. Neither project claims a mounted code-index authority.
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

/// Two mounted projects served by the daemon's real project composition.
///
/// Happy-path routed graph reads must use this fixture: the production
/// scheduler is the sole owner of code-index publication and the mounted
/// server is the sole owner of exact graph-read admission. `RoutedProjects`
/// remains below only for fail-closed registry-shape tests that deliberately
/// construct an unmounted or ambiguous target.
struct ProductionRoutedProjects {
    isolation: TempDir,
    harness: tracedecay::daemon::ProductionProjectCompositionHarnessV1,
    active_root: PathBuf,
    target_root: PathBuf,
}

fn init_git_repo(root: &Path) {
    fs::write(root.join(".gitignore"), ".tracedecay/\n").unwrap();
    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "initial"]);
}

async fn production_routed_projects() -> ProductionRoutedProjects {
    let isolation = TempDir::new().unwrap();
    let active_root = isolation.path().join("active");
    let target_root = isolation.path().join("target");

    for (root, file, source) in [
        (
            &active_root,
            "active_only.rs",
            "fn active_only() -> i32 { 1 }\n",
        ),
        (
            &target_root,
            "target_only.rs",
            "fn target_only() -> i32 { 2 }\n",
        ),
    ] {
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join(file), source).unwrap();
        init_git_repo(root);
    }

    let harness = tracedecay::daemon::ProductionProjectCompositionHarnessV1::open(
        isolation.path(),
        [active_root.clone(), target_root.clone()],
    )
    .await
    .expect("production routed-project composition");
    let active_server = harness.server(&active_root).expect("active project server");
    let target_server = harness.server(&target_root).expect("target project server");
    crate::support::warm_code_index_search(&active_server, "active_only").await;
    crate::support::warm_code_index_search(&target_server, "target_only").await;

    ProductionRoutedProjects {
        isolation,
        harness,
        active_root,
        target_root,
    }
}

impl ProductionRoutedProjects {
    fn active_root(&self) -> &Path {
        &self.active_root
    }

    fn target_root(&self) -> &Path {
        &self.target_root
    }

    fn server(&self) -> Arc<McpServer> {
        self.harness
            .server(self.active_root())
            .expect("active project server")
    }

    async fn shutdown(self) {
        self.harness.shutdown().await;
        drop(self.isolation);
    }
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

    // Both projects are real repositories BEFORE `TraceDecay::init` runs:
    // registered-route identity admission reads the repository identity
    // marker that real registration writes into the git common dir, so a
    // routable project must be a repository at registration time.
    let active_dir = TempDir::new().unwrap();
    fs::create_dir_all(active_dir.path().join("src")).unwrap();
    fs::write(
        active_dir.path().join("src/active_only.rs"),
        "fn active_only() -> i32 { 1 }\n",
    )
    .unwrap();
    init_git_repo(active_dir.path());
    let active = TraceDecay::init_with_options(active_dir.path(), options.clone())
        .await
        .unwrap();

    let target_dir = TempDir::new().unwrap();
    fs::create_dir_all(target_dir.path().join("src")).unwrap();
    fs::write(
        target_dir.path().join("src/target_only.rs"),
        "fn target_only() -> i32 { 2 }\n",
    )
    .unwrap();
    init_git_repo(target_dir.path());
    let target = TraceDecay::init_with_options(target_dir.path(), options)
        .await
        .unwrap();

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
    let projects = production_routed_projects().await;
    let target_workspace = projects.target_root().to_path_buf();
    let server = projects.server();

    let responses = run_client_connection_with_messages(
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

    let responses = run_client_connection_with_messages(server, vec![files_call(2)]).await;
    assert_served_project(
        &response_with_id(&responses, json!(2)),
        "unrouted files read on a fresh client",
        "active_only.rs",
        "target_only.rs",
    );
    projects.shutdown().await;
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
    let projects = production_routed_projects().await;
    let target_project = projects.target_root().to_path_buf();

    // Attach a linked worktree at a sibling path outside every ancestor of
    // the registered checkout, so the parent-directory alias walk from it can
    // never reach the registered project.
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
    assert!(
        tracedecay::worktree::git_common_dir(&target_project).is_some(),
        "the linked-worktree fixture needs a resolvable git common dir"
    );
    let server = projects.server();

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
    projects.shutdown().await;
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
    let projects = production_routed_projects().await;
    let unregistered = TempDir::new().unwrap();
    let unregistered_workspace = unregistered.path().to_path_buf();
    let server = projects.server();

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
    projects.shutdown().await;
}

/// Hosts report the workspace path the user opened, which is often a symlink
/// to the checkout. Route resolution canonicalizes before matching aliases, so
/// the symlinked spelling must reach the same registered project rather than
/// missing and falling back to the active one.
#[cfg(unix)]
#[tokio::test]
async fn hook_route_through_symlinked_workspace_reaches_target() {
    let projects = production_routed_projects().await;
    let link_parent = TempDir::new().unwrap();
    let link = link_parent.path().join("target-link");
    std::os::unix::fs::symlink(projects.target_root(), &link)
        .expect("symlink alias for the target checkout");
    let server = projects.server();

    let responses =
        run_server_with_messages(server, vec![workspace_open(&link), files_call(1)]).await;
    assert_served_project(
        &response_with_id(&responses, json!(1)),
        "symlinked hook workspace",
        "target_only.rs",
        "active_only.rs",
    );
    projects.shutdown().await;
}
