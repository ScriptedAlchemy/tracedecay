//! `tracedecay_project_search` as a caller sees it over MCP `tools/call`.
//!
//! Each test sends the production JSON-RPC request through `McpServer` and
//! asserts the payload a client reads. Expectations are the registered
//! project values and the typed miss, bound, and refusal states — not a
//! second call into the search handler.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;
use tracedecay_domain::ProjectId;
use tracedecay_project::project::TraceDecay;
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;

use crate::support::{
    TestEnv, TestTempDir, TestTraceDecay, extract_real_server_text, handle_real_server_tool_call,
    handle_real_server_tool_call_raw, setup_empty_project, test_temp_dir,
};

const TOOL: &str = "tracedecay_project_search";
const SECRET: &str = "secret-token-xyz";
const ACTIVE_HEAD: &str = "served-head";

struct RegisteredProject {
    id: String,
    label: String,
    /// Registered default branch.
    branch: String,
    /// Live checkout HEAD; `None` for the plain directories.
    head_branch: Option<&'static str>,
    display_root: String,
    canonical_root: String,
    created_at: i64,
    last_seen_at: i64,
    alias_count: i64,
}

struct SearchFixture {
    _cg: TestTraceDecay,
    _env: TestEnv,
    _project_dir: TestTempDir,
    _registry_dir: TestTempDir,
    _roots: TestTempDir,
    server: std::sync::Arc<McpServer>,
    registry_path: String,
    active: RegisteredProject,
    alpha: RegisteredProject,
    beta: RegisteredProject,
}

async fn open_search_fixture() -> SearchFixture {
    let (cg, env, project_dir) = setup_empty_project().await;
    let registry_dir = test_temp_dir();
    let roots = test_temp_dir();
    let alpha_root = roots.path().join("search-alpha-root");
    let beta_root = roots.path().join("search-beta-root");
    let alpha_alias = roots.path().join("shared-needle-alpha");
    let beta_alias = roots.path().join("shared-needle-beta");
    fs::create_dir_all(&alpha_root).unwrap();
    fs::create_dir_all(&beta_root).unwrap();
    fs::create_dir_all(&alpha_alias).unwrap();
    fs::create_dir_all(&beta_alias).unwrap();

    let active_id = cg
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("active project identity");
    let runtime = HostAdmissionTestRuntimeV1::project_scoped(
        registry_dir.path(),
        cg.project_root(),
        ProjectId::new(active_id.clone()).expect("active project id"),
    )
    .await
    .expect("project-scoped registry runtime");

    let alpha = register_project(
        &runtime,
        "search-alpha",
        "search-alpha-root",
        &alpha_root,
        "branch-alpha",
        Some(&format!(
            "https://user:{SECRET}@example.test/search-alpha-repo.git"
        )),
        Some(&alpha_alias),
        3,
    )
    .await;
    let beta = register_project(
        &runtime,
        "search-beta",
        "search-beta-root",
        &beta_root,
        "branch-beta",
        None,
        Some(&beta_alias),
        2,
    )
    .await;
    let active_label = file_name(cg.project_root());
    crate::common::fixture::git_run(
        cg.project_root(),
        &["symbolic-ref", "HEAD", &format!("refs/heads/{ACTIVE_HEAD}")],
    );
    let mut active = register_project(
        &runtime,
        &active_id,
        &active_label,
        cg.project_root(),
        "branch-active",
        None,
        None,
        1,
    )
    .await;
    active.head_branch = Some(ACTIVE_HEAD);

    let registry_path = registry_dir
        .path()
        .join("global.db")
        .canonicalize()
        .expect("registry database")
        .display()
        .to_string();
    let server = McpServer::new_with_host_admission_test_runtime_for_test(
        TraceDecay::open_with_options(cg.project_root(), crate::support::graph_open_options(&cg))
            .await
            .expect("open calling project"),
        None,
        runtime,
    )
    .await
    .expect("MCP server");

    SearchFixture {
        _cg: cg,
        _env: env,
        _project_dir: project_dir,
        _registry_dir: registry_dir,
        _roots: roots,
        server,
        registry_path,
        active,
        alpha,
        beta,
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_else(|| panic!("path has no file name: {}", path.display()))
        .to_owned()
}

async fn register_project(
    runtime: &HostAdmissionTestRuntimeV1,
    project_id: &str,
    label: &str,
    root: &Path,
    branch: &str,
    remote: Option<&str>,
    alias: Option<&Path>,
    alias_count: i64,
) -> RegisteredProject {
    let record = runtime
        .upsert_code_project(project_id, root, None, remote, Some(branch))
        .await
        .unwrap_or_else(|error| panic!("register {project_id}: {error}"));
    assert_eq!(
        record.project_id, project_id,
        "registry rewrote the project id"
    );
    if let Some(alias) = alias {
        runtime
            .upsert_project_alias(alias, project_id)
            .await
            .unwrap_or_else(|error| panic!("alias {project_id}: {error}"));
    }
    RegisteredProject {
        id: project_id.to_owned(),
        label: label.to_owned(),
        branch: branch.to_owned(),
        head_branch: None,
        display_root: record.display_root,
        canonical_root: record.canonical_root,
        created_at: record.created_at,
        last_seen_at: record.last_seen_at,
        alias_count,
    }
}

async fn search_json(server: &McpServer, arguments: Value) -> Value {
    let result = handle_real_server_tool_call(server, TOOL, arguments).await;
    let text = extract_real_server_text(&result);
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("project search JSON for {text}: {error}"))
}

