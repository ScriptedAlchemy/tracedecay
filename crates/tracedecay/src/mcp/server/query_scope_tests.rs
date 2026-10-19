//! Query-facing route scope convergence and fail-closed tests.
//!
//! These tests pin that the query-facing MCP entry point
//! (`resolve_registered_project_route_for_tool`) resolves scope ONCE into the
//! transport-neutral `tracedecay_contracts::ResolvedScope` and carries it
//! on the routed project reader, failing closed exactly as the unrouted
//! selection already did.

use std::path::Path;

use serde_json::json;
use tempfile::TempDir;
use tracedecay_contracts::ResolvedScope;
use tracedecay_domain::ProjectId;
use tracedecay_session_memory::context::RegisteredScopeResolver;

use super::McpServer;
use super::writer_test_support::{init_indexed_repo, registered_context};
use crate::mcp::tools::handlers::resolve_registered_project_route_for_tool;
use crate::test_support::git::git;

async fn assert_scope_surface_identity(
    registered_root: &Path,
    requested_root: &Path,
    project_id: &str,
) -> ResolvedScope {
    let profile = TempDir::new().expect("isolated profile");
    let (registry, _registry_owner) =
        tracedecay_global_db::tests::harness::open_registered_test_database_fixture(
            &profile.path().join("profile.db"),
            tracedecay_runtime_core::db::TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        .expect("registered profile database");
    registry
        .upsert_code_project(project_id, registered_root, None, None, None)
        .await
        .expect("enroll exact project root");
    let owner = registry
        .project_registry_context_by_id(project_id)
        .await
        .expect("registered owner lookup")
        .expect("enrolled owner");
    let project_id = ProjectId::new(project_id).expect("project id");
    let (scope_root, mcp) = tracedecay_mcp::scope::resolve_query_scope(&owner, requested_root)
        .expect("MCP exact scope");
    let indexed =
        tracedecay_code_index_runtime::resolved_scope_for_project(&scope_root, &project_id)
            .expect("indexed exact scope");
    let application =
        RegisteredScopeResolver::resolve(registered_root, requested_root, &project_id)
            .expect("application exact scope");
    let dashboard = tracedecay_dashboard_api::scope::resolve_dashboard_scope(
        &scope_root,
        Some(project_id.as_str()),
    )
    .expect("dashboard exact scope");
    for (surface, scope) in [
        ("MCP", mcp),
        ("application", application),
        ("dashboard", dashboard),
    ] {
        scope.validate().expect("surface digest validates");
        assert_eq!(scope.scope_digest, indexed.scope_digest, "{surface} digest");
        assert_eq!(scope, indexed, "{surface} structural scope");
    }
    indexed
}

fn scope_identity_repository() -> TempDir {
    let root = TempDir::new().expect("repository");
    git(root.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(root.path().join("lib.rs"), "pub fn scope() {}\n").expect("source");
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "initial"]);
    assert!(
        tracedecay_runtime_core::storage::write_repository_identity_marker(
            root.path(),
            "project.scope-parity",
        )
        .expect("registered repository marker")
    );
    root
}

#[tokio::test]
async fn registered_scope_surfaces_share_git_identity() {
    let repository = scope_identity_repository();
    let root = repository.path().canonicalize().expect("canonical root");
    let subdir = root.join("src/deep");
    std::fs::create_dir_all(&subdir).expect("ordinary subdirectory");
    let scope = assert_scope_surface_identity(&root, &subdir, "project.scope-parity").await;
    assert_eq!(
        scope,
        tracedecay_code_index_runtime::resolved_scope_for_project(&root, &scope.project_id)
            .expect("registered root indexing scope"),
    );
}

#[tokio::test]
async fn registered_scope_surfaces_share_linked_identity() {
    let repository = scope_identity_repository();
    let root = repository.path().canonicalize().expect("canonical root");
    let linked = root.join("linked-checkouts/feature");
    git(
        &root,
        &["worktree", "add", linked.to_str().unwrap(), "-b", "feature"],
    );
    let linked = linked.canonicalize().expect("canonical linked checkout");
    let scope = assert_scope_surface_identity(&root, &linked, "project.scope-parity").await;
    let primary =
        tracedecay_code_index_runtime::resolved_scope_for_project(&root, &scope.project_id)
            .expect("primary indexing scope");
    assert_eq!(scope.repository_id, primary.repository_id);
    assert_ne!(scope.worktree_id, primary.worktree_id);
    assert_ne!(scope.scope_digest, primary.scope_digest);
}

#[tokio::test]
async fn registered_scope_surfaces_share_enrolled_package_identity() {
    let repository = scope_identity_repository();
    let root = repository.path().canonicalize().expect("canonical root");
    let package = root.join("packages/app");
    let subdir = package.join("src/deep");
    std::fs::create_dir_all(&subdir).expect("package subdirectory");
    let project_id = tracedecay_runtime_core::storage::default_profile_project_id(&package);
    assert_ne!(project_id, "project.scope-parity");
    let scope = assert_scope_surface_identity(&package, &subdir, &project_id).await;
    let enclosing = tracedecay_code_index_runtime::resolved_scope_for_project(
        &root,
        &ProjectId::new("project.scope-parity").expect("enclosing project"),
    )
    .expect("enclosing indexing scope");
    assert_ne!(scope.project_id, enclosing.project_id);
    assert_eq!(scope.repository_id, enclosing.repository_id);
    assert_ne!(scope.worktree_id, enclosing.worktree_id);
    assert_ne!(scope.scope_digest, enclosing.scope_digest);
}

