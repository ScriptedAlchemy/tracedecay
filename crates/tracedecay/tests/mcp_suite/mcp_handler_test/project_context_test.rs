//! `tracedecay_project_context` as a caller sees it: one real MCP `tools/call`
//! against a registered profile registry.
//!
//! Timestamps and canonical paths are the facts the registration wrote. The
//! tool must echo those facts and must not invent an active project, a hit,
//! or a credential-bearing remote.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use tracedecay::mcp::McpServer;
use tracedecay::project::TraceDecay;
use tracedecay::test_support::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_domain::ProjectId;
use tracedecay_global_db::{
    CodeProjectRecord, GraphScopeUpsert, ProjectAliasRecord, StoreArtifactUpsert,
    StoreInstanceRecord, StoreInstanceUpsert,
};

use crate::support::{
    extract_real_server_text, handle_real_server_tool_call, handle_real_server_tool_call_raw,
    setup_empty_project, test_temp_dir,
};

const ALPHA_ID: &str = "proj_alpha";
const ALPHA_DIR: &str = "registered-alpha-project";
const ALPHA_ALIAS: &str = "registered-alias";
const SECRET_REMOTE: &str = "https://token:secret@example.test/alpha.git";

struct RegisteredContext {
    server: std::sync::Arc<McpServer>,
    registry_path: String,
    alpha: Value,
    active: Value,
}

async fn project_context(server: &McpServer, arguments: Value) -> Value {
    let result =
        handle_real_server_tool_call(server, "tracedecay_project_context", arguments).await;
    let text = extract_real_server_text(&result);
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("project context text is not JSON ({error}): {text}"))
}

fn registry_db_path(profile_root: &Path) -> std::path::PathBuf {
    profile_root.join("global.db")
}

fn not_found(registry_path: &str) -> Value {
    json!({
        "status": "not_found",
        "registry_path": registry_path,
        "project": null,
        "aliases": [],
        "stores": [],
    })
}

fn sorted_aliases(mut aliases: Vec<Value>) -> Vec<Value> {
    aliases.sort_by(|left, right| {
        left["alias_path"]
            .as_str()
            .unwrap_or("")
            .cmp(right["alias_path"].as_str().unwrap_or(""))
    });
    aliases
}

fn public_project(project: &CodeProjectRecord, label: &str, is_active: bool) -> Value {
    json!({
        "project_id": project.project_id,
        "label": label,
        "project_root": project.display_root,
        "display_root": project.display_root,
        "canonical_root": project.canonical_root,
        "git_common_dir": project.git_common_dir,
        "default_branch": project.default_branch,
        "created_at": project.created_at,
        "last_seen_at": project.last_seen_at,
        "is_active": is_active,
    })
}

fn alias_row(alias_path: &str, project_id: &str, last_seen_at: i64) -> Value {
    json!({
        "alias_path": alias_path,
        "project_id": project_id,
        "last_seen_at": last_seen_at,
    })
}

fn alpha_context(
    registry_path: &str,
    project: &CodeProjectRecord,
    alias: &ProjectAliasRecord,
    store: &StoreInstanceRecord,
) -> Value {
    json!({
        "status": "ok",
        "is_active": false,
        "registry_path": registry_path,
        "project": public_project(project, ALPHA_DIR, false),
        "aliases": sorted_aliases(vec![
            alias_row(&project.canonical_root, ALPHA_ID, project.last_seen_at),
            alias_row("git-remote-name:alpha.git", ALPHA_ID, project.last_seen_at),
            alias_row(&alias.alias_path, ALPHA_ID, alias.last_seen_at),
        ]),
        "stores": [{
            "store": {
                "store_id": "store_alpha",
                "project_id": ALPHA_ID,
                "store_kind": "code_project",
                "storage_mode": "profile_sharded",
                "store_relpath": "projects/proj_alpha",
                "manifest_relpath": "projects/proj_alpha/store_manifest.json",
                "created_at": store.created_at,
                "last_verified_at": 1_800_000_001,
                "last_write_at": null,
            },
            "graph_scopes": [{
                "graph_scope_id": "scope_alpha_main",
                "project_id": ALPHA_ID,
                "store_id": "store_alpha",
                "branch_name": "main",
                "db_relpath": "projects/proj_alpha/tracedecay.db",
                "parent_scope_id": null,
                "last_synced_at": 1_800_000_002,
                "writable": true,
            }],
            "artifacts": [{
                "store_id": "store_alpha",
                "artifact_kind": "graph_db",
                "relpath": "projects/proj_alpha/tracedecay.db",
                "size_bytes": 128,
                "schema_version": "1",
                "updated_at": 1_800_000_003,
            }],
        }],
    })
}

