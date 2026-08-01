use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::sync::Arc;

use serde_json::json;
use tempfile::TempDir;

use super::super::get_tool_definitions;
use super::dispatch_test_support::*;
use super::*;
use crate::config::lock_user_data_dir_test_env;

/// `DiagnosticsRead` answers to two tool names, and the classifier only
/// declines the surface for one of them. The deferred name must land on a
/// group that owns a concrete handler; it previously resolved to nothing,
/// so every executor-less server answered `unknown tool`.
#[test]
fn diagnostics_without_an_executor_reaches_the_analysis_handler() {
    assert_eq!(
        classify_mcp_tool_dispatch_group("tracedecay_diagnostics", true),
        Some(McpToolDispatchGroup::ApplicationSurface),
    );
    assert_eq!(
        classify_mcp_tool_dispatch_group("tracedecay_diagnostics", false),
        Some(McpToolDispatchGroup::Analysis),
    );
    assert_eq!(
        dispatch_group_for_tool("tracedecay_diagnostics"),
        Some(McpToolDispatchGroup::Analysis),
        "the deferred lookup has no other table to resolve against",
    );
    for executor_available in [true, false] {
        assert_eq!(
            classify_mcp_tool_dispatch_group("tracedecay_diagnostics_read", executor_available),
            Some(McpToolDispatchGroup::ApplicationSurface),
            "the reviewed request shape has no in-process handler to fall back to",
        );
    }
}

/// The MCP deadline horizon asks this predicate which reads walk git, so it
/// must stay in step with the git dispatch family rather than a name list.
#[test]
fn git_dispatch_family_is_visible_to_the_server_horizon() {
    for tool_name in [
        "tracedecay_pr_context",
        "tracedecay_diff_context",
        "tracedecay_changelog",
        "tracedecay_branch_diff",
        "tracedecay_affected",
    ] {
        assert!(
            tool_dispatches_git_reads(tool_name),
            "{tool_name} dispatches through the git family",
        );
    }
    assert!(!tool_dispatches_git_reads("tracedecay_outline"));
    assert!(!tool_dispatches_git_reads("tracedecay_diagnostics"));
}

#[tokio::test]
async fn advertised_tools_resolve_one_concrete_dispatch_entry() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("dispatch-registry");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn dispatch_probe() {}\n").unwrap();
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-dispatch-registry",
    )
    .await
    .unwrap();
    let options = ToolCallRegistryOptions::default();
    let mut advertised = BTreeSet::new();

    for definition in get_tool_definitions() {
        assert!(
            advertised.insert(definition.name.clone()),
            "{} is advertised more than once",
            definition.name
        );
        // Both executor states, because the classifier defers a tool to a
        // different group when no application invocation executor is
        // attached. Probing only the attached state let the deferred group
        // resolve to nothing at all without failing this test.
        for executor_available in [true, false] {
            let group = classify_mcp_tool_dispatch_group(&definition.name, executor_available)
                .unwrap_or_else(|| {
                    panic!(
                        "{} has no production dispatch entry with executor_available={executor_available}",
                        definition.name
                    )
                });

            match group {
                McpToolDispatchGroup::ApplicationSurface => assert!(
                    ApplicationSurfaceOperation::from_tool_name(&definition.name).is_some(),
                    "{} has no application-surface handler entry",
                    definition.name
                ),
                McpToolDispatchGroup::RetainedApplication => {
                    let operation = RetainedSurfaceOperation::from_name(&definition.name)
                        .unwrap_or_else(|| {
                            panic!("{} has no retained-surface handler entry", definition.name)
                        });
                    let composition = retained_mcp_composition().unwrap_or_else(|error| {
                        panic!("{} catalog composition failed: {error}", definition.name)
                    });
                    let profile = ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).unwrap();
                    let operation_name = SurfaceOperationName::new(operation.as_str()).unwrap();
                    let capability = composition
                        .snapshot()
                        .resolve_binding(
                            &profile,
                            BindingSurface::Mcp,
                            &operation_name,
                            1,
                            &BTreeSet::new(),
                        )
                        .unwrap_or_else(|| {
                            panic!("{} catalog binding is not callable", definition.name)
                        });
                    let expected = retained_surface_application_operation(operation).unwrap();
                    assert_eq!(capability.capability_id(), expected.capability_id());
                    assert_eq!(capability.use_case_id(), expected.use_case_id());
                    assert!(
                        composition
                            .bind_handler(capability.use_case_id(), &())
                            .is_some(),
                        "{} application handler is not registered",
                        definition.name
                    );
                }
                group => {
                    assert_eq!(
                        dispatch_group_for_tool(&definition.name),
                        Some(group),
                        "{} does not resolve through the canonical MCP binding registry",
                        definition.name
                    );
                    assert!(
                        concrete_dispatch_group_accepts(
                            group,
                            &definition.name,
                            &cg,
                            options.clone()
                        )
                        .await,
                        "{} has no concrete handler-family entry",
                        definition.name
                    );
                }
            }
        }
    }

    for tool_name in [
        "tracedecay_not_registered",
        "tracedecay_lcm_not_registered",
        "tracedecay_multi_root_scope_set_read",
        "tracedecay_multi_root_scope_set_compare_and_swap",
        "tracedecay_multi_root_execute",
    ] {
        assert!(!advertised.contains(tool_name));
        for executor_available in [true, false] {
            assert_eq!(
                classify_mcp_tool_dispatch_group(tool_name, executor_available),
                None,
                "{tool_name} must fail closed with executor_available={executor_available}"
            );
        }
        let rejected = handle_tool_call_with_registry_and_implicit_project(
            &cg,
            tool_name,
            json!({}),
            None,
            None,
            options.clone(),
        )
        .await;
        assert!(
            rejected.is_err(),
            "{tool_name} must reject handler dispatch"
        );
    }
}

