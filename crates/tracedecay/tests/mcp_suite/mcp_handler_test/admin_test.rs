use crate::support::*;
use serde_json::{Value, json};
#[cfg(feature = "test-transport")]
use std::fs;
#[cfg(feature = "test-transport")]
use std::path::{Path, PathBuf};
use tracedecay_mcp::get_tool_definitions;
#[cfg(feature = "test-transport")]
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn project_registry_tools_are_bounded_read_only_and_contextual() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let registry_dir = test_temp_dir();
    let registry_path = registry_dir.path().join("global.db");
    let seeded_project_root = registry_dir.path().join("registered-alpha-project");
    fs::create_dir_all(&seeded_project_root).unwrap();
    drop(seed_project_registry(&registry_path, &seeded_project_root).await);
    let active_project_id = cg
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("active project identity");
    let registry_runtime = HostAdmissionTestRuntimeV1::project_scoped(
        registry_dir.path(),
        cg.project_root(),
        tracedecay_domain::ProjectId::new(active_project_id.clone())
            .expect("test project identity"),
    )
    .await
    .unwrap();
    registry_runtime
        .upsert_code_project(
            &active_project_id,
            cg.project_root(),
            None,
            None,
            Some("main"),
        )
        .await
        .unwrap();
    let server = tracedecay::mcp::McpServer::new_with_host_admission_test_runtime_for_test(
        tracedecay_project::project::TraceDecay::open_with_options(
            cg.project_root(),
            crate::support::graph_open_options(&cg),
        )
        .await
        .unwrap(),
        None,
        registry_runtime,
    )
    .await
    .expect("registered test server");

    let list = handle_real_server_tool_call(
        &server,
        "tracedecay_project_list",
        json!({"limit": 1, "format": "json"}),
    )
    .await;
    let list_payload: Value = serde_json::from_str(extract_real_server_text(&list)).unwrap();
    assert_eq!(list_payload["projects"].as_array().unwrap().len(), 1);
    assert_eq!(list_payload["limit"], 1);
    assert_eq!(list_payload["truncated"], true);
    assert_eq!(list_payload["summary"]["project_count"], 1);
    assert_eq!(list_payload["project_tree"].as_array().unwrap().len(), 1);
    assert!(
        matches!(
            list_payload["projects"][0]["project_id"].as_str(),
            Some("proj_alpha" | "proj_beta")
        ) || list_payload["projects"][0]["project_id"] == active_project_id,
        "the bounded list must return one registered project: {list_payload}"
    );
    let list_text = extract_real_server_text(&list);
    assert!(
        !list_text.contains("secret") && !list_text.contains("git_remote_url"),
        "project list must not expose credential-bearing remotes: {list_text}"
    );
    let list_markdown = handle_real_server_tool_call(
        &server,
        "tracedecay_project_list",
        json!({"limit": 2, "format": "markdown"}),
    )
    .await;
    let list_markdown_text = extract_real_server_text(&list_markdown);
    assert!(
        list_markdown_text.contains("Repositories")
            && list_markdown_text.contains("branches: main"),
        "project list should render compact grouped markdown: {list_markdown_text}"
    );

    let search = handle_real_server_tool_call(
        &server,
        "tracedecay_project_search",
        json!({"query": "alpha", "limit": 10, "format": "json"}),
    )
    .await;
    let search_payload: Value = serde_json::from_str(extract_real_server_text(&search)).unwrap();
    let search_projects = search_payload["projects"].as_array().unwrap();
    assert_eq!(search_projects.len(), 1);
    assert_eq!(search_projects[0]["project_id"], "proj_alpha");
    assert_eq!(
        search_projects[0]["is_active"], false,
        "a separately registered project must not be marked active: {search_payload}"
    );
    assert_eq!(search_payload["project_tree"].as_array().unwrap().len(), 1);
    let search_text = extract_real_server_text(&search);
    assert!(
        !search_text.contains("secret") && !search_text.contains("git_remote_url"),
        "project search must not expose credential-bearing remotes: {search_text}"
    );

    let multi_term_search = handle_real_server_tool_call(
        &server,
        "tracedecay_project_search",
        json!({"query": "alpha beta", "limit": 10, "format": "json"}),
    )
    .await;
    let multi_term_payload: Value =
        serde_json::from_str(extract_real_server_text(&multi_term_search)).unwrap();
    let multi_term_ids: Vec<&str> = multi_term_payload["projects"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|project| project["project_id"].as_str())
        .collect();
    assert!(
        multi_term_ids.contains(&"proj_alpha") && multi_term_ids.contains(&"proj_beta"),
        "multi-term project search should match either term: {multi_term_payload}"
    );

    let remote_secret_search = handle_real_server_tool_call(
        &server,
        "tracedecay_project_search",
        json!({"query": "secret", "limit": 10, "format": "json"}),
    )
    .await;
    let remote_secret_payload: Value =
        serde_json::from_str(extract_real_server_text(&remote_secret_search)).unwrap();
    assert_eq!(
        remote_secret_payload["projects"].as_array().unwrap().len(),
        0,
        "project search must not match credential-bearing remote URL text: {remote_secret_payload}"
    );

    let context = handle_real_server_tool_call(
        &server,
        "tracedecay_project_context",
        json!({"project_selector": {"project_id": active_project_id}, "format": "json"}),
    )
    .await;
    let context_payload: Value = serde_json::from_str(extract_real_server_text(&context)).unwrap();
    assert_eq!(context_payload["project"]["project_id"], active_project_id);
    assert_eq!(
        context_payload["is_active"], true,
        "the calling project must be marked is_active in project context: {context_payload}"
    );
    assert_eq!(
        context_payload["project"]["is_active"], true,
        "the nested project record must also carry is_active: {context_payload}"
    );
    let context_text = extract_real_server_text(&context);
    assert!(
        !context_text.contains("secret") && !context_text.contains("git_remote_url"),
        "project context must not expose credential-bearing remotes: {context_text}"
    );
    let seeded_context = handle_real_server_tool_call(
        &server,
        "tracedecay_project_context",
        json!({"project_selector": {"project_id": "proj_alpha"}, "format": "json"}),
    )
    .await;
    let seeded_context_payload: Value =
        serde_json::from_str(extract_real_server_text(&seeded_context)).unwrap();
    assert_eq!(
        seeded_context_payload["stores"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        seeded_context_payload["stores"][0]["graph_scopes"][0]["branch_name"],
        "main"
    );
    assert_eq!(
        seeded_context_payload["stores"][0]["artifacts"][0]["artifact_kind"],
        "graph_db"
    );

    let alias_context = handle_real_server_tool_call(
        &server,
        "tracedecay_project_context",
        json!({"path": "registered-alias", "format": "json"}),
    )
    .await;
    let alias_payload: Value =
        serde_json::from_str(extract_real_server_text(&alias_context)).unwrap();
    assert_eq!(alias_payload["status"], "ok");
    assert_eq!(alias_payload["project"]["project_id"], "proj_alpha");
    assert_eq!(
        alias_payload["project"]["display_root"],
        seeded_project_root.to_string_lossy().as_ref()
    );

    let unknown_alias = handle_real_server_tool_call(
        &server,
        "tracedecay_project_context",
        json!({"path": "unknown-alias", "format": "json"}),
    )
    .await;
    let unknown_payload: Value =
        serde_json::from_str(extract_real_server_text(&unknown_alias)).unwrap();
    assert_eq!(unknown_payload["status"], "not_found");
    assert!(unknown_payload["project"].is_null());
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn project_context_surfaces_registry_read_failure_as_tool_error() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let registry_dir = test_temp_dir();
    let registry_path = registry_dir.path().join("global.db");
    let runtime =
        HostAdmissionTestRuntimeV1::project_scoped(registry_dir.path(), cg.project_root(), {
            cg.store_layout()
                .identity
                .project_id
                .as_deref()
                .and_then(|value| tracedecay_domain::ProjectId::new(value.to_string()).ok())
                .expect("test project identity")
        })
        .await
        .unwrap();
    runtime
        .upsert_code_project(
            "proj_broken_registry",
            cg.project_root(),
            None,
            None,
            Some("main"),
        )
        .await
        .unwrap();
    runtime
        .upsert_project_alias(Path::new("registered-alias"), "proj_broken_registry")
        .await
        .unwrap();
    rusqlite::Connection::open(&registry_path)
        .unwrap()
        .execute_batch("DROP TABLE project_aliases")
        .unwrap();
    let server = tracedecay::mcp::McpServer::new_with_host_admission_test_runtime_for_test(
        tracedecay_project::project::TraceDecay::open_with_options(
            cg.project_root(),
            crate::support::graph_open_options(&cg),
        )
        .await
        .unwrap(),
        None,
        runtime,
    )
    .await
    .expect("registered test server");

    let response = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_project_context",
        json!({"path": "registered-alias", "format": "json"}),
    )
    .await;

    assert_eq!(
        crate::support::tool_refusal(&response),
        json!({
            "kind": "execution_failed",
            "code": "graph_tool.failed",
            "message": "database error: SQLite prepare query failed: no such table: project_aliases (operation: resolve project identity alias)",
        })
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn project_search_surfaces_registry_read_failure_as_tool_error() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let registry_dir = test_temp_dir();
    let registry_path = registry_dir.path().join("global.db");
    let runtime =
        HostAdmissionTestRuntimeV1::project_scoped(registry_dir.path(), cg.project_root(), {
            cg.store_layout()
                .identity
                .project_id
                .as_deref()
                .and_then(|value| tracedecay_domain::ProjectId::new(value.to_string()).ok())
                .expect("test project identity")
        })
        .await
        .unwrap();
    runtime
        .upsert_code_project(
            "proj_broken_search",
            cg.project_root(),
            None,
            None,
            Some("main"),
        )
        .await
        .unwrap();
    rusqlite::Connection::open(&registry_path)
        .unwrap()
        .execute_batch("DROP TABLE project_aliases")
        .unwrap();
    let server = tracedecay::mcp::McpServer::new_with_host_admission_test_runtime_for_test(
        tracedecay_project::project::TraceDecay::open_with_options(
            cg.project_root(),
            crate::support::graph_open_options(&cg),
        )
        .await
        .unwrap(),
        None,
        runtime,
    )
    .await
    .expect("registered test server");

    let response = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_project_search",
        json!({"query": "broken", "format": "json"}),
    )
    .await;

    // A registry read failure never becomes a successful empty search.
    assert_eq!(
        crate::support::tool_refusal(&response),
        json!({
            "kind": "execution_failed",
            "code": "graph_tool.failed",
            "message": "database error: SQLite prepare query failed: no such table: project_aliases (operation: search registered code projects)",
        })
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn project_registry_tools_prefer_injected_registry_over_process_default() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let process_registry_dir = test_temp_dir();
    let client_registry_dir = test_temp_dir();
    let client_registry_path = client_registry_dir.path().join("global.db");

    let process_db = HostAdmissionTestRuntimeV1::profile(process_registry_dir.path())
        .await
        .unwrap();
    process_db
        .upsert_code_project(
            "proj_process_default",
            &cg.project_root().with_file_name("process-default"),
            None,
            None,
            Some("main"),
        )
        .await
        .unwrap();
    drop(process_db);
    let client_seed_root = client_registry_dir.path().join("registered-alpha-project");
    fs::create_dir_all(&client_seed_root).unwrap();
    seed_project_registry(&client_registry_path, &client_seed_root).await;
    let project_id = cg
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .and_then(|value| tracedecay_domain::ProjectId::new(value.to_string()).ok())
        .expect("test project identity");
    let client_runtime = HostAdmissionTestRuntimeV1::project_scoped(
        client_registry_dir.path(),
        cg.project_root(),
        project_id,
    )
    .await
    .unwrap();
    client_runtime
        .upsert_code_project(
            cg.store_layout()
                .identity
                .project_id
                .as_deref()
                .expect("active project identity"),
            cg.project_root(),
            None,
            None,
            Some("main"),
        )
        .await
        .unwrap();
    let server = tracedecay::mcp::McpServer::new_with_host_admission_test_runtime_for_test(
        tracedecay_project::project::TraceDecay::open_with_options(
            cg.project_root(),
            crate::support::graph_open_options(&cg),
        )
        .await
        .unwrap(),
        None,
        client_runtime,
    )
    .await
    .expect("registered test server");

    let list = handle_real_server_tool_call(
        &server,
        "tracedecay_project_list",
        json!({"limit": 10, "format": "json"}),
    )
    .await;
    let list_payload: Value = serde_json::from_str(extract_real_server_text(&list)).unwrap();
    assert_eq!(
        list_payload["registry_path"],
        client_registry_path
            .canonicalize()
            .unwrap()
            .display()
            .to_string()
    );
    let list_text = extract_real_server_text(&list);
    assert!(list_text.contains("proj_alpha"));
    assert!(
        !list_text.contains("proj_process_default"),
        "project list should not read process-default registry: {list_text}"
    );

    let search = handle_real_server_tool_call(
        &server,
        "tracedecay_project_search",
        json!({"query": "alpha", "limit": 10, "format": "json"}),
    )
    .await;
    let search_text = extract_real_server_text(&search);
    assert!(search_text.contains("proj_alpha"));
    assert!(
        !search_text.contains("proj_process_default"),
        "project search should not read process-default registry: {search_text}"
    );

    let context = handle_real_server_tool_call(
        &server,
        "tracedecay_project_context",
        json!({"project_selector": {"project_id": "proj_alpha"}, "format": "json"}),
    )
    .await;
    let context_payload: Value = serde_json::from_str(extract_real_server_text(&context)).unwrap();
    assert_eq!(context_payload["project"]["project_id"], "proj_alpha");
    assert_eq!(
        context_payload["registry_path"],
        client_registry_path
            .canonicalize()
            .unwrap()
            .display()
            .to_string()
    );
}