async fn search_text(server: &McpServer, arguments: Value) -> String {
    let result = handle_real_server_tool_call(server, TOOL, arguments).await;
    extract_real_server_text(&result).to_owned()
}

fn project_ids(payload: &Value) -> Vec<String> {
    payload["projects"]
        .as_array()
        .unwrap_or_else(|| panic!("projects is not an array: {payload}"))
        .iter()
        .map(|project| {
            project["project_id"]
                .as_str()
                .unwrap_or_else(|| panic!("project id missing: {project}"))
                .to_owned()
        })
        .collect()
}

fn newest_first(projects: &[&RegisteredProject]) -> Vec<String> {
    let mut projects = projects.to_vec();
    projects.sort_by(|left, right| {
        right
            .last_seen_at
            .cmp(&left.last_seen_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    projects
        .into_iter()
        .map(|project| project.id.clone())
        .collect()
}

fn public_project(project: &RegisteredProject, is_active: bool) -> Value {
    json!({
        "project_id": project.id,
        "label": project.label,
        "project_root": project.display_root,
        "display_root": project.display_root,
        "canonical_root": project.canonical_root,
        "git_common_dir": null,
        "default_branch": project.branch,
        "head_branch": project.head_branch,
        "created_at": project.created_at,
        "last_seen_at": project.last_seen_at,
        "is_active": is_active,
    })
}

fn tree_group(project: &RegisteredProject, is_active: bool) -> Value {
    json!({
        "label": project.label,
        "git_common_dir": null,
        "project_count": 1,
        "branches": [listed_branch(project)],
        "projects": [{
            "project_id": project.id,
            "label": project.label,
            "project_root": project.display_root,
            "canonical_root": project.canonical_root,
            "kind": "project",
            "default_branch": project.branch,
            "head_branch": project.head_branch,
            "branches": [listed_branch(project)],
            "store_count": 0,
            "artifact_count": 0,
            "alias_count": project.alias_count,
            "last_seen_at": project.last_seen_at,
            "is_active": is_active,
        }],
    })
}

fn listing(
    query: &str,
    limit: i64,
    truncated: bool,
    registry_path: &str,
    projects: Vec<Value>,
    tree: Vec<Value>,
) -> Value {
    json!({
        "status": "ok",
        "title": format!("projects matching \"{query}\""),
        "registry_path": registry_path,
        "limit": limit,
        "truncated": truncated,
        "summary": {
            "project_count": projects.len(),
            "repo_count": tree.len(),
            "truncated": truncated,
        },
        "project_tree": tree,
        "projects": projects,
        "query": query,
    })
}

fn assert_payload(actual: &Value, expected: Value) {
    assert_eq!(actual, &expected, "project search payload");
}

fn markdown_hit(query: &str, project: &RegisteredProject, active: bool) -> String {
    let marker = if active { " *" } else { "" };
    format!(
        "Found 1 projects matching \"{query}\" across 1 repositories.\n\nRepositories:\n- {} (branches: {})\n  - `{}`{marker} [project] branches: {}; stores: 0; path: {}\n",
        project.label,
        listed_branch(project),
        project.id,
        listed_branch(project),
        project.display_root
    )
}

/// A readable checkout lists its live HEAD; a plain directory keeps the
/// registered branch.
fn listed_branch(project: &RegisteredProject) -> &str {
    project.head_branch.unwrap_or(&project.branch)
}

#[tokio::test]
async fn project_search_returns_each_public_field_and_omits_the_credential_remote() {
    let fixture = open_search_fixture().await;
    let server = fixture.server.as_ref();

    let by_id = search_json(server, json!({"query": "search-alpha", "format": "json"})).await;
    assert_payload(
        &by_id,
        listing(
            "search-alpha",
            10,
            false,
            &fixture.registry_path,
            vec![public_project(&fixture.alpha, false)],
            vec![tree_group(&fixture.alpha, false)],
        ),
    );
    assert!(
        !by_id.to_string().contains(SECRET),
        "a hit must not echo the credential-bearing remote: {by_id}"
    );

    let by_case = search_json(server, json!({"query": "SEARCH-ALPHA", "format": "json"})).await;
    assert_payload(
        &by_case,
        listing(
            "SEARCH-ALPHA",
            10,
            false,
            &fixture.registry_path,
            vec![public_project(&fixture.alpha, false)],
            vec![tree_group(&fixture.alpha, false)],
        ),
    );

    let by_path = search_json(
        server,
        json!({"query": "search-alpha-root", "format": "json"}),
    )
    .await;
    assert_eq!(project_ids(&by_path), ["search-alpha"]);

    let by_alias = search_json(
        server,
        json!({"query": "shared-needle-alpha", "format": "json"}),
    )
    .await;
    assert_eq!(project_ids(&by_alias), ["search-alpha"]);

    let by_branch = search_json(server, json!({"query": "branch-alpha", "format": "json"})).await;
    assert_eq!(project_ids(&by_branch), ["search-alpha"]);

    let by_remote_name = search_json(
        server,
        json!({"query": "search-alpha-repo.git", "format": "json"}),
    )
    .await;
    assert_eq!(
        project_ids(&by_remote_name),
        ["search-alpha"],
        "the repository name is searchable; the credential must not be: {by_remote_name}"
    );

    let by_secret = search_json(server, json!({"query": SECRET, "format": "json"})).await;
    assert_payload(
        &by_secret,
        listing(SECRET, 10, false, &fixture.registry_path, vec![], vec![]),
    );

    let by_active = search_json(server, json!({"query": "branch-active", "format": "json"})).await;
    assert_payload(
        &by_active,
        listing(
            "branch-active",
            10,
            false,
            &fixture.registry_path,
            vec![public_project(&fixture.active, true)],
            vec![tree_group(&fixture.active, true)],
        ),
    );

    let markdown = search_text(
        server,
        json!({"query": "search-alpha", "format": "markdown"}),
    )
    .await;
    assert_eq!(
        markdown,
        markdown_hit("search-alpha", &fixture.alpha, false)
    );

    let active_markdown = search_text(
        server,
        json!({"query": "branch-active", "format": "markdown"}),
    )
    .await;
    assert_eq!(
        active_markdown,
        markdown_hit("branch-active", &fixture.active, true)
    );
}

#[tokio::test]
async fn project_search_bounds_pages_and_does_not_expand_wildcards() {
    let fixture = open_search_fixture().await;
    let server = fixture.server.as_ref();
    let both = newest_first(&[&fixture.alpha, &fixture.beta]);
    let page = search_json(
        server,
        json!({"query": "shared-needle", "limit": 1, "format": "json"}),
    )
    .await;
    assert_eq!(project_ids(&page).as_slice(), &both[..1]);
    assert_eq!(page["status"], "ok");
    assert_eq!(page["limit"], 1);
    assert_eq!(page["truncated"], true);
    assert_eq!(page["summary"]["project_count"], 1);
    assert_eq!(page["summary"]["repo_count"], 1);
    assert_eq!(page["summary"]["truncated"], true);
    assert_eq!(page["query"], "shared-needle");

    let clamped_low = search_json(
        server,
        json!({"query": "shared-needle", "limit": 0, "format": "json"}),
    )
    .await;
    assert_eq!(project_ids(&clamped_low).as_slice(), &both[..1]);
    assert_eq!(clamped_low["limit"], 1);
    assert_eq!(clamped_low["truncated"], true);

    let clamped_high = search_json(
        server,
        json!({"query": "shared-needle", "limit": 99, "format": "json"}),
    )
    .await;
    assert_eq!(project_ids(&clamped_high), both);
    assert_eq!(clamped_high["limit"], 50);
    assert_eq!(clamped_high["truncated"], false);
    assert_eq!(clamped_high["summary"]["project_count"], 2);
    assert_eq!(clamped_high["summary"]["repo_count"], 2);

    let either_term = search_json(
        server,
        json!({"query": "branch-alpha branch-beta", "format": "json"}),
    )
    .await;
    assert_eq!(project_ids(&either_term), both);
    assert_eq!(either_term["query"], "branch-alpha branch-beta");
    assert_eq!(
        either_term["title"],
        "projects matching \"branch-alpha branch-beta\""
    );

    let escaped_percent = search_json(server, json!({"query": "search-%", "format": "json"})).await;
    assert_payload(
        &escaped_percent,
        listing(
            "search-%",
            10,
            false,
            &fixture.registry_path,
            vec![],
            vec![],
        ),
    );

    let literal_percent = search_json(server, json!({"query": "%", "format": "json"})).await;
    assert_eq!(project_ids(&literal_percent), Vec::<String>::new());
    assert_eq!(literal_percent["status"], "ok");

    let blank = search_json(server, json!({"query": "   ", "format": "json"})).await;
    assert_eq!(project_ids(&blank), Vec::<String>::new());
    assert_eq!(blank["status"], "ok");
    assert_eq!(blank["title"], "projects matching \"   \"");

    let missing = search_json(
        server,
        json!({"query": "no-such-project-token", "format": "json"}),
    )
    .await;
    assert_payload(
        &missing,
        listing(
            "no-such-project-token",
            10,
            false,
            &fixture.registry_path,
            vec![],
            vec![],
        ),
    );
    let missing_markdown = search_text(
        server,
        json!({"query": "no-such-project-token", "format": "markdown"}),
    )
    .await;
    assert_eq!(
        missing_markdown,
        "No projects matching \"no-such-project-token\" found."
    );

    // The search ORs whitespace-separated tokens, so a spaced `OR` would be a
    // legitimate two-letter substring token that can match a random temp
    // path (`.tmpXoRyz`). Keep the quote breakout, drop the whitespace, so
    // the query is one token that no fixture field contains.
    let injected = search_json(
        server,
        json!({"query": "search-alpha'OR'1'='1", "format": "json"}),
    )
    .await;
    assert_eq!(project_ids(&injected), Vec::<String>::new());
    assert_eq!(injected["status"], "ok");
    assert_eq!(injected["query"], "search-alpha'OR'1'='1");
}

#[tokio::test]
async fn project_search_rejects_a_non_string_query_and_an_unmounted_registry() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let server = McpServer::new(
        TraceDecay::open_with_options(cg.project_root(), crate::support::graph_open_options(&cg))
            .await
            .expect("open calling project"),
        None,
    )
    .await;

    for arguments in [json!({}), json!({"query": 12}), json!({"query": null})] {
        let response = handle_real_server_tool_call_raw(&server, TOOL, arguments).await;
        assert_eq!(response["jsonrpc"], "2.0");
        assert!(response["result"].is_null(), "{response}");
        assert_eq!(
            response["error"],
            json!({
                "code": -32602,
                "message": "missing required parameter: query",
                "data": {
                    "tool": TOOL,
                    "reason_code": "missing_required_parameter",
                    "retryable": false,
                    "detail": "missing required parameter: query"
                }
            })
        );
    }

    let unavailable =
        search_json(&server, json!({"query": "search-alpha", "format": "json"})).await;
    assert_eq!(
        unavailable,
        json!({
            "status": "unavailable",
            "message": "project registry is not present for this profile",
            "projects": [],
            "title": "projects matching \"search-alpha\"",
            "summary": {
                "project_count": 0,
                "repo_count": 0,
                "truncated": false
            },
            "project_tree": [],
            "query": "search-alpha",
            "limit": 10,
            "truncated": false
        })
    );
}