#[test]
fn graph_reader_selector_dispatch_policy_is_allowlisted() {
    for tool in get_tool_definitions() {
        let properties = &tool.input_schema["properties"];
        let schema_has_registered_project_selector =
            ["project_selector", "project_id", "project_path"]
                .iter()
                .any(|selector_key| properties.get(*selector_key).is_some());
        assert_eq!(
            tool_accepts_registered_project_selector(&tool.name),
            schema_has_registered_project_selector,
            "{} registered-project selector schema and dispatch policy should stay in lockstep",
            tool.name
        );
    }

    for tool_name in [
        // `tracedecay_search` resolves a daemon-owned code-index search
        // authority that is bound to the active project, so a selector
        // would run the active authority against a different graph.
        "tracedecay_search",
        "tracedecay_str_replace",
        "tracedecay_run_affected_tests",
        "tracedecay_status",
        "tracedecay_health",
        "tracedecay_dead_code",
    ] {
        assert!(
            !tool_accepts_registered_project_selector(tool_name),
            "{tool_name} should not be routed by the pure graph-reader selector policy"
        );
    }

    // Pure graph reads that need nothing but the selected project's graph
    // must accept a selector.
    for tool_name in [
        "tracedecay_type_hierarchy",
        "tracedecay_outline",
        "tracedecay_read",
        "tracedecay_body",
    ] {
        assert!(
            tool_accepts_registered_project_selector(tool_name),
            "{tool_name} should route through the graph-reader selector policy"
        );
    }
}

