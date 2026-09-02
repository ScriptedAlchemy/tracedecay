use crate::support::*;
use serde_json::{Value, json};
#[cfg(feature = "test-transport")]
use std::fs;
#[cfg(feature = "test-transport")]
use std::path::Path;
#[cfg(feature = "test-transport")]
use tracedecay::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_mcp::get_tool_definitions;

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn project_registry_tools_are_bounded_read_only_and_contextual() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let registry_dir = test_temp_dir();
    let registry_path = registry_dir.path().join("global.db");
    let seeded_project_root = registry_dir.path().join("registered-alpha-project");
    fs::create_dir_all(&seeded_project_root).unwrap();
    let registry_runtime = seed_project_registry(&registry_path, &seeded_project_root).await;
    let active_project_id = cg
        .store_layout()
        .identity
        .project_id
        .clone()
        .expect("active project identity");
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
    let _env_guard = GlobalDbEnvGuard::set(&registry_path);

    let list = handle_tool_call_with_runtime(
        &cg,
        &registry_runtime,
        "tracedecay_project_list",
        json!({"limit": 1, "format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let list_payload: Value = serde_json::from_str(extract_text(&list.value)).unwrap();
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
    let list_text = extract_text(&list.value);
    assert!(
        !list_text.contains("secret") && !list_text.contains("git_remote_url"),
        "project list must not expose credential-bearing remotes: {list_text}"
    );
    let list_markdown = handle_tool_call_with_runtime(
        &cg,
        &registry_runtime,
        "tracedecay_project_list",
        json!({"limit": 2, "format": "markdown"}),
        None,
        None,
    )
    .await
    .unwrap();
    let list_markdown_text = extract_text(&list_markdown.value);
    assert!(
        list_markdown_text.contains("Repositories")
            && list_markdown_text.contains("branches: main"),
        "project list should render compact grouped markdown: {list_markdown_text}"
    );

    let search = handle_tool_call_with_runtime(
        &cg,
        &registry_runtime,
        "tracedecay_project_search",
        json!({"query": "alpha", "limit": 10, "format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let search_payload: Value = serde_json::from_str(extract_text(&search.value)).unwrap();
    let search_projects = search_payload["projects"].as_array().unwrap();
    assert_eq!(search_projects.len(), 1);
    assert_eq!(search_projects[0]["project_id"], "proj_alpha");
    assert_eq!(
        search_projects[0]["is_active"], false,
        "a separately registered project must not be marked active: {search_payload}"
    );
    assert_eq!(search_payload["project_tree"].as_array().unwrap().len(), 1);
    let search_text = extract_text(&search.value);
    assert!(
        !search_text.contains("secret") && !search_text.contains("git_remote_url"),
        "project search must not expose credential-bearing remotes: {search_text}"
    );

    let multi_term_search = handle_tool_call_with_runtime(
        &cg,
        &registry_runtime,
        "tracedecay_project_search",
        json!({"query": "alpha beta", "limit": 10, "format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let multi_term_payload: Value =
        serde_json::from_str(extract_text(&multi_term_search.value)).unwrap();
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

    let remote_secret_search = handle_tool_call_with_runtime(
        &cg,
        &registry_runtime,
        "tracedecay_project_search",
        json!({"query": "secret", "limit": 10, "format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let remote_secret_payload: Value =
        serde_json::from_str(extract_text(&remote_secret_search.value)).unwrap();
    assert_eq!(
        remote_secret_payload["projects"].as_array().unwrap().len(),
        0,
        "project search must not match credential-bearing remote URL text: {remote_secret_payload}"
    );

    let context = handle_tool_call_with_runtime(
        &cg,
        &registry_runtime,
        "tracedecay_project_context",
        json!({"project_selector": {"project_id": active_project_id}, "format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let context_payload: Value = serde_json::from_str(extract_text(&context.value)).unwrap();
    assert_eq!(context_payload["project"]["project_id"], active_project_id);
    assert_eq!(
        context_payload["is_active"], true,
        "the calling project must be marked is_active in project context: {context_payload}"
    );
    assert_eq!(
        context_payload["project"]["is_active"], true,
        "the nested project record must also carry is_active: {context_payload}"
    );
    let context_text = extract_text(&context.value);
    assert!(
        !context_text.contains("secret") && !context_text.contains("git_remote_url"),
        "project context must not expose credential-bearing remotes: {context_text}"
    );
    let seeded_context = handle_tool_call_with_runtime(
        &cg,
        &registry_runtime,
        "tracedecay_project_context",
        json!({"project_selector": {"project_id": "proj_alpha"}, "format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let seeded_context_payload: Value =
        serde_json::from_str(extract_text(&seeded_context.value)).unwrap();
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

    let alias_context = handle_tool_call_with_runtime(
        &cg,
        &registry_runtime,
        "tracedecay_project_context",
        json!({"path": "registered-alias", "format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let alias_payload: Value = serde_json::from_str(extract_text(&alias_context.value)).unwrap();
    assert_eq!(alias_payload["status"], "ok");
    assert_eq!(alias_payload["project"]["project_id"], "proj_alpha");
    assert_eq!(
        alias_payload["project"]["display_root"],
        seeded_project_root.to_string_lossy().as_ref()
    );

    let unknown_alias = handle_tool_call_with_runtime(
        &cg,
        &registry_runtime,
        "tracedecay_project_context",
        json!({"path": "unknown-alias", "format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let unknown_payload: Value = serde_json::from_str(extract_text(&unknown_alias.value)).unwrap();
    assert_eq!(unknown_payload["status"], "not_found");
    assert!(unknown_payload["project"].is_null());
}

/// When no project registry authority is mounted for the profile, all three
/// registry tools report `unavailable`. List and search still return the same
/// top-level keys as the ok-shape (`title`, `summary`, `project_tree`) with
/// zeroed/empty values, so callers can rely on a stable payload shape without
/// mistaking the unavailable authority for an authoritative empty result.
#[tokio::test]
async fn project_registry_tools_missing_registry_carries_stable_shape() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let registry_dir = test_temp_dir();
    // Point at a path with no file on disk so the registry resolves to "missing".
    let registry_path = registry_dir.path().join("does-not-exist.db");
    let _env_guard = GlobalDbEnvGuard::set(&registry_path);

    let list = handle_tool_call(
        &cg,
        "tracedecay_project_list",
        json!({"format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let list_payload: Value = serde_json::from_str(extract_text(&list.value)).unwrap();
    assert_eq!(list_payload["status"], "unavailable");
    assert_eq!(list_payload["title"], "registered projects");
    assert_eq!(list_payload["summary"]["project_count"], 0);
    assert_eq!(list_payload["summary"]["repo_count"], 0);
    assert_eq!(list_payload["summary"]["truncated"], false);
    assert_eq!(list_payload["project_tree"].as_array().unwrap().len(), 0);
    assert_eq!(list_payload["projects"].as_array().unwrap().len(), 0);

    let search = handle_tool_call(
        &cg,
        "tracedecay_project_search",
        json!({"query": "alpha", "format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let search_payload: Value = serde_json::from_str(extract_text(&search.value)).unwrap();
    assert_eq!(search_payload["status"], "unavailable");
    assert_eq!(search_payload["title"], "projects matching \"alpha\"");
    assert_eq!(search_payload["summary"]["project_count"], 0);
    assert_eq!(search_payload["summary"]["repo_count"], 0);
    assert_eq!(search_payload["summary"]["truncated"], false);
    assert_eq!(search_payload["project_tree"].as_array().unwrap().len(), 0);
    assert_eq!(search_payload["projects"].as_array().unwrap().len(), 0);

    let context = handle_tool_call(
        &cg,
        "tracedecay_project_context",
        json!({"project_selector": {"project_id": "project.missing"}, "format": "json"}),
        None,
        None,
    )
    .await
    .unwrap();
    let context_payload: Value = serde_json::from_str(extract_text(&context.value)).unwrap();
    assert_eq!(
        context_payload["status"], "unavailable",
        "an unmounted profile registry is not an authoritative not-found answer"
    );
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
        tracedecay::tracedecay::TraceDecay::open(cg.project_root())
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

    let message = response["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("registry read failure should surface as an error: {response}"));
    assert!(
        message.contains("resolve project identity alias") || message.contains("project_aliases"),
        "{message}"
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
        tracedecay::tracedecay::TraceDecay::open(cg.project_root())
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

    let message = response["error"]["message"].as_str().unwrap_or_else(|| {
        panic!("registry read failure must not become a successful empty search: {response}")
    });
    assert!(
        message.contains("search code projects") || message.contains("project_aliases"),
        "{message}"
    );
}

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn project_registry_tools_prefer_injected_registry_over_process_default() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let process_registry_dir = test_temp_dir();
    let process_registry_path = process_registry_dir.path().join("global.db");
    let client_registry_dir = test_temp_dir();
    let client_registry_path = client_registry_dir.path().join("global.db");
    let _env_guard = GlobalDbEnvGuard::set(&process_registry_path);

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
        tracedecay::tracedecay::TraceDecay::open(cg.project_root())
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

#[cfg(feature = "test-transport")]
#[tokio::test]
async fn selected_project_read_skips_cache_write_for_read_only_store() {
    let fixture = production_composition_fixture().await;
    let server = fixture
        .harness
        .server(&fixture.project_root)
        .expect("production project server");
    wait_for_current_graph(&server).await;
    let storage =
        handle_real_server_tool_call(&server, "tracedecay_storage_status", json!({})).await;
    let storage_payload: Value = serde_json::from_str(extract_real_server_text(&storage)).unwrap();
    let target_project_key = storage_payload["scope"]["project_id"]
        .as_str()
        .expect("production project identity")
        .to_owned();
    let read_args = json!({
        "project_selector": {"project_id": target_project_key},
        "file": "src/main.rs",
        "mode": "full",
        "format": "json"
    });
    let initial_read =
        handle_real_server_tool_call(&server, "tracedecay_read", read_args.clone()).await;
    let initial_payload = extract_first_json_content(&initial_read);
    assert_eq!(initial_payload["file"], "src/main.rs");
    assert!(
        initial_payload["body"]
            .as_str()
            .is_some_and(|body| body.contains("fn main()")),
        "initial selected read should return the mounted file content: {initial_payload}"
    );
    let initial_digest = initial_payload["digest"].clone();
    assert!(initial_digest.is_string(), "{initial_payload}");

    let unchanged_read = handle_real_server_tool_call(&server, "tracedecay_read", read_args).await;
    let unchanged_payload = extract_first_json_content(&unchanged_read);
    assert_eq!(unchanged_payload["file"], "src/main.rs");
    assert_eq!(
        unchanged_payload["unchanged"], true,
        "the successful second read must publish the canonical no-write result: {unchanged_payload}"
    );
    assert_eq!(
        unchanged_payload["digest"], initial_digest,
        "the no-write result must identify the exact content returned initially"
    );
    assert!(
        unchanged_payload["body"].is_null(),
        "an unchanged read must not duplicate the file body: {unchanged_payload}"
    );
    fixture.harness.shutdown().await;
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

#[tokio::test]
async fn active_project_tool_defaults_to_markdown() {
    let (cg, _env, _dir) = setup_empty_project().await;
    // Call the crate dispatch directly: the test-local wrapper injects
    // format:"json", and this test asserts the true default.
    let result =
        tracedecay::mcp::handle_tool_call(&cg, "tracedecay_active_project", json!({}), None, None)
            .await
            .unwrap();
    let text = extract_text(&result.value);
    assert!(
        serde_json::from_str::<Value>(text).is_err(),
        "default active_project output should be markdown, got: {text}"
    );
    assert!(
        text.contains("**project_root:**"),
        "markdown field missing: {text}"
    );
}

#[tokio::test]
async fn active_project_tool_reports_resolved_store_metadata() {
    let (cg, _env, _dir) = setup_empty_project().await;
    let project_root = cg.project_root().display().to_string();
    let graph_db_path = cg.db_path().display().to_string();

    let result = handle_tool_call(
        &cg,
        "tracedecay_active_project",
        json!({}),
        Some(json!({"transport": "stdio"})),
        Some("src"),
    )
    .await
    .unwrap();

    let payload: Value = serde_json::from_str(extract_text(&result.value)).unwrap();
    assert_eq!(
        payload["project_root"].as_str(),
        Some(project_root.as_str())
    );
    assert_eq!(payload["scope_prefix"].as_str(), Some("src"));
    assert_eq!(
        payload["resolution_source"].as_str(),
        Some("active_project")
    );
    assert_eq!(payload["storage"]["class"].as_str(), Some("code_project"));
    assert_eq!(payload["storage"]["mode"].as_str(), Some("profile_sharded"));
    assert_eq!(
        payload["storage"]["graph_db_path"].as_str(),
        Some(graph_db_path.as_str())
    );
    assert!(
        payload["storage"]["data_root"]
            .as_str()
            .is_some_and(|path| path.contains(".tracedecay") && path.contains("projects"))
    );
    assert_eq!(payload["storage"]["graph_db_exists"].as_bool(), Some(true));
    assert!(payload["branch"].get("serving_db_path").is_none());
    assert!(payload["branch"].get("serving_db_exists").is_none());
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
