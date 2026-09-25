//! `tracedecay_project_list` as an MCP client calls it: one `tools/call` on a
//! real server connection, then the text the caller observes.
//!
//! Timestamps are pinned after registration so the page order and the
//! expected clock fields are inputs, not a wall-clock reading.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;
use tracedecay_domain::ProjectId;
use tracedecay_global_db::{GraphScopeUpsert, StoreArtifactUpsert, StoreInstanceUpsert};
use tracedecay_mcp::McpTransport;
use tracedecay_project::project::TraceDecay;
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_runtime_core::path_safety::canonical_existing_identity;

use crate::support;

const SECRET_REMOTE: &str = "https://user:s3cret-token@git.example/alpha.git";
const ALPHA_CREATED_AT: i64 = 1_700_000_001;
const ALPHA_SEEN_AT: i64 = 1_700_000_020;
const BETA_CREATED_AT: i64 = 1_700_000_002;
const BETA_SEEN_AT: i64 = 1_700_000_030;
const ACTIVE_CREATED_AT: i64 = 1_700_000_003;
const ACTIVE_SEEN_AT: i64 = 1_700_000_010;

struct RegisteredProject {
    id: String,
    label: String,
    root: String,
    git_common_dir: Option<String>,
    branch: &'static str,
    branches: &'static [&'static str],
    kind: &'static str,
    created_at: i64,
    seen_at: i64,
    stores: usize,
    artifacts: usize,
    aliases: usize,
    active: bool,
}