#[tokio::test]
async fn graph_reader_selector_dispatch_targets_registered_project() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let active_project = dir.path().join("active");
    let target_project = dir.path().join("target");
    fs::create_dir_all(active_project.join("src")).unwrap();
    fs::create_dir_all(target_project.join("src")).unwrap();
    fs::write(active_project.join("src/active.rs"), "pub fn active() {}\n").unwrap();
    fs::write(target_project.join("src/target.rs"), "pub fn target() {}\n").unwrap();

    let (active, _active_runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &active_project,
        "project.mcp-active-selector",
    )
    .await
    .unwrap();
    let (target, _target_runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &target_project,
        "project.mcp-target-selector",
    )
    .await
    .unwrap();
    let target = Arc::new(target);
    let target_still_stale = target
        .sync_if_stale(&["src/target.rs".to_string()])
        .await
        .unwrap();
    assert!(
        !target_still_stale,
        "target fixture source should be indexed for selected-project file listing"
    );
    let registry = SelectorRegistry::open().await;
    let target_project_id = target
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .expect("target project should be registered")
        .to_string();

    let result = handle_tool_call_with_registry_and_implicit_project(
        &active,
        "tracedecay_files",
        json!({
            "project_id": target_project_id,
            "path": "src"
        }),
        None,
        Some("tests"),
        selector_options(&registry, vec![Arc::clone(&target)]),
    )
    .await
    .unwrap();
    let text = result.value["content"][0]["text"].as_str().unwrap();

    assert!(
        text.contains("target.rs"),
        "selected registered project file listing should return target graph results: {text}"
    );
    assert!(
        !text.contains("active.rs"),
        "selected registered project file listing should not query the active graph: {text}"
    );

    active.checkpoint().await.unwrap();
    target.checkpoint().await.unwrap();
    active.close();
    Arc::into_inner(target)
        .expect("selector target graph should no longer be retained")
        .close();
}

#[tokio::test]
async fn graph_reader_selector_dispatch_accepts_unique_project_basename() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let active_project = dir.path().join("active");
    let target_project = dir.path().join("target");
    fs::create_dir_all(active_project.join("src")).unwrap();
    fs::create_dir_all(target_project.join("src")).unwrap();
    fs::write(active_project.join("src/active.rs"), "pub fn active() {}\n").unwrap();
    fs::write(target_project.join("src/target.rs"), "pub fn target() {}\n").unwrap();

    let (active, _active_runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &active_project,
        "project.mcp-active-basename",
    )
    .await
    .unwrap();
    let (target, _target_runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &target_project,
        "project.mcp-target-basename",
    )
    .await
    .unwrap();
    let target = Arc::new(target);
    target.index_all().await.unwrap();
    let registry = SelectorRegistry::open().await;

    let result = handle_tool_call_with_registry_and_implicit_project(
        &active,
        "tracedecay_grep",
        json!({
            "project_selector": {"path": "target"},
            "pattern": "target",
            "limit": 5,
        }),
        None,
        None,
        selector_options(&registry, vec![Arc::clone(&target)]),
    )
    .await
    .unwrap();
    let text = result.value["content"][0]["text"].as_str().unwrap();

    assert!(
        text.contains("target"),
        "unique basename selector should return target graph results: {text}"
    );
    assert!(
        !text.contains("active"),
        "unique basename selector should not query the active graph: {text}"
    );

    active.checkpoint().await.unwrap();
    target.checkpoint().await.unwrap();
    active.close();
    Arc::into_inner(target)
        .expect("basename target graph should no longer be retained")
        .close();
}

