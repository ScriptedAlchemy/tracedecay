//! `tracedecay_project_search` through a real MCP `tools/call`.
//!
//! Each assertion is the payload or text a caller sees for one concrete
//! query. Registry paths and timestamps are the rows the fixture stored;
//! every other field is a literal.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;
use tracedecay::project::TraceDecay;
use tracedecay::test_support::host_admission::HostAdmissionTestRuntimeV1;

use crate::support::{
    TestEnv, TestTempDir, TestTraceDecay, extract_real_server_text, handle_real_server_tool_call,
    handle_real_server_tool_call_raw, setup_empty_project, test_temp_dir,
};

/// Registry stamps written by the fixture, not wall-clock. Search orders by
/// `last_seen_at` descending, and the production stamp is whole seconds, so a
/// test that left them to the clock would collide and reorder the page.
const ALPHA_SEEN_AT: i64 = 1_700_000_001;
const BETA_SEEN_AT: i64 = 1_700_000_002;
const ACTIVE_SEEN_AT: i64 = 1_700_000_003;

struct RegisteredSearch {
    _project: TestTraceDecay,
    _env: TestEnv,
    _project_dir: TestTempDir,
    _registry_dir: TestTempDir,
    server: Arc<McpServer>,
    registry_path: String,
    alpha_root: String,
    beta_root: String,
    active_id: String,
    active_label: String,
    active_root: String,
}

async fn registered_search(remote: Option<&str>, alias: Option<&str>) -> RegisteredSearch {
    let (cg, env, project_dir) = setup_empty_project().await;
    let registry_dir = test_temp_dir();
    let alpha_dir = registry_dir.path().join("alpha-checkout");
    let beta_dir = registry_dir.path().join("beta-checkout");
    fs::create_dir_all(&alpha_dir).expect("alpha checkout");
    fs::create_dir_all(&beta_dir).expect("beta checkout");
    let active_id = cg
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("active project identity");
    let active_label = cg
        .project_root()
        .file_name()
        .and_then(|name| name.to_str())
        .expect("active project directory name")
        .to_owned();
    let project_id =
        tracedecay_domain::ProjectId::new(active_id.clone()).expect("active project id");
    let runtime = HostAdmissionTestRuntimeV1::project_scoped(
        registry_dir.path(),
        cg.project_root(),
        project_id,
    )
    .await
    .expect("project-scoped registry");

    let alpha = runtime
        .upsert_code_project("proj_alpha", &alpha_dir, None, remote, Some("main"))
        .await
        .expect("register proj_alpha");
    if let Some(alias) = alias {
        runtime
            .upsert_project_alias(Path::new(alias), "proj_alpha")
            .await
            .expect("register project alias");
    }
    let beta = runtime
        .upsert_code_project("proj_beta", &beta_dir, None, None, Some("release-line"))
        .await
        .expect("register proj_beta");
    let active = runtime
        .upsert_code_project(
            &active_id,
            cg.project_root(),
            None,
            None,
            Some("caller-branch"),
        )
        .await
        .expect("register calling project");
    stamp_project(registry_dir.path(), "proj_alpha", ALPHA_SEEN_AT);
    stamp_project(registry_dir.path(), "proj_beta", BETA_SEEN_AT);
    stamp_project(registry_dir.path(), &active_id, ACTIVE_SEEN_AT);

    let alpha_root = alpha_dir
        .canonicalize()
        .expect("alpha checkout path")
        .display()
        .to_string();
    let beta_root = beta_dir
        .canonicalize()
        .expect("beta checkout path")
        .display()
        .to_string();
    let active_root = cg
        .project_root()
        .canonicalize()
        .expect("calling project path")
        .display()
        .to_string();
    assert_eq!(alpha.display_root, alpha_root);
    assert_eq!(alpha.canonical_root, alpha_root);
    assert_eq!(beta.display_root, beta_root);
    assert_eq!(active.display_root, active_root);
    let registry_path = registry_dir
        .path()
        .join("global.db")
        .canonicalize()
        .expect("registry database")
        .display()
        .to_string();

    let graph = TraceDecay::open(cg.project_root())
        .await
        .expect("reopen project for the MCP server");
    let server = McpServer::new_with_host_admission_test_runtime_for_test(graph, None, runtime)
        .await
        .expect("registered MCP server");

    RegisteredSearch {
        _project: cg,
        _env: env,
        _project_dir: project_dir,
        _registry_dir: registry_dir,
        server,
        registry_path,
        alpha_root,
        beta_root,
        active_id,
        active_label,
        active_root,
    }
}

