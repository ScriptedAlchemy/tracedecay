//! Query-facing route scope convergence tests (plan:
//! `docs/superpowers/plans/v2/01-domain-request-context.md`, "Direct:
//! exact-root CLI/MCP/HTTP/LSP calls resolve the same project and scope" and
//! the negative fail-closed cases).
//!
//! These tests pin that the query-facing MCP entry point
//! (`selected_registered_project_reader`) resolves scope ONCE into the
//! transport-neutral `tracedecay_application::ResolvedScope` and carries it
//! on the routed project reader, failing closed exactly as the unrouted
//! selection already did.

use serde_json::json;

use super::McpServer;
use super::writer_test_support::{init_indexed_repo, registered_context};
use crate::mcp::tools::handlers::selected_registered_project_reader;

#[tokio::test]
async fn exact_root_reader_resolves_same_project_and_scope_via_application_type() {
    let (cg, _dir, authority) = init_indexed_repo().await;
    let project_root = cg.project_root().to_path_buf();
    let context = registered_context(cg, &authority);
    let server = McpServer::new_with_registered_test_context(context, Vec::new())
        .await
        .expect("registered test server");

    let arguments = json!({
        "project_selector": { "path": project_root.to_string_lossy() }
    });
    let first = selected_registered_project_reader(
        "tracedecay_files".to_owned(),
        arguments.clone(),
        server.registry_db.as_deref(),
        server.retained_project_graph_resolver.clone(),
    )
    .await
    .expect("exact-root reader resolves")
    .expect("exact-root reader selects a route");
    let second = selected_registered_project_reader(
        "tracedecay_files".to_owned(),
        arguments,
        server.registry_db.as_deref(),
        server.retained_project_graph_resolver.clone(),
    )
    .await
    .expect("exact-root reader resolves again")
    .expect("exact-root reader selects a route again");

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
        "the same exact root resolves the same scope, digest included"
    );
    assert_eq!(
        scope
            .reference
            .as_ref()
            .map(tracedecay_domain::RefId::as_str),
        Some("refs/heads/main"),
    );

    // The entry-point scope equals the scope the canonical resolver derives
    // for the same exact root: one resolution path, not two.
    let expected = crate::application::context::RegisteredScopeResolver::resolve(
        &project_root,
        &project_root,
        &scope.project_id,
    )
    .expect("canonical resolver resolves the same root");
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
    assert_eq!(
        super::requests::accounting_project_root(
            first.graph.project_root(),
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

    // The unregistered root must live OUTSIDE the registered repository: a
    // path inside it legitimately converges to the registered worktree.
    let outside = tempfile::TempDir::new().expect("outside tempdir");
    let sibling = outside.path().join("unregistered-sibling");
    std::fs::create_dir_all(&sibling).expect("sibling root exists on disk");
    let error = selected_registered_project_reader(
        "tracedecay_files".to_owned(),
        json!({ "project_selector": { "path": sibling.to_string_lossy() } }),
        server.registry_db.as_deref(),
        server.retained_project_graph_resolver.clone(),
    )
    .await
    .expect_err("an unregistered path must fail closed");

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
    let context = runtime
        .mcp_server_context_for_test(cg, None)
        .expect("registered MCP server context");
    let server = McpServer::new_with_registered_test_context(context, Vec::new())
        .await
        .expect("registered test server");

    let error = selected_registered_project_reader(
        "tracedecay_files".to_owned(),
        json!({ "project_selector": { "path": phantom_root.to_string_lossy() } }),
        server.registry_db.as_deref(),
        server.retained_project_graph_resolver.clone(),
    )
    .await
    .expect_err("an unmounted registered project must fail closed");

    let message = error.to_string();
    assert!(
        message.contains("project_route_unavailable"),
        "the unmounted failure keeps its explicit kind: {message}"
    );
}