#[tokio::test]
async fn graph_reader_selector_rejects_ambiguous_project_basename() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let active_project = dir.path().join("active");
    let first_target = dir.path().join("first").join("target");
    let second_target = dir.path().join("second").join("target");
    fs::create_dir_all(&active_project).unwrap();
    fs::create_dir_all(&first_target).unwrap();
    fs::create_dir_all(&second_target).unwrap();

    let (active, _active_runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &active_project,
        "project.mcp-active-ambiguous",
    )
    .await
    .unwrap();
    let (first, _first_runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &first_target,
        "project.mcp-first-ambiguous",
    )
    .await
    .unwrap();
    let (second, _second_runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &second_target,
        "project.mcp-second-ambiguous",
    )
    .await
    .unwrap();
    let registry = SelectorRegistry::open().await;

    let err = handle_tool_call_with_registry_and_implicit_project(
        &active,
        "tracedecay_grep",
        json!({
            "project_selector": {"path": "target"},
            "pattern": "target",
        }),
        None,
        None,
        ToolCallRegistryOptions {
            global_db: Some(registry.database()),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert!(
        format!("{err}").contains("registered project not found for selector"),
        "ambiguous basename selector should be rejected: {err}"
    );

    active.checkpoint().await.unwrap();
    first.checkpoint().await.unwrap();
    second.checkpoint().await.unwrap();
    active.close();
    first.close();
    second.close();
}

#[tokio::test]
async fn status_and_runtime_share_cursor_session_ingest_authority() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("active");
    fs::create_dir_all(&project).unwrap();
    let (cg, runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-session-ingest-authority",
    )
    .await
    .unwrap();
    let database = runtime
        .registered_database(crate::application::host_admission::HostAdmissionScope::Project)
        .unwrap();
    let cursor_path = dir.path().join("cursor.jsonl");
    let claude_path = dir.path().join("claude.jsonl");
    fs::write(&cursor_path, b"0123456789").unwrap();
    fs::write(&claude_path, b"01234567890123456789").unwrap();
    for (provider, session_id, path) in [
        ("cursor", "session.cursor", cursor_path.as_path()),
        ("claude", "session.claude", claude_path.as_path()),
    ] {
        assert!(
            database
                .upsert_session(&crate::sessions::SessionRecord {
                    provider: provider.to_owned(),
                    session_id: session_id.to_owned(),
                    project_key: project.display().to_string(),
                    project_path: project.display().to_string(),
                    title: None,
                    started_at: None,
                    ended_at: None,
                    transcript_path: Some(path.display().to_string()),
                    metadata_json: None,
                    parent_session_id: None,
                    is_subagent: false,
                    agent_id: None,
                    parent_tool_use_id: None,
                })
                .await
        );
    }
    database
        .set_parse_offset(
            cursor_path.to_str().unwrap(),
            crate::global_db::ParseOffset {
                byte_offset: 4,
                mtime: 100,
                file_id: 0,
            },
        )
        .await
        .unwrap();
    database
        .set_parse_offset(
            claude_path.to_str().unwrap(),
            crate::global_db::ParseOffset {
                byte_offset: 20,
                mtime: 200,
                file_id: 0,
            },
        )
        .await
        .unwrap();
    let options = || ToolCallRegistryOptions {
        registered_project_session_db: runtime.registered_database_arc(
            crate::application::host_admission::HostAdmissionScope::Project,
        ),
        ..Default::default()
    };
    let status = handle_tool_call_with_registry_and_implicit_project(
        &cg,
        "tracedecay_status",
        json!({
            "format": "json",
            "include_branch_diagnostics": false,
            "include_storage_health": false,
            "include_staleness": false,
        }),
        None,
        None,
        options(),
    )
    .await
    .unwrap();
    let runtime_result = handle_tool_call_with_registry_and_implicit_project(
        &cg,
        "tracedecay_runtime",
        json!({
            "format": "json",
            "session_ingest_health": true,
        }),
        None,
        None,
        options(),
    )
    .await
    .unwrap();
    let parse = |result: ToolResult| {
        serde_json::from_str::<Value>(
            result.value["content"][0]["text"]
                .as_str()
                .expect("tool JSON text"),
        )
        .expect("parse tool JSON")
    };
    let status = parse(status);
    let runtime_result = parse(runtime_result);

    assert_eq!(
        status["session_ingest"],
        runtime_result["cursor_session_ingest"]
    );
    assert_eq!(status["session_ingest"]["tracked_transcripts"], 1);
    assert_eq!(status["session_ingest"]["pending_bytes"], 6);
    assert_eq!(status["session_ingest"]["last_ingest_unix"], 100);

    cg.checkpoint().await.unwrap();
    cg.close();
}

#[tokio::test]
async fn unsupported_selector_tool_rejects_explicit_project_selector() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("active");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn active_symbol() {}\n").unwrap();
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-unsupported-selector",
    )
    .await
    .unwrap();
    cg.index_all().await.unwrap();

    let err = handle_tool_call(
        &cg,
        "tracedecay_status",
        json!({
            "project_id": "explicit-selector-should-not-fall-open",
        }),
        None,
        None,
    )
    .await
    .expect_err("unsupported selector tools must reject explicit selectors");

    cg.checkpoint().await.unwrap();
    cg.close();

    assert!(
        format!("{err}").contains("does not accept project selectors"),
        "unexpected selector rejection error: {err}"
    );
}