fn stamp_project(profile_root: &Path, project_id: &str, seen_at: i64) {
    let updated = rusqlite::Connection::open(profile_root.join("global.db"))
        .expect("open registry")
        .execute(
            "UPDATE code_projects SET created_at = ?1, last_seen_at = ?1 WHERE project_id = ?2",
            rusqlite::params![seen_at, project_id],
        )
        .expect("stamp registered project");
    assert_eq!(updated, 1, "fixture must stamp {project_id}");
}

async fn search_json(server: &McpServer, arguments: Value) -> Value {
    let result = handle_real_server_tool_call(server, "tracedecay_project_search", arguments).await;
    assert_eq!(result["content"][0]["type"], "text");
    let text = extract_real_server_text(&result);
    serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("tracedecay_project_search text was not JSON: {error}\n{text}")
    })
}

async fn search_text(server: &McpServer, arguments: Value) -> String {
    let result = handle_real_server_tool_call(server, "tracedecay_project_search", arguments).await;
    extract_real_server_text(&result).to_owned()
}

fn project_json(
    id: &str,
    label: &str,
    root: &str,
    branch: &str,
    created_at: i64,
    last_seen_at: i64,
    is_active: bool,
) -> Value {
    json!({
        "canonical_root": root,
        "created_at": created_at,
        "default_branch": branch,
        "display_root": root,
        "git_common_dir": null,
        "is_active": is_active,
        "label": label,
        "last_seen_at": last_seen_at,
        "project_id": id,
        "project_root": root,
    })
}

fn tree_entry(
    id: &str,
    label: &str,
    root: &str,
    branch: &str,
    last_seen_at: i64,
    alias_count: usize,
    is_active: bool,
) -> Value {
    json!({
        "alias_count": alias_count,
        "artifact_count": 0,
        "branches": [branch],
        "canonical_root": root,
        "default_branch": branch,
        "is_active": is_active,
        "kind": "project",
        "label": label,
        "last_seen_at": last_seen_at,
        "project_id": id,
        "project_root": root,
        "store_count": 0,
    })
}

fn tree_group(label: &str, branch: &str, entry: Value) -> Value {
    json!({
        "branches": [branch],
        "git_common_dir": null,
        "label": label,
        "project_count": 1,
        "projects": [entry],
    })
}

fn ok_listing(
    query: &str,
    registry_path: &str,
    limit: u64,
    truncated: bool,
    project_count: usize,
    repo_count: usize,
    projects: Vec<Value>,
    tree: Vec<Value>,
) -> Value {
    json!({
        "limit": limit,
        "project_tree": tree,
        "projects": projects,
        "query": query,
        "registry_path": registry_path,
        "status": "ok",
        "summary": {
            "project_count": project_count,
            "repo_count": repo_count,
            "truncated": truncated,
        },
        "title": format!("projects matching \"{query}\""),
        "truncated": truncated,
    })
}

fn matched_markdown(
    query: &str,
    label: &str,
    branch: &str,
    id: &str,
    active: bool,
    root: &str,
    truncated: bool,
) -> String {
    let marker = if active { " *" } else { "" };
    let mut text = format!(
        "Found 1 projects matching \"{query}\" across 1 repositories.\n\nRepositories:\n- {label} (branches: {branch})\n  - `{id}`{marker} [project] branches: {branch}; stores: 0; path: {root}\n"
    );
    if truncated {
        text.push_str("\nResult truncated; increase limit for more projects.\n");
    }
    text
}

fn alpha_project(world: &RegisteredSearch, alias_count: usize, active: bool) -> (Value, Value) {
    let project = project_json(
        "proj_alpha",
        "alpha-checkout",
        &world.alpha_root,
        "main",
        ALPHA_SEEN_AT,
        ALPHA_SEEN_AT,
        active,
    );
    let entry = tree_entry(
        "proj_alpha",
        "alpha-checkout",
        &world.alpha_root,
        "main",
        ALPHA_SEEN_AT,
        alias_count,
        active,
    );
    (project, tree_group("alpha-checkout", "main", entry))
}

fn beta_project(world: &RegisteredSearch) -> (Value, Value) {
    let project = project_json(
        "proj_beta",
        "beta-checkout",
        &world.beta_root,
        "release-line",
        BETA_SEEN_AT,
        BETA_SEEN_AT,
        false,
    );
    let entry = tree_entry(
        "proj_beta",
        "beta-checkout",
        &world.beta_root,
        "release-line",
        BETA_SEEN_AT,
        1,
        false,
    );
    (project, tree_group("beta-checkout", "release-line", entry))
}