#[test]
fn active_project_and_storage_status_tools_are_advertised_readonly() {
    let tools = get_tool_definitions().expect("tool definitions");
    for name in ["tracedecay_active_project", "tracedecay_storage_status"] {
        let tool = tools
            .iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("missing MCP tool definition for {name}"));
        assert_eq!(tool.input_schema["type"], "object");
        assert!(
            tool.input_schema["properties"]
                .as_object()
                .is_some_and(|properties| properties
                    .keys()
                    .all(|key| key == "format" || key == "include_details")),
            "{name} should not require callers to pass resolver internals"
        );
        assert_eq!(
            tool.annotations
                .as_ref()
                .and_then(|annotations| annotations["readOnlyHint"].as_bool()),
            Some(true),
            "{name} must be advertised read-only"
        );
        assert!(
            !tool.description.contains(".tracedecay/tracedecay.db"),
            "{name} description must not hardcode the repo-local graph DB path"
        );
    }
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn storage_status_tool_summarizes_active_project_store_health() {
    let fixture = production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    let result =
        handle_real_server_tool_call(&server, "tracedecay_storage_status", json!({})).await;
    let payload: Value = serde_json::from_str(extract_real_server_text(&result)).unwrap();
    assert_eq!(
        payload["contract"]["schema_id"].as_str(),
        Some("schema.application.primitive.storage-status.result")
    );
    assert!(
        payload["scope"]["project_id"]
            .as_str()
            .is_some_and(|project_id| !project_id.is_empty())
    );
    assert!(
        payload["problem"].is_null() && !payload["outcome"].is_null(),
        "production invocation must return retained storage evidence: {payload}"
    );
    assert_eq!(payload["outcome"]["outcome"], json!("evidence"));
    assert_eq!(
        payload["outcome"]["value"]["payload"]["status"],
        json!("ok")
    );
    assert_eq!(
        payload["outcome"]["value"]["payload"]["project_id"], payload["scope"]["project_id"],
        "storage evidence must belong to the resolved production scope"
    );
    assert!(
        payload["outcome"]["value"]["payload"]["database_bytes"]
            .as_u64()
            .is_some_and(|bytes| bytes > 0),
        "production storage authority must report the retained database: {payload}"
    );
    fixture.harness.shutdown().await;
}