#[tokio::test]
async fn query_search_rejects_cross_project_selector() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("active");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn active_symbol() {}\n").unwrap();
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-cross-selector",
    )
    .await
    .unwrap();

    let err = handle_tool_call(
        &cg,
        "tracedecay_search",
        json!({
            "project_id": "cross-project-must-not-be-relabelled",
            "query": "target",
        }),
        None,
        None,
    )
    .await
    .expect_err("single-root search must reject project selectors");

    cg.close();
    assert!(
        format!("{err}").contains("does not accept project selectors"),
        "unexpected selector rejection error: {err}"
    );
}

#[tokio::test]
async fn selected_project_retrieve_finds_selected_project_response_handle() {
    const LARGE_RESPONSE_MARKER_COUNT: usize = 200;
    const LAST_RETURNED_RESPONSE_MARKER: usize = 19;

    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let active_project = dir.path().join("active");
    let target_project = dir.path().join("target");
    fs::create_dir_all(active_project.join("src")).unwrap();
    fs::create_dir_all(target_project.join("src")).unwrap();
    fs::write(
        active_project.join("src/lib.rs"),
        "pub fn active_only_symbol() {}\n",
    )
    .unwrap();

    let mut target_source = String::new();
    let response_padding = "x".repeat(256);
    for i in 0..LARGE_RESPONSE_MARKER_COUNT {
        let _ = writeln!(
            target_source,
            "pub fn selected_project_handle_marker_{i:03}() -> &'static str {{ \"marker-{i:03}-{response_padding}\" }}"
        );
    }
    fs::write(target_project.join("src/lib.rs"), target_source).unwrap();

    let (active, _active_runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &active_project,
        "project.mcp-active-retrieval",
    )
    .await
    .unwrap();
    let (target, _target_runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &target_project,
        "project.mcp-target-retrieval",
    )
    .await
    .unwrap();
    let target = Arc::new(target);
    active.index_all().await.unwrap();
    target.index_all().await.unwrap();
    let target_project_id = target
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .expect("target project should be registered")
        .to_string();

    let registry = SelectorRegistry::open().await;
    let result = handle_tool_call_with_registry_and_implicit_project(
        &active,
        "tracedecay_grep",
        json!({
            "pattern": "selected_project_handle_marker",
            "project_id": target_project_id,
            "max_results": LARGE_RESPONSE_MARKER_COUNT,
            "context_lines": 3,
            "format": "json"
        }),
        None,
        None,
        selector_options(&registry, vec![Arc::clone(&target)]),
    )
    .await
    .unwrap();
    let envelope: Value = serde_json::from_str(
        result.value["content"][0]["text"]
            .as_str()
            .expect("search result text"),
    )
    .expect("truncated search envelope");
    assert_eq!(envelope["truncated"], true);
    let handle = envelope["handle"]
        .as_str()
        .expect("large selected-project search should return a handle");
    let retrieve_instruction = envelope["retrieve_instruction"]
        .as_str()
        .expect("truncated envelope should include retrieve guidance");
    assert!(
        retrieve_instruction.contains("pass the same selector"),
        "selected-project envelopes should tell clients to retrieve from the same project: {retrieve_instruction}"
    );

    let retrieved = handle_tool_call_with_registry_and_implicit_project(
        &active,
        "tracedecay_retrieve",
        json!({
            "handle": handle,
            "project_id": target.store_layout().identity.project_id.as_deref().unwrap(),
            "format": "json"
        }),
        None,
        None,
        selector_options(&registry, vec![Arc::clone(&target)]),
    )
    .await
    .unwrap();
    let payload: Value = serde_json::from_str(
        retrieved.value["content"][0]["text"]
            .as_str()
            .expect("retrieve result text"),
    )
    .expect("retrieve payload");

    assert_eq!(payload["expired"], false);
    assert!(
        payload["content"]
            .as_str()
            .is_some_and(|content| content.contains(&format!(
                "selected_project_handle_marker_{LAST_RETURNED_RESPONSE_MARKER:03}"
            ))),
        "selected project retrieve should return the full selected-project response: {payload}"
    );
}
