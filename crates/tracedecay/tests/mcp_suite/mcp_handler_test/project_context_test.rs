//! What a caller of `tracedecay_project_context` observes over MCP `tools/call`.
//!
//! Selectors, the served project, a missing registry, and a failed read are
//! the answers the tool returns. The assertions are those payloads, not the
//! selector parser or the registry port.

use crate::support::{
    extract_real_server_text, handle_real_server_tool_call, handle_real_server_tool_call_raw,
    setup_empty_project, test_temp_dir,
};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;

#[tokio::test]
async fn project_context_returns_the_project_the_caller_named() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let registry_dir = test_temp_dir();
    let alpha_root = registry_dir.path().join("alpha-checkout");
    fs::create_dir_all(&alpha_root).expect("alpha checkout");
    let active_project_id = cg
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("served project identity");
    let runtime =
        tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1::project_scoped(
            registry_dir.path(),
            cg.project_root(),
            tracedecay_domain::ProjectId::new(active_project_id.clone()).expect("project id"),
        )
        .await
        .expect("registered runtime");
    // The served checkout's live HEAD differs from its registered default
    // `main`: the context names each fact in its own field. The non-git alpha
    // has a registered default and no readable HEAD.
    crate::common::fixture::git_run(
        cg.project_root(),
        &["symbolic-ref", "HEAD", "refs/heads/served-head"],
    );
    let registry_path = runtime
        .profile_root_for_test()
        .join("global.db")
        .canonicalize()
        .expect("registry path")
        .display()
        .to_string();

    // The remote carries a credential. The tool's answer must describe the
    // public registry row and must not repeat that credential.
    let alpha = runtime
        .upsert_code_project(
            "proj_alpha",
            &alpha_root,
            None,
            Some("https://token:secret@example.test/alpha.git"),
            Some("main"),
        )
        .await
        .expect("seed proj_alpha");
    let named_alias = runtime
        .upsert_project_alias(Path::new("registered-alias"), "proj_alpha")
        .await
        .expect("seed alias");
    let store = runtime
        .upsert_store_instance(tracedecay_global_db::StoreInstanceUpsert {
            store_id: "store_alpha".to_string(),
            project_id: "proj_alpha".to_string(),
            store_kind: "code_project".to_string(),
            storage_mode: "profile_sharded".to_string(),
            store_relpath: "projects/proj_alpha".to_string(),
            manifest_relpath: Some("projects/proj_alpha/store_manifest.json".to_string()),
            last_verified_at: Some(1_800_000_001),
            last_write_at: None,
        })
        .await
        .expect("seed store");
    runtime
        .upsert_graph_scope(tracedecay_global_db::GraphScopeUpsert {
            graph_scope_id: "scope_alpha_main".to_string(),
            project_id: "proj_alpha".to_string(),
            store_id: "store_alpha".to_string(),
            branch_name: "main".to_string(),
            db_relpath: "projects/proj_alpha/tracedecay.db".to_string(),
            parent_scope_id: None,
            last_synced_at: Some(1_800_000_002),
            writable: true,
        })
        .await
        .expect("seed graph scope");
    runtime
        .upsert_store_artifact(tracedecay_global_db::StoreArtifactUpsert {
            store_id: "store_alpha".to_string(),
            artifact_kind: "graph_db".to_string(),
            relpath: "projects/proj_alpha/tracedecay.db".to_string(),
            size_bytes: Some(128),
            schema_version: Some("1".to_string()),
            updated_at: Some(1_800_000_003),
        })
        .await
        .expect("seed artifact");
    let active = runtime
        .upsert_code_project(
            &active_project_id,
            cg.project_root(),
            None,
            None,
            Some("main"),
        )
        .await
        .expect("seed served project");

    let server = tracedecay::mcp::McpServer::new_with_host_admission_test_runtime_for_test(
        tracedecay_project::project::TraceDecay::open_with_options(
            cg.project_root(),
            crate::support::graph_open_options(&cg),
        )
        .await
        .expect("open served project"),
        None,
        runtime,
    )
    .await
    .expect("mcp server");

    let alpha_context = json!({
        "status": "ok",
        "is_active": false,
        "registry_path": registry_path,
        "project": {
            "project_id": "proj_alpha",
            "label": "alpha-checkout",
            "project_root": alpha.display_root,
            "display_root": alpha.display_root,
            "canonical_root": alpha.canonical_root,
            "git_common_dir": null,
            "default_branch": "main",
            "head_branch": null,
            "created_at": alpha.created_at,
            "last_seen_at": alpha.last_seen_at,
            "is_active": false,
        },
        "aliases": sorted_aliases(vec![
            json!({
                "alias_path": alpha.canonical_root,
                "project_id": "proj_alpha",
                "last_seen_at": alpha.last_seen_at,
            }),
            json!({
                "alias_path": "git-remote-name:alpha.git",
                "project_id": "proj_alpha",
                "last_seen_at": alpha.last_seen_at,
            }),
            json!({
                "alias_path": named_alias.alias_path,
                "project_id": "proj_alpha",
                "last_seen_at": named_alias.last_seen_at,
            }),
        ]),
        "stores": [{
            "store": {
                "store_id": "store_alpha",
                "project_id": "proj_alpha",
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
                "project_id": "proj_alpha",
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
    });
    let by_id = call_project_context(
        &server,
        json!({"project_selector": {"project_id": "proj_alpha"}}),
    )
    .await;
    assert_eq!(by_id, alpha_context, "project id selector");
    assert!(
        !by_id.to_string().contains("secret") && !by_id.to_string().contains("token:"),
        "project context leaked the registered remote credential: {by_id}"
    );

    let by_alias = call_project_context(&server, json!({"path": "registered-alias"})).await;
    assert_eq!(
        by_alias, alpha_context,
        "a registered alias names the same project as its id"
    );

    let by_path = call_project_context(&server, json!({"path": alpha.display_root})).await;
    assert_eq!(
        by_path, alpha_context,
        "the registered checkout path names the same project"
    );

    let active_context = json!({
        "status": "ok",
        "is_active": true,
        "registry_path": registry_path,
        "project": {
            "project_id": active_project_id,
            "label": Path::new(&active.display_root)
                .file_name()
                .and_then(|name| name.to_str())
                .expect("served project label"),
            "project_root": active.display_root,
            "display_root": active.display_root,
            "canonical_root": active.canonical_root,
            "git_common_dir": null,
            "default_branch": "main",
            "head_branch": "served-head",
            "created_at": active.created_at,
            "last_seen_at": active.last_seen_at,
            "is_active": true,
        },
        "aliases": [{
            "alias_path": active.canonical_root,
            "project_id": active_project_id,
            "last_seen_at": active.last_seen_at,
        }],
        "stores": [],
    });
    let omitted = call_project_context(&server, json!({})).await;
    assert_eq!(
        omitted, active_context,
        "omitting the selector reads the served project"
    );
    let by_active_id = call_project_context(
        &server,
        json!({"project_selector": {"project_id": active_project_id}}),
    )
    .await;
    assert_eq!(
        by_active_id, active_context,
        "the served project id is the active project"
    );

    let not_found = json!({
        "status": "not_found",
        "registry_path": registry_path,
        "project": null,
        "aliases": [],
        "stores": [],
    });
    assert_eq!(
        call_project_context(
            &server,
            json!({"project_selector": {"project_id": "proj_missing"}}),
        )
        .await,
        not_found,
        "an unknown project id is not_found, not an empty project"
    );
    assert_eq!(
        call_project_context(&server, json!({"path": "unknown-alias"})).await,
        not_found,
        "an unregistered relative path does not adopt the served project"
    );
}

#[tokio::test]
async fn project_context_reports_an_unmounted_registry_as_unavailable() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let server = tracedecay::mcp::McpServer::new(
        tracedecay_project::project::TraceDecay::open_with_options(
            cg.project_root(),
            crate::support::graph_open_options(&cg),
        )
        .await
        .expect("open served project"),
        None,
    )
    .await;

    let payload = call_project_context(
        &server,
        json!({"project_selector": {"project_id": "proj_alpha"}}),
    )
    .await;

    assert_eq!(
        payload,
        json!({
            "status": "unavailable",
            "message": "project registry is not present for this profile",
            "projects": [],
        }),
        "a server with no registry port must not answer not_found"
    );
}

#[tokio::test]
async fn project_context_reports_a_broken_registry_read_as_a_tool_error() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let registry_dir = test_temp_dir();
    let registry_path = registry_dir.path().join("global.db");
    let runtime =
        tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1::project_scoped(
            registry_dir.path(),
            cg.project_root(),
            tracedecay_domain::ProjectId::new(
                cg.store_layout()
                    .identity
                    .project_id
                    .clone()
                    .expect("served project identity"),
            )
            .expect("project id"),
        )
        .await
        .expect("registered runtime");
    runtime
        .upsert_code_project("proj_broken", cg.project_root(), None, None, Some("main"))
        .await
        .expect("seed project");
    runtime
        .upsert_project_alias(Path::new("registered-alias"), "proj_broken")
        .await
        .expect("seed alias");
    rusqlite::Connection::open(&registry_path)
        .expect("open registry")
        .execute_batch("DROP TABLE project_aliases")
        .expect("drop aliases");
    let server = tracedecay::mcp::McpServer::new_with_host_admission_test_runtime_for_test(
        tracedecay_project::project::TraceDecay::open_with_options(
            cg.project_root(),
            crate::support::graph_open_options(&cg),
        )
        .await
        .expect("open served project"),
        None,
        runtime,
    )
    .await
    .expect("mcp server");

    let response = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_project_context",
        json!({"path": "registered-alias"}),
    )
    .await;

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
    assert!(
        response.get("result").is_none() || response["result"].is_null(),
        "a broken alias table must not become a successful context: {response}"
    );
}

async fn call_project_context(server: &tracedecay::mcp::McpServer, arguments: Value) -> Value {
    let result =
        handle_real_server_tool_call(server, "tracedecay_project_context", arguments).await;
    assert_eq!(
        result["content"][0]["type"], "text",
        "project context is a text tool result: {result}"
    );
    serde_json::from_str(extract_real_server_text(&result)).expect("project context JSON")
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