/// Page counts read from the admitted file itself, not from the tool.
#[cfg(feature = "test-transport")]
struct AdmittedStorePages {
    page_size_bytes: u32,
    page_count: u64,
    freelist_pages: u64,
}

#[cfg(feature = "test-transport")]
fn admitted_store_pages(path: &Path) -> AdmittedStorePages {
    let connection =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap_or_else(|error| {
                panic!("open admitted graph store {}: {error}", path.display())
            });
    let page_size: i64 = connection
        .pragma_query_value(None, "page_size", |row| row.get(0))
        .unwrap_or_else(|error| panic!("read page_size from {}: {error}", path.display()));
    let page_count: i64 = connection
        .pragma_query_value(None, "page_count", |row| row.get(0))
        .unwrap_or_else(|error| panic!("read page_count from {}: {error}", path.display()));
    let freelist_pages: i64 = connection
        .pragma_query_value(None, "freelist_count", |row| row.get(0))
        .unwrap_or_else(|error| panic!("read freelist_count from {}: {error}", path.display()));
    AdmittedStorePages {
        page_size_bytes: u32::try_from(page_size)
            .unwrap_or_else(|_| panic!("page_size {page_size} does not fit u32")),
        page_count: u64::try_from(page_count)
            .unwrap_or_else(|_| panic!("page_count {page_count} does not fit u64")),
        freelist_pages: u64::try_from(freelist_pages)
            .unwrap_or_else(|_| panic!("freelist_count {freelist_pages} does not fit u64")),
    }
}