fn active_context(registry_path: &str, project: &CodeProjectRecord) -> Value {
    let label = Path::new(&project.display_root)
        .file_name()
        .and_then(|name| name.to_str())
        .expect("active project display root has a file name");
    json!({
        "status": "ok",
        "is_active": true,
        "registry_path": registry_path,
        "project": public_project(project, label, true),
        "aliases": [alias_row(&project.canonical_root, &project.project_id, project.last_seen_at)],
        "stores": [],
    })
}

async fn open_registered_context() -> RegisteredContext {
    let (cg, _env, _dir) = setup_empty_project().await;
    let registry_dir = test_temp_dir();
    let seeded_root = registry_dir.path().join(ALPHA_DIR);
    fs::create_dir_all(&seeded_root).expect("seeded project directory");
    let project_id = cg
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .and_then(|value| ProjectId::new(value.to_string()).ok())
        .expect("test project identity");
    let active_id = project_id.as_str().to_owned();
    let runtime = HostAdmissionTestRuntimeV1::project_scoped(
        registry_dir.path(),
        cg.project_root(),
        project_id,
    )
    .await
    .expect("project-scoped registry runtime");
    let active = runtime
        .upsert_code_project(&active_id, cg.project_root(), None, None, Some("main"))
        .await
        .expect("register the connected project");
    let alpha = runtime
        .upsert_code_project(
            ALPHA_ID,
            &seeded_root,
            None,
            Some(SECRET_REMOTE),
            Some("main"),
        )
        .await
        .expect("register proj_alpha");
    let alias = runtime
        .upsert_project_alias(Path::new(ALPHA_ALIAS), ALPHA_ID)
        .await
        .expect("register proj_alpha alias");
    let store = runtime
        .upsert_store_instance(StoreInstanceUpsert {
            store_id: "store_alpha".to_string(),
            project_id: ALPHA_ID.to_string(),
            store_kind: "code_project".to_string(),
            storage_mode: "profile_sharded".to_string(),
            store_relpath: "projects/proj_alpha".to_string(),
            manifest_relpath: Some("projects/proj_alpha/store_manifest.json".to_string()),
            last_verified_at: Some(1_800_000_001),
            last_write_at: None,
        })
        .await
        .expect("register proj_alpha store");
    runtime
        .upsert_graph_scope(GraphScopeUpsert {
            graph_scope_id: "scope_alpha_main".to_string(),
            project_id: ALPHA_ID.to_string(),
            store_id: "store_alpha".to_string(),
            branch_name: "main".to_string(),
            db_relpath: "projects/proj_alpha/tracedecay.db".to_string(),
            parent_scope_id: None,
            last_synced_at: Some(1_800_000_002),
            writable: true,
        })
        .await
        .expect("register proj_alpha graph scope");
    runtime
        .upsert_store_artifact(StoreArtifactUpsert {
            store_id: "store_alpha".to_string(),
            artifact_kind: "graph_db".to_string(),
            relpath: "projects/proj_alpha/tracedecay.db".to_string(),
            size_bytes: Some(128),
            schema_version: Some("1".to_string()),
            updated_at: Some(1_800_000_003),
        })
        .await
        .expect("register proj_alpha artifact");
    let registry_file = registry_db_path(registry_dir.path());
    let registry_path = registry_file
        .canonicalize()
        .expect("registry database exists")
        .display()
        .to_string();
    let server = McpServer::new_with_host_admission_test_runtime_for_test(
        TraceDecay::open(cg.project_root())
            .await
            .expect("open the connected project"),
        None,
        runtime,
    )
    .await
    .expect("MCP server bound to the test registry");
    RegisteredContext {
        alpha: alpha_context(&registry_path, &alpha, &alias, &store),
        active: active_context(&registry_path, &active),
        registry_path,
        server,
    }
}