#[tokio::test]
async fn registered_scope_surfaces_share_non_git_identity() {
    let directory = TempDir::new().expect("non-Git project");
    let root = directory.path().canonicalize().expect("canonical root");
    let subdir = root.join("src/deep");
    std::fs::create_dir_all(&subdir).expect("ordinary subdirectory");
    let project_id = tracedecay_runtime_core::storage::default_profile_project_id(&root);
    let scope = assert_scope_surface_identity(&root, &subdir, &project_id).await;
    assert!(scope.reference.is_none());
}

#[tokio::test]
async fn canonical_project_id_reader_resolves_same_project_and_scope_via_application_type() {
    let (cg, _dir, authority) = init_indexed_repo().await;
    let project_root = cg.project_root().to_path_buf();
    let context = registered_context(cg, &authority);
    let server = McpServer::new_with_registered_test_context(context, Vec::new())
        .await
        .expect("registered test server");

    let arguments = json!({
        "project_selector": { "project_id": "project.mcp-writer" }
    });
    let first = resolve_registered_project_route_for_tool(
        "tracedecay_files".to_owned(),
        arguments.clone(),
        server.registry_db.as_deref(),
        server.retained_project_server_resolver.clone(),
    )
    .await
    .expect("project-id reader resolves")
    .expect("project-id reader selects a route");
    let second = resolve_registered_project_route_for_tool(
        "tracedecay_files".to_owned(),
        arguments,
        server.registry_db.as_deref(),
        server.retained_project_server_resolver.clone(),
    )
    .await
    .expect("project-id reader resolves again")
    .expect("project-id reader selects a route again");

    let scope = &first.scope;
    scope.validate().expect("route scope validates");
    assert_eq!(scope.project_id.as_str(), "project.mcp-writer");
    assert_eq!(
        scope.project_id.as_str(),
        first.owner.project.project_id,
        "the scope names the same project the registry authorized"
    );
    assert_eq!(
        scope, &second.scope,
        "the same project id resolves the same scope, digest included"
    );
    assert_eq!(
        scope
            .reference
            .as_ref()
            .map(tracedecay_domain::RefId::as_str),
        Some("refs/heads/main"),
    );

    // The entry-point scope equals the daemon code-index authority for the
    // same exact root. Registered-root authorization cannot substitute its
    // privacy-bound provenance identifiers for this application scope.
    let expected =
        tracedecay_code_index_runtime::resolved_scope_for_project(&project_root, &scope.project_id)
            .expect("daemon identity authority resolves the same root");
    assert_eq!(
        scope, &expected,
        "the routed scope must equal the canonical exact-root resolution"
    );
    let target = super::requests::invocation_target_for_route(Some(&first));
    assert_eq!(
        target.resolved(),
        Some(scope),
        "handler admission must consume the exact scope resolved at routing"
    );
    let selected_graph = first
        .retained_server()
        .expect("selected server remains mounted")
        .cg_snapshot()
        .await;
    assert_eq!(
        super::requests::accounting_project_root(
            selected_graph.project_root(),
            Some(&first.owner),
            Some(scope),
        ),
        Some(std::path::Path::new(&first.owner.project.canonical_root)),
        "accounting must retain the selected route authority instead of the active project"
    );
}

#[tokio::test]
async fn unregistered_selector_still_fails_closed_without_substitution() {
    let (cg, _dir, authority) = init_indexed_repo().await;
    let context = registered_context(cg, &authority);
    let server = McpServer::new_with_registered_test_context(context, Vec::new())
        .await
        .expect("registered test server");

    let error = resolve_registered_project_route_for_tool(
        "tracedecay_files".to_owned(),
        json!({ "project_selector": { "project_id": "project.missing" } }),
        server.registry_db.as_deref(),
        server.retained_project_server_resolver.clone(),
    )
    .await
    .expect_err("an unregistered project id must fail closed");

    let message = error.to_string();
    assert!(
        message.contains("project_route_not_found"),
        "the unrouted failure keeps its explicit kind: {message}"
    );
}

#[tokio::test]
async fn registered_but_unmounted_project_still_reports_unavailable() {
    let (cg, _dir, authority) = init_indexed_repo().await;
    let runtime = super::writer_test_support::registered_runtime(&authority);
    // A second project known to the registry but never mounted: the resolver
    // has no graph for it, so the route reports unavailable rather than
    // fabricating a graph or a scope. Its root must live OUTSIDE the mounted
    // repository: a path inside it would converge to the mounted worktree.
    let outside = tempfile::TempDir::new().expect("outside tempdir");
    let phantom_root = outside.path().join("registered-unmounted");
    std::fs::create_dir_all(&phantom_root).expect("phantom root exists on disk");
    let phantom_root = phantom_root.canonicalize().expect("canonical phantom root");
    runtime
        .upsert_code_project(
            "project.mcp-writer-unmounted",
            &phantom_root,
            None,
            None,
            Some("main"),
        )
        .await
        .expect("phantom project registers");
    let context =
        crate::test_support::host_admission::mcp_server_context_for_test(runtime, cg, None)
            .expect("registered MCP server context");
    let server = McpServer::new_with_registered_test_context(context, Vec::new())
        .await
        .expect("registered test server");

    let error = resolve_registered_project_route_for_tool(
        "tracedecay_files".to_owned(),
        json!({ "project_selector": { "project_id": "project.mcp-writer-unmounted" } }),
        server.registry_db.as_deref(),
        server.retained_project_server_resolver.clone(),
    )
    .await
    .expect_err("an unmounted registered project must fail closed");

    let message = error.to_string();
    assert!(
        message.contains("project_route_unavailable"),
        "the unmounted failure keeps its explicit kind: {message}"
    );
}