#[tokio::test]
async fn project_search_returns_the_registered_project_for_one_query() {
    let world = registered_search(None, Some("sibling-alias")).await;
    let (alpha, alpha_group) = alpha_project(&world, 2, false);

    let by_id = search_json(
        &world.server,
        json!({"query": "proj_alpha", "format": "json"}),
    )
    .await;
    assert_eq!(
        by_id,
        ok_listing(
            "proj_alpha",
            &world.registry_path,
            10,
            false,
            1,
            1,
            vec![alpha.clone()],
            vec![alpha_group.clone()],
        )
    );

    let markdown = search_text(
        &world.server,
        json!({"query": "proj_alpha", "format": "markdown"}),
    )
    .await;
    assert_eq!(
        markdown,
        matched_markdown(
            "proj_alpha",
            "alpha-checkout",
            "main",
            "proj_alpha",
            false,
            &world.alpha_root,
            false,
        )
    );

    let by_alias = search_json(
        &world.server,
        json!({"query": "sibling-alias", "format": "json"}),
    )
    .await;
    assert_eq!(
        by_alias,
        ok_listing(
            "sibling-alias",
            &world.registry_path,
            10,
            false,
            1,
            1,
            vec![alpha.clone()],
            vec![alpha_group.clone()],
        )
    );

    let (beta, beta_group) = beta_project(&world);
    let by_branch = search_json(
        &world.server,
        json!({"query": "release-line", "format": "json"}),
    )
    .await;
    assert_eq!(
        by_branch,
        ok_listing(
            "release-line",
            &world.registry_path,
            10,
            false,
            1,
            1,
            vec![beta],
            vec![beta_group],
        )
    );

    let by_case = search_json(
        &world.server,
        json!({"query": "PROJ_ALPHA", "format": "json"}),
    )
    .await;
    assert_eq!(
        by_case,
        ok_listing(
            "PROJ_ALPHA",
            &world.registry_path,
            10,
            false,
            1,
            1,
            vec![alpha.clone()],
            vec![alpha_group.clone()],
        )
    );

    let wildcard = search_json(&world.server, json!({"query": "proj_%", "format": "json"})).await;
    assert_eq!(
        wildcard,
        ok_listing(
            "proj_%",
            &world.registry_path,
            10,
            false,
            0,
            0,
            Vec::new(),
            Vec::new(),
        )
    );

    let (beta, beta_group) = beta_project(&world);
    let either = search_json(
        &world.server,
        json!({"query": "alpha beta", "limit": 99, "format": "json"}),
    )
    .await;
    assert_eq!(
        either,
        ok_listing(
            "alpha beta",
            &world.registry_path,
            50,
            false,
            2,
            2,
            vec![beta, alpha.clone()],
            vec![alpha_group, beta_group],
        )
    );

    let page = search_json(
        &world.server,
        json!({"query": "checkout", "limit": 0, "format": "json"}),
    )
    .await;
    let (beta, beta_group) = beta_project(&world);
    assert_eq!(
        page,
        ok_listing(
            "checkout",
            &world.registry_path,
            1,
            true,
            1,
            1,
            vec![beta],
            vec![beta_group],
        )
    );
    let page_text = search_text(
        &world.server,
        json!({"query": "checkout", "limit": 0, "format": "markdown"}),
    )
    .await;
    assert_eq!(
        page_text,
        matched_markdown(
            "checkout",
            "beta-checkout",
            "release-line",
            "proj_beta",
            false,
            &world.beta_root,
            true,
        )
    );

    let active_project = project_json(
        &world.active_id,
        &world.active_label,
        &world.active_root,
        "caller-branch",
        ACTIVE_SEEN_AT,
        ACTIVE_SEEN_AT,
        true,
    );
    let active_group = tree_group(
        &world.active_label,
        "caller-branch",
        tree_entry(
            &world.active_id,
            &world.active_label,
            &world.active_root,
            "caller-branch",
            ACTIVE_SEEN_AT,
            1,
            true,
        ),
    );
    let active = search_json(
        &world.server,
        json!({"query": world.active_id, "format": "json"}),
    )
    .await;
    assert_eq!(
        active,
        ok_listing(
            &world.active_id,
            &world.registry_path,
            10,
            false,
            1,
            1,
            vec![active_project],
            vec![active_group],
        )
    );
    let active_text = search_text(
        &world.server,
        json!({
            "query": world.active_id,
            "format": "markdown",
        }),
    )
    .await;
    assert_eq!(
        active_text,
        matched_markdown(
            &world.active_id,
            &world.active_label,
            "caller-branch",
            &world.active_id,
            true,
            &world.active_root,
            false,
        )
    );

    let (beta, beta_group) = beta_project(&world);
    let (alpha, alpha_group) = alpha_project(&world, 2, false);
    let active_project = project_json(
        &world.active_id,
        &world.active_label,
        &world.active_root,
        "caller-branch",
        ACTIVE_SEEN_AT,
        ACTIVE_SEEN_AT,
        true,
    );
    let active_entry = tree_entry(
        &world.active_id,
        &world.active_label,
        &world.active_root,
        "caller-branch",
        ACTIVE_SEEN_AT,
        1,
        true,
    );
    let mut groups = vec![
        tree_group(&world.active_label, "caller-branch", active_entry),
        alpha_group,
        beta_group,
    ];
    groups.sort_by(|left, right| {
        left["label"]
            .as_str()
            .unwrap()
            .cmp(right["label"].as_str().unwrap())
    });
    let all = search_json(&world.server, json!({"query": "", "format": "json"})).await;
    assert_eq!(
        all,
        ok_listing(
            "",
            &world.registry_path,
            10,
            false,
            3,
            3,
            vec![active_project, beta, alpha],
            groups,
        )
    );

    let blank = search_json(&world.server, json!({"query": "   ", "format": "json"})).await;
    assert_eq!(
        blank,
        ok_listing(
            "   ",
            &world.registry_path,
            10,
            false,
            0,
            0,
            Vec::new(),
            Vec::new(),
        )
    );
    let blank_text =
        search_text(&world.server, json!({"query": "   ", "format": "markdown"})).await;
    assert_eq!(blank_text, "No projects matching \"   \" found.");

    let missing = handle_real_server_tool_call_raw(
        &world.server,
        "tracedecay_project_search",
        json!({"limit": 5}),
    )
    .await;
    assert_eq!(
        missing["error"],
        json!({
            "code": -32602,
            "message": "missing required parameter: query",
            "data": {
                "detail": "missing required parameter: query",
                "reason_code": "missing_required_parameter",
                "retryable": false,
                "tool": "tracedecay_project_search",
            }
        })
    );
}