#[tokio::test]
async fn project_list_returns_the_registry_page_the_caller_asked_for() {
    let (cg, _env, _project_dir) = support::setup_empty_project().await;
    let profile_dir = support::test_temp_dir();
    let profile_root = fs::canonicalize(profile_dir.path()).expect("profile root");
    let alpha_root = git_repository(&profile_root.join("listed-alpha"));
    let beta_root = directory(&profile_root.join("listed-beta"));

    {
        let runtime = HostAdmissionTestRuntimeV1::profile(&profile_root)
            .await
            .expect("profile registry");
        let alpha = runtime
            .upsert_code_project(
                "proj_alpha",
                &alpha_root,
                Some(&alpha_root.join(".git")),
                Some(SECRET_REMOTE),
                Some("main"),
            )
            .await
            .expect("register alpha");
        assert_eq!(
            alpha.git_remote_url.as_deref(),
            Some(SECRET_REMOTE),
            "the credential remote must be stored so its absence from the listing is real"
        );
        let store = runtime
            .upsert_store_instance(StoreInstanceUpsert {
                store_id: "store_alpha".to_string(),
                project_id: alpha.project_id.clone(),
                store_kind: "code_project".to_string(),
                storage_mode: "profile_sharded".to_string(),
                store_relpath: "projects/proj_alpha".to_string(),
                manifest_relpath: Some("projects/proj_alpha/store_manifest.json".to_string()),
                last_verified_at: Some(1_700_000_004),
                last_write_at: None,
            })
            .await
            .expect("register alpha store");
        runtime
            .upsert_graph_scope(GraphScopeUpsert {
                graph_scope_id: "scope_alpha_release".to_string(),
                project_id: alpha.project_id,
                store_id: store.store_id.clone(),
                branch_name: "release".to_string(),
                db_relpath: "projects/proj_alpha/tracedecay.db".to_string(),
                parent_scope_id: None,
                last_synced_at: Some(1_700_000_005),
                writable: true,
            })
            .await
            .expect("register alpha branch");
        runtime
            .upsert_store_artifact(StoreArtifactUpsert {
                store_id: store.store_id,
                artifact_kind: "graph_db".to_string(),
                relpath: "projects/proj_alpha/tracedecay.db".to_string(),
                size_bytes: Some(128),
                schema_version: Some("1".to_string()),
                updated_at: Some(1_700_000_006),
            })
            .await
            .expect("register alpha artifact");
        runtime
            .upsert_code_project("proj_beta", &beta_root, None, None, Some("dev"))
            .await
            .expect("register beta");
    }

    let active_id = cg
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("active project id");
    let active_project_id = ProjectId::new(active_id.clone()).expect("active project id");
    let runtime = HostAdmissionTestRuntimeV1::project_scoped(
        &profile_root,
        cg.project_root(),
        active_project_id,
    )
    .await
    .expect("project-scoped registry");
    let active_git =
        canonical_existing_identity(&cg.project_root().join(".git")).expect("active .git");
    assert_eq!(
        cg.project_root().join(".git"),
        active_git,
        "the calling checkout must be a primary repository"
    );
    runtime
        .upsert_code_project(
            &active_id,
            cg.project_root(),
            Some(active_git.as_path()),
            None,
            Some("main"),
        )
        .await
        .expect("register the calling project");

    let registry_path = runtime.profile_database_for_test().db_path().to_path_buf();
    pin_registration_times(&registry_path, &active_id);

    let alpha = registered(
        "proj_alpha",
        &alpha_root,
        Some(alpha_root.join(".git")),
        "main",
        &["main", "release"],
        "primary",
        ALPHA_CREATED_AT,
        ALPHA_SEEN_AT,
        1,
        1,
        3,
        false,
    );
    let beta = registered(
        "proj_beta",
        &beta_root,
        None,
        "dev",
        &["dev"],
        "project",
        BETA_CREATED_AT,
        BETA_SEEN_AT,
        0,
        0,
        1,
        false,
    );
    let active = registered(
        &active_id,
        cg.project_root(),
        Some(active_git),
        "main",
        &["main"],
        "primary",
        ACTIVE_CREATED_AT,
        ACTIVE_SEEN_AT,
        0,
        0,
        2,
        true,
    );
    assert!(
        active.label.starts_with(".tmp"),
        "the fixture temp dir must sort before listed-alpha, got {}",
        active.label
    );

    let server = McpServer::new_with_host_admission_test_runtime_for_test(
        TraceDecay::open(cg.project_root())
            .await
            .expect("reopen calling project"),
        None,
        runtime,
    )
    .await
    .expect("mcp server");
    let registry = registry_path.display().to_string();

    let default_text = tool_text(tools_call(&server, json!({"limit": 25})).await);
    assert_eq!(
        default_text,
        full_markdown(&active, &alpha, &beta),
        "omitted format is markdown"
    );
    let explicit_markdown =
        tool_text(tools_call(&server, json!({"format": "markdown", "limit": 25})).await);
    assert_eq!(explicit_markdown, full_markdown(&active, &alpha, &beta));

    let json_page = tool_json(tools_call(&server, json!({"format": "json", "limit": 25})).await);
    assert_eq!(
        json_page,
        listing_json(
            &registry,
            25,
            false,
            &[&beta, &alpha, &active],
            &[&active, &alpha, &beta]
        )
    );

    let widest = tool_json(tools_call(&server, json!({"format": "json", "limit": 250})).await);
    assert_eq!(
        widest,
        listing_json(
            &registry,
            100,
            false,
            &[&beta, &alpha, &active],
            &[&active, &alpha, &beta]
        ),
        "limit is clamped to 100"
    );

    let one = listing_json(&registry, 1, true, &[&beta], &[&beta]);
    let clamped = tool_json(tools_call(&server, json!({"format": "json", "limit": 0})).await);
    assert_eq!(clamped, one, "limit 0 is clamped to 1");
    let limited = tool_json(tools_call(&server, json!({"format": "json", "limit": 1})).await);
    assert_eq!(limited, one);
    let truncated_markdown =
        tool_text(tools_call(&server, json!({"format": "markdown", "limit": 1})).await);
    assert_eq!(truncated_markdown, truncated_beta_markdown(&beta));
}

#[tokio::test]
async fn project_list_reports_an_empty_registry_as_an_empty_listing() {
    let (cg, _env, _project_dir) = support::setup_empty_project().await;
    let profile_dir = support::test_temp_dir();
    let active_id = cg
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("active project id");
    let runtime = HostAdmissionTestRuntimeV1::project_scoped(
        profile_dir.path(),
        cg.project_root(),
        ProjectId::new(active_id).expect("active project id"),
    )
    .await
    .expect("empty registry");
    let registry_path = runtime
        .profile_database_for_test()
        .db_path()
        .display()
        .to_string();
    let server = McpServer::new_with_host_admission_test_runtime_for_test(
        TraceDecay::open(cg.project_root())
            .await
            .expect("reopen calling project"),
        None,
        runtime,
    )
    .await
    .expect("mcp server");

    let markdown = tool_text(tools_call(&server, json!({})).await);
    assert_eq!(markdown, "No registered projects found.");

    let payload = tool_json(tools_call(&server, json!({"format": "json"})).await);
    assert_eq!(
        payload,
        json!({
            "status": "ok",
            "title": "registered projects",
            "registry_path": registry_path,
            "limit": 25,
            "truncated": false,
            "summary": {
                "project_count": 0,
                "repo_count": 0,
                "truncated": false,
            },
            "project_tree": [],
            "projects": [],
        })
    );
}