#[tokio::test]
async fn project_context_reports_the_registered_project_a_caller_names() {
    let fixture = open_registered_context().await;

    let by_id = project_context(
        &fixture.server,
        json!({
            "project_selector": {"project_id": ALPHA_ID},
            "format": "json",
        }),
    )
    .await;
    assert_eq!(by_id, fixture.alpha, "project_id selector");
    assert!(
        !by_id.to_string().contains("token:secret"),
        "project context must not echo the credential-bearing remote: {by_id}"
    );

    let by_alias = project_context(
        &fixture.server,
        json!({"path": ALPHA_ALIAS, "format": "json"}),
    )
    .await;
    assert_eq!(by_alias, fixture.alpha, "registered alias");

    let by_checkout = project_context(
        &fixture.server,
        json!({
            "path": fixture.alpha["project"]["display_root"],
            "format": "json",
        }),
    )
    .await;
    assert_eq!(by_checkout, fixture.alpha, "absolute checkout path");

    let selector_wins = project_context(
        &fixture.server,
        json!({
            "project_selector": {"project_id": ALPHA_ID},
            "path": "missing-alias",
            "format": "json",
        }),
    )
    .await;
    assert_eq!(
        selector_wins, fixture.alpha,
        "project_selector wins over a path that does not resolve"
    );

    let connected = project_context(&fixture.server, json!({"format": "json"})).await;
    assert_eq!(
        connected, fixture.active,
        "omitting both selectors reads the connected project"
    );

    let empty_selector = project_context(
        &fixture.server,
        json!({"project_selector": {}, "format": "json"}),
    )
    .await;
    assert_eq!(
        empty_selector, fixture.active,
        "a project_selector without project_id is not a name"
    );
}

#[tokio::test]
async fn project_context_reports_not_found_for_a_name_the_registry_does_not_hold() {
    let fixture = open_registered_context().await;
    let expected = not_found(&fixture.registry_path);

    let unknown_alias = project_context(
        &fixture.server,
        json!({"path": "unknown-alias", "format": "json"}),
    )
    .await;
    assert_eq!(unknown_alias, expected, "unknown alias");

    let id_as_path =
        project_context(&fixture.server, json!({"path": ALPHA_ID, "format": "json"})).await;
    assert_eq!(
        id_as_path, expected,
        "a project id passed as path is not an alias lookup"
    );

    let unknown_id = project_context(
        &fixture.server,
        json!({
            "project_selector": {"project_id": "project.missing"},
            "format": "json",
        }),
    )
    .await;
    assert_eq!(unknown_id, expected, "unknown project id");

    let stranger = test_temp_dir();
    let stranger_path = stranger
        .path()
        .canonicalize()
        .expect("unregistered directory")
        .display()
        .to_string();
    let unregistered_checkout = project_context(
        &fixture.server,
        json!({"path": stranger_path, "format": "json"}),
    )
    .await;
    assert_eq!(
        unregistered_checkout, expected,
        "an unregistered absolute path is not_found, not a tool failure"
    );
}

#[tokio::test]
async fn project_context_reports_unavailable_when_no_registry_is_mounted() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let server = McpServer::new(
        TraceDecay::open(cg.project_root())
            .await
            .expect("open the connected project"),
        None,
    )
    .await;

    let payload = project_context(
        &server,
        json!({
            "project_selector": {"project_id": "project.missing"},
            "format": "json",
        }),
    )
    .await;

    assert_eq!(
        payload,
        json!({
            "status": "unavailable",
            "message": "project registry is not present for this profile",
            "projects": [],
        })
    );
}

#[tokio::test]
async fn project_context_surfaces_a_broken_registry_read_as_a_tool_error() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let registry_dir = test_temp_dir();
    let registry_path = registry_db_path(registry_dir.path());
    let project_id = cg
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .and_then(|value| ProjectId::new(value.to_string()).ok())
        .expect("test project identity");
    let runtime = HostAdmissionTestRuntimeV1::project_scoped(
        registry_dir.path(),
        cg.project_root(),
        project_id,
    )
    .await
    .expect("project-scoped registry runtime");
    runtime
        .upsert_code_project(
            "proj_broken_registry",
            cg.project_root(),
            None,
            None,
            Some("main"),
        )
        .await
        .expect("register a project before dropping its alias table");
    runtime
        .upsert_project_alias(Path::new(ALPHA_ALIAS), "proj_broken_registry")
        .await
        .expect("alias write must succeed before the table is dropped");
    rusqlite::Connection::open(&registry_path)
        .expect("open the test registry")
        .execute_batch("DROP TABLE project_aliases")
        .expect("drop project_aliases");
    let server = McpServer::new_with_host_admission_test_runtime_for_test(
        TraceDecay::open(cg.project_root())
            .await
            .expect("open the connected project"),
        None,
        runtime,
    )
    .await
    .expect("MCP server bound to the broken registry");

    let response = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_project_context",
        json!({"path": ALPHA_ALIAS, "format": "json"}),
    )
    .await;

    assert!(response.get("result").is_none(), "{response}");
    assert_eq!(response["error"]["code"], -32603, "{response}");
    assert_eq!(
        response["error"]["data"]["tool"], "tracedecay_project_context",
        "{response}"
    );
    assert_eq!(
        response["error"]["message"],
        "tool execution failed: database error: SQLite prepare query failed: no such table: project_aliases (operation: resolve project identity alias)",
        "{response}"
    );
}