#[tokio::test]
async fn project_search_matches_the_remote_name_and_hides_the_credential() {
    let world = registered_search(
        Some("https://user:secret-token@example.test/hidden.git"),
        Some("sibling-alias"),
    )
    .await;
    let (alpha, alpha_group) = alpha_project(&world, 3, false);

    let by_name = search_json(
        &world.server,
        json!({"query": "hidden.git", "format": "json"}),
    )
    .await;
    assert_eq!(
        by_name,
        ok_listing(
            "hidden.git",
            &world.registry_path,
            10,
            false,
            1,
            1,
            vec![alpha.clone()],
            vec![alpha_group.clone()],
        )
    );
    let by_alias = search_json(
        &world.server,
        json!({"query": "sibling-alias", "format": "json"}),
    )
    .await;
    assert_eq!(
        by_alias,
        ok_listing(
            "sibling-alias",
            &world.registry_path,
            10,
            false,
            1,
            1,
            vec![alpha],
            vec![alpha_group],
        )
    );

    let by_secret = search_json(
        &world.server,
        json!({"query": "secret-token", "format": "json"}),
    )
    .await;
    assert_eq!(
        by_secret,
        ok_listing(
            "secret-token",
            &world.registry_path,
            10,
            false,
            0,
            0,
            Vec::new(),
            Vec::new(),
        )
    );
}

#[tokio::test]
async fn project_search_missing_registry_is_unavailable() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let graph = TraceDecay::open(cg.project_root())
        .await
        .expect("reopen project for the MCP server");
    let server = McpServer::new(graph, None).await;

    let payload = search_json(&server, json!({"query": "alpha", "format": "json"})).await;
    assert_eq!(
        payload,
        json!({
            "limit": 10,
            "message": "project registry is not present for this profile",
            "project_tree": [],
            "projects": [],
            "query": "alpha",
            "status": "unavailable",
            "summary": {
                "project_count": 0,
                "repo_count": 0,
                "truncated": false,
            },
            "title": "projects matching \"alpha\"",
            "truncated": false,
        })
    );

    let text = search_text(&server, json!({"query": "alpha", "format": "markdown"})).await;
    assert!(
        text.contains("**message:** project registry is not present for this profile"),
        "missing-registry markdown must state why, not an empty list: {text}"
    );
    assert!(
        !text.starts_with("No projects matching"),
        "an unmounted registry must not render as an authoritative empty search: {text}"
    );
}