#[tokio::test]
async fn project_list_reports_a_broken_registry_as_a_tool_error() {
    let (cg, _env, _project_dir) = support::setup_empty_project().await;
    let profile_dir = support::test_temp_dir();
    let profile_root = fs::canonicalize(profile_dir.path()).expect("profile root");
    let beta_root = directory(&profile_root.join("listed-beta"));
    {
        let runtime = HostAdmissionTestRuntimeV1::profile(&profile_root)
            .await
            .expect("profile registry");
        runtime
            .upsert_code_project("proj_beta", &beta_root, None, None, Some("dev"))
            .await
            .expect("register beta");
    }
    let active_id = cg
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("active project id");
    let runtime = HostAdmissionTestRuntimeV1::project_scoped(
        &profile_root,
        cg.project_root(),
        ProjectId::new(active_id).expect("active project id"),
    )
    .await
    .expect("project-scoped registry");
    let registry_path = runtime.profile_database_for_test().db_path().to_path_buf();
    rusqlite::Connection::open(&registry_path)
        .expect("open registry")
        .execute_batch("DROP TABLE project_aliases")
        .expect("drop project aliases");
    let server = McpServer::new_with_host_admission_test_runtime_for_test(
        TraceDecay::open(cg.project_root())
            .await
            .expect("reopen calling project"),
        None,
        runtime,
    )
    .await
    .expect("mcp server");

    let response = tools_call(&server, json!({"format": "json"})).await;
    assert_eq!(
        response,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {
                "code": -32603,
                "message": "tool execution failed: database error: SQLite prepare query failed: no such table: project_aliases (operation: resolve project identity alias)",
                "data": {
                    "tool": "tracedecay_project_list",
                    "cli_fallback": "This tool is also available from the shell: `tracedecay tool project_list ...` (`tracedecay tool project_list --help` for parameters). If MCP calls keep failing or timing out, fall back to that CLI instead of querying .tracedecay databases directly."
                }
            }
        })
    );
}

fn registered(
    id: &str,
    root: &Path,
    git_common_dir: Option<PathBuf>,
    branch: &'static str,
    branches: &'static [&'static str],
    kind: &'static str,
    created_at: i64,
    seen_at: i64,
    stores: usize,
    artifacts: usize,
    aliases: usize,
    active: bool,
) -> RegisteredProject {
    let root = root.display().to_string();
    RegisteredProject {
        id: id.to_string(),
        label: Path::new(&root)
            .file_name()
            .and_then(|name| name.to_str())
            .expect("project directory name")
            .to_string(),
        root,
        git_common_dir: git_common_dir.map(|path| path.display().to_string()),
        branch,
        branches,
        kind,
        created_at,
        seen_at,
        stores,
        artifacts,
        aliases,
        active,
    }
}

fn listing_json(
    registry: &str,
    limit: u64,
    truncated: bool,
    projects: &[&RegisteredProject],
    tree: &[&RegisteredProject],
) -> Value {
    json!({
        "status": "ok",
        "title": "registered projects",
        "registry_path": registry,
        "limit": limit,
        "truncated": truncated,
        "summary": {
            "project_count": projects.len(),
            "repo_count": tree.len(),
            "truncated": truncated,
        },
        "project_tree": tree.iter().copied().map(tree_group).collect::<Vec<_>>(),
        "projects": projects.iter().copied().map(project_row).collect::<Vec<_>>(),
    })
}

fn tree_group(project: &RegisteredProject) -> Value {
    json!({
        "label": project.label,
        "git_common_dir": project.git_common_dir,
        "project_count": 1,
        "branches": project.branches,
        "projects": [{
            "project_id": project.id,
            "label": project.label,
            "project_root": project.root,
            "canonical_root": project.root,
            "kind": project.kind,
            "default_branch": project.branch,
            "branches": project.branches,
            "store_count": project.stores,
            "artifact_count": project.artifacts,
            "alias_count": project.aliases,
            "last_seen_at": project.seen_at,
            "is_active": project.active,
        }],
    })
}