#[cfg(feature = "test-transport")]
fn storage_status_envelope(result: &Value) -> Value {
    serde_json::from_str(extract_real_server_text(result))
        .unwrap_or_else(|error| panic!("storage status JSON: {error}"))
}

#[cfg(feature = "test-transport")]
fn assert_storage_status_matches_store(
    envelope: &Value,
    admitted_project_id: &str,
    store_path: &str,
    pages: &AdmittedStorePages,
) {
    let database_bytes = u64::from(pages.page_size_bytes).saturating_mul(pages.page_count);
    assert_eq!(envelope["problem"], Value::Null);
    assert_eq!(envelope["outcome"]["outcome"], json!("evidence"));
    assert_eq!(
        envelope["outcome"]["value"]["execution"]["termination"],
        json!("completed")
    );
    assert_eq!(envelope["scope"]["project_id"], json!(admitted_project_id));
    let payload = &envelope["outcome"]["value"]["payload"];
    assert_eq!(payload["status"], json!("ok"));
    assert_eq!(payload["read_only"], json!(false));
    assert_eq!(payload["details"], json!([]));
    assert_eq!(payload["project_id"], json!(admitted_project_id));
    assert_eq!(payload["store_path"], json!(store_path));
    assert_eq!(payload["page_size_bytes"], json!(pages.page_size_bytes));
    assert_eq!(payload["page_count"], json!(pages.page_count));
    assert_eq!(payload["freelist_pages"], json!(pages.freelist_pages));
    assert_eq!(payload["database_bytes"], json!(database_bytes));
    assert_eq!(
        payload["history_coverage"],
        json!("durable_project_store_history")
    );
    let history = payload["history"]
        .as_array()
        .unwrap_or_else(|| panic!("storage history must be an array: {payload}"));
    assert_eq!(history.len(), 1);
    assert_eq!(history[0]["database_bytes"], json!(database_bytes));
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn storage_status_reports_admitted_page_math_and_rejects_unknown_fields() {
    let fixture = production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    let admitted_project_id = fixture
        .harness
        .project_id(&fixture.project_root)
        .await
        .expect("admitted project identity");
    let store_path: PathBuf = server.cg().await.store_layout().graph_db_path.clone();
    let store_path = fs::canonicalize(&store_path)
        .unwrap_or_else(|error| panic!("canonicalize {}: {error}", store_path.display()));
    let store_path_text = store_path.display().to_string();
    let pages = admitted_store_pages(&store_path);

    let omitted =
        handle_real_server_tool_call(&server, "tracedecay_storage_status", json!({})).await;
    let omitted = storage_status_envelope(&omitted);
    assert_storage_status_matches_store(&omitted, &admitted_project_id, &store_path_text, &pages);
    let stable_history = omitted["outcome"]["value"]["payload"]["history"].clone();

    let detailed = handle_real_server_tool_call(
        &server,
        "tracedecay_storage_status",
        json!({"include_details": true}),
    )
    .await;
    let detailed = storage_status_envelope(&detailed);
    assert_storage_status_matches_store(&detailed, &admitted_project_id, &store_path_text, &pages);
    assert_eq!(
        detailed["outcome"]["value"]["payload"]["history"], stable_history,
        "an unchanged store must keep the first history sample"
    );
    assert_eq!(
        detailed["outcome"]["value"]["payload"]["details"],
        json!([])
    );

    let explicit = handle_real_server_tool_call(
        &server,
        "tracedecay_storage_status",
        json!({"include_details": false}),
    )
    .await;
    let explicit = storage_status_envelope(&explicit);
    assert_eq!(
        explicit["outcome"]["value"]["payload"]["history"], stable_history,
        "include_details false must not append a history sample"
    );
    assert_eq!(
        explicit["outcome"]["value"]["payload"]["status"],
        json!("ok")
    );
    assert_eq!(
        explicit["outcome"]["value"]["payload"]["details"],
        json!([])
    );

    let rejected = handle_real_server_tool_call_raw(
        &server,
        "tracedecay_storage_status",
        json!({"not_a_storage_field": true}),
    )
    .await;
    assert_eq!(
        crate::support::route_refusal(&rejected),
        crate::support::application_invalid_request_error(
            "unknown field `not_a_storage_field`, expected `include_details`"
        )
    );
    fixture.harness.shutdown().await;
}