fn project_row(project: &RegisteredProject) -> Value {
    json!({
        "project_id": project.id,
        "label": project.label,
        "project_root": project.root,
        "display_root": project.root,
        "canonical_root": project.root,
        "git_common_dir": project.git_common_dir,
        "default_branch": project.branch,
        "created_at": project.created_at,
        "last_seen_at": project.seen_at,
        "is_active": project.active,
    })
}

fn full_markdown(
    active: &RegisteredProject,
    alpha: &RegisteredProject,
    beta: &RegisteredProject,
) -> String {
    format!(
        "Found 3 registered projects across 3 repositories.\n\nRepositories:\n\
         - {active_label} (branches: main)\n  \
         - `{active_id}` * [primary] branches: main; stores: 0; path: {active_root}\n\
         - listed-alpha (branches: main, release)\n  \
         - `proj_alpha` [primary] branches: main, release; stores: 1; path: {alpha_root}\n\
         - listed-beta (branches: dev)\n  \
         - `proj_beta` [project] branches: dev; stores: 0; path: {beta_root}\n",
        active_label = active.label,
        active_id = active.id,
        active_root = active.root,
        alpha_root = alpha.root,
        beta_root = beta.root,
    )
}

fn truncated_beta_markdown(beta: &RegisteredProject) -> String {
    format!(
        "Found 1 registered projects across 1 repositories.\n\nRepositories:\n\
         - listed-beta (branches: dev)\n  \
         - `proj_beta` [project] branches: dev; stores: 0; path: {path}\n\n\
         Result truncated; increase limit for more projects.\n",
        path = beta.root,
    )
}

fn pin_registration_times(registry_path: &Path, active_id: &str) {
    let connection = rusqlite::Connection::open(registry_path).expect("open registry");
    for (project_id, created_at, seen_at) in [
        ("proj_alpha", ALPHA_CREATED_AT, ALPHA_SEEN_AT),
        ("proj_beta", BETA_CREATED_AT, BETA_SEEN_AT),
        (active_id, ACTIVE_CREATED_AT, ACTIVE_SEEN_AT),
    ] {
        let updated = connection
            .execute(
                "UPDATE code_projects SET created_at = ?1, last_seen_at = ?2 WHERE project_id = ?3",
                rusqlite::params![created_at, seen_at, project_id],
            )
            .expect("pin registration times");
        assert_eq!(updated, 1, "missing registry row {project_id}");
    }
}

/// One request in, one response out, the same line framing `McpServer::run_connection`
/// serves to a client. Arguments are not rewritten: an omitted `format` stays omitted
/// and the default page is markdown, not JSON.
struct ClientCall {
    request: Option<String>,
    response: String,
}

impl McpTransport for ClientCall {
    async fn read_line(&mut self) -> std::io::Result<Option<String>> {
        Ok(self.request.take())
    }

    async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        self.response.push_str(line);
        Ok(())
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

async fn tools_call(server: &McpServer, arguments: Value) -> Value {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "tracedecay_project_list",
            "arguments": arguments,
        }
    });
    let mut transport = ClientCall {
        request: Some(request.to_string()),
        response: String::new(),
    };
    Box::pin(server.run_connection(&mut transport))
        .await
        .expect("mcp tools/call");
    serde_json::from_str(transport.response.trim()).expect("json-rpc response")
}

fn tool_text(response: Value) -> String {
    assert!(
        response.get("error").is_none(),
        "project_list failed: {response}"
    );
    let result = &response["result"];
    assert!(
        result.get("isError").is_none(),
        "a registry answer is not a tool error: {result}"
    );
    let content = result["content"].as_array().expect("content");
    assert_eq!(content.len(), 1, "one text block: {result}");
    assert_eq!(content[0]["type"], "text");
    content[0]["text"].as_str().expect("text").to_string()
}

fn tool_json(response: Value) -> Value {
    serde_json::from_str(&tool_text(response)).expect("project_list json")
}

fn directory(path: &Path) -> PathBuf {
    fs::create_dir_all(path).expect("create project directory");
    fs::canonicalize(path).expect("canonicalize project directory")
}

fn git_repository(path: &Path) -> PathBuf {
    let root = directory(path);
    crate::common::fixture::git_run(&root, &["init", "--quiet"]);
    root
}
