use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::sync::Mutex;

use serde_json::{Value, json};
use tempfile::TempDir;

use super::super::get_tool_definitions;
use super::dispatch_test_support::*;
use super::*;
use crate::config::lock_user_data_dir_test_env;

/// Records the daemon operation every multi-root tool routes to, then refuses
/// it. The refusal is the point: it proves the MCP name reached the closed
/// daemon route rather than some in-process handler.
#[derive(Default)]
struct RecordingMultiRootExecutor {
    operations: Mutex<Vec<tracedecay_daemon_protocol::DaemonInvocationOperation>>,
}

impl tracedecay_application::ApplicationInvocationExecutor for RecordingMultiRootExecutor {
    fn invoke(
        &self,
        _invocation: tracedecay_application::ApplicationInvocation,
    ) -> tracedecay_application::ApplicationInvocationFuture<
        '_,
        std::result::Result<
            tracedecay_application::ApplicationResponse,
            tracedecay_application::InvocationError,
        >,
    > {
        Box::pin(async { Err(tracedecay_application::InvocationError::Unavailable) })
    }
}

impl tracedecay_daemon_protocol::DaemonInvocationExecutor for RecordingMultiRootExecutor {
    fn invoke_controlled(
        &self,
        request: tracedecay_daemon_protocol::DaemonInvocationRequest,
        _deadline: tracedecay_application::Deadline,
        _cancellation: tracedecay_application::CancellationSignal,
        _policy: tracedecay_daemon_protocol::InvocationCancellationPolicy,
    ) -> tracedecay_daemon_protocol::DaemonInvocationExecutorFuture<
        '_,
        std::result::Result<
            tracedecay_daemon_protocol::DaemonInvocationResponse,
            tracedecay_daemon_protocol::DaemonInvocationError,
        >,
    > {
        self.operations
            .lock()
            .expect("recorded daemon operations")
            .push(request.operation());
        Box::pin(async { Err(tracedecay_daemon_protocol::DaemonInvocationError::Unavailable) })
    }

    fn observe_feedback(
        &self,
        _subject_digest: tracedecay_domain::ManifestDigest,
        _observed_at: tracedecay_domain::UtcMicros,
        _event: tracedecay_application::feedback::observations::FeedbackSourceEventV1,
    ) -> tracedecay_daemon_protocol::DaemonInvocationExecutorFuture<
        '_,
        tracedecay_domain::errors::Result<()>,
    > {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn multi_root_tools_invoke_the_closed_daemon_routes() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("multi-root-mcp");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn mcp_multi_root() {}\n").unwrap();
    let (cg, _runtime) =
        TraceDecay::init_test_fixture_with_registered_runtime(&project, "project.mcp-multi-root")
            .await
            .unwrap();
    let executor = RecordingMultiRootExecutor::default();
    let requests = [
        (
            "tracedecay_multi_root_scope_set_read",
            json!({"scope_set_id": "scope-set.mcp"}),
        ),
        (
            "tracedecay_multi_root_scope_set_compare_and_swap",
            json!({
                "scope_set_id": "scope-set.mcp",
                "expected_revision": null,
                "roots": [{"project_id": "project.mcp-root", "root": "/project/mcp-root"}]
            }),
        ),
        (
            "tracedecay_multi_root_execute",
            json!({
                "scope_set_id": "scope-set.mcp",
                "scope_set_revision": 1,
                "scope_set_digest": format!("sha256:{}", "a".repeat(64)),
                "operation": {"kind": "query", "request": {}},
                "page": 0,
                "continuation": null
            }),
        ),
    ];

    for (tool_name, args) in requests {
        let result = handle_tool_call_with_registry_options(
            &cg,
            tool_name,
            args,
            None,
            None,
            ToolCallRegistryOptions {
                application_invocation_executor: Some(&executor),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(result.semantic_error(), Some(true));
    }

    assert_eq!(
        *executor
            .operations
            .lock()
            .expect("recorded daemon operations"),
        vec![
            tracedecay_daemon_protocol::DaemonInvocationOperation::MultiRootScopeSetRead,
            tracedecay_daemon_protocol::DaemonInvocationOperation::MultiRootScopeSetCompareAndSwap,
            tracedecay_daemon_protocol::DaemonInvocationOperation::MultiRootExecute,
        ]
    );
}

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

#[tokio::test]
async fn unmounted_files_root_dispatch_reports_a_real_orphaned_rust_source() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("unmounted-files-root-dispatch");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"unmounted-files-root-dispatch\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn mounted() {}\n").unwrap();
    fs::write(
        project.join("src/orphan.rs"),
        "pub fn reachable_only_if_declared() {}\n",
    )
    .unwrap();
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-unmounted-files-root-dispatch",
    )
    .await
    .unwrap();

    let result = handle_tool_call(
        &cg,
        "tracedecay_unmounted_files",
        json!({"ecosystem": "rust", "format": "json"}),
        None,
        None,
    )
    .await
    .expect("the production root dispatch reaches the portable unmounted-files handler");
    let payload: Value = serde_json::from_str(
        result.value["content"][0]["text"]
            .as_str()
            .expect("unmounted-files JSON text"),
    )
    .expect("unmounted-files JSON payload");

    assert_eq!(payload["unmounted_file_count"], 1);
    assert_eq!(payload["returned_count"], 1);
    assert_eq!(payload["unmounted"][0]["file"], "src/orphan.rs");
    assert_eq!(
        payload["unmounted"][0]["package"],
        "unmounted-files-root-dispatch"
    );
    assert_eq!(
        payload["unmounted"][0]["suggested_declaration"],
        "mod orphan;"
    );

    cg.close();
}

#[test]
fn hotpath_tool_identity_preserves_catalog_names_and_bounds_unknown_values() {
    assert_eq!(
        mcp_tool_hotpath_identity("tracedecay_search", false),
        "tracedecay_search"
    );
    assert_eq!(
        mcp_tool_hotpath_identity("attacker-controlled-unknown-name", false),
        "unknown"
    );
    assert_eq!(
        mcp_tool_hotpath_identity("another-unknown-name", true),
        "unknown"
    );
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

    for definition in get_tool_definitions().expect("tool definitions") {
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
            assert_eq!(
                mcp_tool_hotpath_identity(&definition.name, executor_available),
                definition.name,
                "{} must retain exact bounded Hotpath identity",
                definition.name
            );
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
                McpToolDispatchGroup::MultiRoot => assert!(
                    matches!(
                        definition.name.as_str(),
                        "tracedecay_multi_root_scope_set_read"
                            | "tracedecay_multi_root_scope_set_compare_and_swap"
                            | "tracedecay_multi_root_execute"
                    ),
                    "{} has no multi-root daemon handler entry",
                    definition.name
                ),
                McpToolDispatchGroup::Work => assert!(
                    crate::mcp::tools::binding::work_operation_for_tool(&definition.name).is_some(),
                    "{} has no canonical Work operation entry",
                    definition.name
                ),
                McpToolDispatchGroup::Workflow => assert!(
                    crate::mcp::tools::binding::workflow_operation_for_tool(&definition.name)
                        .is_some(),
                    "{} has no canonical Workflow operation entry",
                    definition.name
                ),
                McpToolDispatchGroup::RetainedApplication => {
                    let composition = retained_mcp_composition().unwrap_or_else(|error| {
                        panic!("{} catalog composition failed: {error}", definition.name)
                    });
                    let profile = ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).unwrap();
                    for operation in retained_operations_for_advertised_tool(&definition.name) {
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
                                panic!(
                                    "{} action {} catalog binding is not callable",
                                    definition.name,
                                    operation.as_str()
                                )
                            });
                        let expected = retained_surface_application_operation(operation).unwrap();
                        assert_eq!(capability.capability_id(), expected.capability_id());
                        assert_eq!(capability.use_case_id(), expected.use_case_id());
                        assert!(
                            composition
                                .bind_handler(capability.use_case_id(), &())
                                .is_some(),
                            "{} action {} application handler is not registered",
                            definition.name,
                            operation.as_str()
                        );
                    }
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
        "tracedecay_fact_store",
    ] {
        assert!(!advertised.contains(tool_name));
        for executor_available in [true, false] {
            assert_eq!(
                classify_mcp_tool_dispatch_group(tool_name, executor_available),
                None,
                "{tool_name} must fail closed with executor_available={executor_available}"
            );
        }
        let rejected = handle_tool_call_with_registry_options(
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

fn retained_operations_for_advertised_tool(tool_name: &str) -> Vec<RetainedSurfaceOperation> {
    match tool_name {
        "tracedecay_session_refresh" => vec![
            RetainedSurfaceOperation::SessionRefreshStatus,
            RetainedSurfaceOperation::SessionRefreshCancel,
            RetainedSurfaceOperation::SessionRefreshBegin,
        ],
        _ => vec![
            RetainedSurfaceOperation::from_tool_name(tool_name)
                .unwrap_or_else(|| panic!("{tool_name} has no retained-surface handler entry")),
        ],
    }
}

#[test]
fn graph_reader_selector_dispatch_policy_is_allowlisted() {
    for tool in get_tool_definitions().expect("tool definitions") {
        let properties = &tool.input_schema["properties"];
        let schema_has_registered_project_selector = properties.get("project_selector").is_some();
        assert_eq!(
            tool_accepts_registered_project_selector(&tool.name),
            schema_has_registered_project_selector,
            "{} registered-project selector schema and dispatch policy should stay in lockstep",
            tool.name
        );
        if schema_has_registered_project_selector {
            for alias in ["project_id", "project_path", "project_root", "root"] {
                assert!(properties.get(alias).is_none());
            }
            let selector = &properties["project_selector"];
            assert_eq!(
                selector["required"],
                json!(["project_id"]),
                "{} selector must require project_id",
                tool.name
            );
            assert_eq!(
                selector["additionalProperties"], false,
                "{} selector must be a closed object",
                tool.name
            );
            assert_eq!(
                selector["properties"]
                    .as_object()
                    .unwrap_or_else(|| panic!("{} closed selector properties", tool.name))
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                vec!["project_id"],
                "{} selector must expose exactly project_id",
                tool.name
            );
        }
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
        .registered_database(tracedecay_sessions::admission::HostAdmissionScope::Project)
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
                .upsert_session(&tracedecay_sessions::runtime::SessionRecord {
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
            tracedecay_global_db::ParseOffset {
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
            tracedecay_global_db::ParseOffset {
                byte_offset: 20,
                mtime: 200,
                file_id: 0,
            },
        )
        .await
        .unwrap();
    let options = || ToolCallRegistryOptions {
        registered_project_session_db: runtime
            .registered_database_arc(tracedecay_sessions::admission::HostAdmissionScope::Project),
        ..Default::default()
    };
    let status = handle_tool_call_with_registry_options(
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
    let runtime_result = handle_tool_call_with_registry_options(
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

/// Status must report the serving truth the retrieval lanes enforce. On a
/// fresh daemon the census answers before any generation seals; claiming
/// `serving_branch` there contradicted every lane's truthful
/// `generation_rebuilding` refusal. Once a sealed complete generation exists,
/// the branch claim returns together with the typed `retrieval_serving`
/// state.
#[tokio::test]
async fn status_serving_branch_reports_the_lane_serving_truth() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("status-serving-truth");
    fs::create_dir_all(project.join("src")).unwrap();
    run_git_in(&project, &["init", "-b", "main"]);
    fs::write(project.join("src/lib.rs"), "pub fn probe() {}\n").unwrap();
    run_git_in(&project, &["add", "."]);
    run_git_in(&project, &["commit", "-m", "initial"]);
    let (cg, runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-status-serving-truth",
    )
    .await
    .unwrap();
    // Publish store branch metadata and reopen, so `serving_branch` is a
    // claim the store would actually make — the exact claim the gate must
    // withhold while nothing serves.
    cg.checkpoint().await.unwrap();
    let layout = cg.store_layout().clone();
    cg.close();
    let meta = tracedecay_runtime_core::branch_meta::BranchMeta::new("main");
    tracedecay_runtime_core::branch_meta::save_branch_meta(&layout.data_root, &meta).unwrap();
    let cg = runtime
        .open_project_graph_for_test(
            &project,
            crate::tracedecay::TraceDecayOpenOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        cg.serving_branch(),
        Some("main"),
        "the store must publish a branch claim for this test to be falsifiable",
    );

    let freshness_reader = |latest_generation_id: Option<&str>,
                            staleness_state: Option<&str>,
                            rebuild_in_flight: bool| {
        let latest_generation_id = latest_generation_id.map(str::to_owned);
        let staleness_state = staleness_state.map(str::to_owned);
        let reader: tracedecay_dashboard_api::code_index_freshness_api::CodeIndexFreshnessReader =
            std::sync::Arc::new(move |worktree_root: std::path::PathBuf| {
                let freshness = tracedecay_dashboard_api::code_index_freshness_api::CodeIndexWorktreeFreshnessV1 {
                    worktree_root: worktree_root.display().to_string(),
                    latest_generation_id: latest_generation_id.clone(),
                    staleness_state: staleness_state.clone(),
                    rebuild_in_flight,
                    ..Default::default()
                };
                Box::pin(async move { Some(freshness) })
            });
        reader
    };
    let status_output = |result: ToolResult| {
        serde_json::from_str::<Value>(
            result.value["content"][0]["text"]
                .as_str()
                .expect("status JSON text"),
        )
        .expect("parse status JSON")
    };

    // Fresh daemon: the census answers, nothing has sealed yet.
    let rebuilding = handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_status",
        json!({"format": "json"}),
        None,
        None,
        ToolCallRegistryOptions {
            code_index_freshness_reader: Some(freshness_reader(None, Some("indexing"), true)),
            ..Default::default()
        },
    )
    .await
    .expect("status answers while nothing serves");
    let rebuilding = status_output(rebuilding);
    assert_eq!(
        rebuilding["retrieval_serving"],
        json!({"status": "unavailable", "reason": "generation_rebuilding"}),
        "status must carry the same typed refusal the lanes enforce",
    );
    assert!(
        rebuilding.get("serving_branch").is_none(),
        "no branch is served before the first sealed generation: {rebuilding}",
    );

    // A sealed complete generation exists: the branch claim is truthful again.
    let serving = handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_status",
        json!({"format": "json"}),
        None,
        None,
        ToolCallRegistryOptions {
            code_index_freshness_reader: Some(freshness_reader(
                Some("generation.status-serving-truth.1"),
                Some("fresh"),
                false,
            )),
            ..Default::default()
        },
    )
    .await
    .expect("status answers while serving");
    let serving = status_output(serving);
    assert_eq!(
        serving["retrieval_serving"],
        json!({"status": "serving", "freshness": "current"})
    );
    assert_eq!(
        serving["serving_branch"],
        json!("main"),
        "a serving census restores the branch claim: {serving}",
    );

    let rebuilding = handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_status",
        json!({"format": "json"}),
        None,
        None,
        ToolCallRegistryOptions {
            code_index_freshness_reader: Some(freshness_reader(
                Some("generation.status-serving-truth.1"),
                Some("stale"),
                true,
            )),
            ..Default::default()
        },
    )
    .await
    .expect("status answers while a stale seat is rebuilding");
    let rebuilding = status_output(rebuilding);
    assert_eq!(
        rebuilding["retrieval_serving"]["freshness"],
        json!("last_complete_stale")
    );
    assert_eq!(
        rebuilding["retrieval_serving"]["condition"],
        json!("rebuilding"),
        "scheduler liveness must distinguish a routine rebuild: {rebuilding}",
    );

    // A seat sealed long ago surfaces its age inside the serving claim, so a
    // wedged daemon serving days-old answers is visibly wedged rather than a
    // bare "serving".
    let sealed_at_micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as i64
        - 3 * 86_400 * 1_000_000;
    let aged_reader: tracedecay_dashboard_api::code_index_freshness_api::CodeIndexFreshnessReader =
        std::sync::Arc::new(move |worktree_root: std::path::PathBuf| {
            let freshness =
                tracedecay_dashboard_api::code_index_freshness_api::CodeIndexWorktreeFreshnessV1 {
                    worktree_root: worktree_root.display().to_string(),
                    latest_generation_id: Some("generation.status-serving-truth.1".to_owned()),
                    sealed_at_micros: Some(sealed_at_micros),
                    staleness_state: Some("stale".to_owned()),
                    ..Default::default()
                };
            Box::pin(async move { Some(freshness) })
        });
    let aged = handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_status",
        json!({"format": "json"}),
        None,
        None,
        ToolCallRegistryOptions {
            code_index_freshness_reader: Some(aged_reader),
            ..Default::default()
        },
    )
    .await
    .expect("status answers for an aged seat");
    let aged = status_output(aged);
    assert_eq!(aged["retrieval_serving"]["status"], json!("serving"));
    assert_eq!(
        aged["retrieval_serving"]["freshness"],
        json!("last_complete_stale")
    );
    assert_eq!(
        aged["retrieval_serving"]["condition"],
        json!("stalled"),
        "a stale seat with no scheduler remedy must be typed stalled: {aged}",
    );
    let age_seconds = aged["retrieval_serving"]["seated_generation_age_seconds"]
        .as_i64()
        .expect("an aged seat reports its age in the serving claim");
    assert!(
        age_seconds >= 3 * 86_400 - 60,
        "the serving claim must expose the seat age: {aged}",
    );

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

    let err = handle_tool_call(
        &cg,
        "tracedecay_status",
        json!({
            "project_selector": {
                "project_id": "explicit-selector-should-not-fall-open"
            },
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
            "project_selector": {
                "project_id": "cross-project-must-not-be-relabelled"
            },
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

    let (active, active_runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &active_project,
        "project.mcp-active-retrieval",
    )
    .await
    .unwrap();
    let (target, target_runtime) = init_sibling_registered_fixture(
        &active_runtime,
        &target_project,
        "project.mcp-target-retrieval",
    )
    .await;
    let active_project_id = active
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .expect("active project should be registered")
        .to_string();
    let target_project_id = target
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .expect("target project should be registered")
        .to_string();
    let target_server = crate::mcp::McpServer::new_with_host_admission_test_runtime_for_test(
        target,
        None,
        crate::host_admission::ProjectScopedTestRuntimeV1::new(target_runtime)
            .expect("target project-scoped runtime"),
    )
    .await
    .expect("target retained server");
    let server = crate::mcp::McpServer::new_with_retained_test_servers_for_test(
        active,
        None,
        crate::host_admission::ProjectScopedTestRuntimeV1::new(active_runtime)
            .expect("active project-scoped runtime"),
        vec![target_server],
    )
    .await
    .expect("active routed server");

    let result = server
        .handle_request(&tracedecay_mcp::transport::JsonRpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: Some(json!(1)),
            method: "tools/call".to_owned(),
            params: Some(json!({
                "name": "tracedecay_grep",
                "arguments": {
                    "pattern": "selected_project_handle_marker",
                    "project_selector": {"project_id": target_project_id},
                    "max_results": LARGE_RESPONSE_MARKER_COUNT,
                    "context_lines": 3,
                    "format": "json"
                }
            })),
        })
        .await
        .expect("selected-project grep response");
    let result = result
        .result
        .unwrap_or_else(|| panic!("selected-project grep failed: {:?}", result.error));
    let envelope: Value = result["content"]
        .as_array()
        .and_then(|items| {
            items.iter().find_map(|item| {
                item["text"]
                    .as_str()
                    .and_then(|text| serde_json::from_str(text).ok())
            })
        })
        .expect("truncated search JSON envelope");
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

    let retrieved = server
        .handle_request(&tracedecay_mcp::transport::JsonRpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: Some(json!(2)),
            method: "tools/call".to_owned(),
            params: Some(json!({
                "name": "tracedecay_retrieve",
                "arguments": {
                    "handle": handle,
                    "project_selector": {"project_id": target_project_id},
                    "format": "json"
                }
            })),
        })
        .await
        .expect("selected-project retrieve response");
    let retrieved = retrieved
        .result
        .unwrap_or_else(|| panic!("selected-project retrieve failed: {:?}", retrieved.error));
    let payload: Value = retrieved["content"]
        .as_array()
        .and_then(|items| {
            items.iter().find_map(|item| {
                item["text"]
                    .as_str()
                    .and_then(|text| serde_json::from_str(text).ok())
            })
        })
        .expect("retrieve JSON payload");

    assert_eq!(payload["expired"], false);
    assert!(
        payload["content"]
            .as_str()
            .is_some_and(|content| content.contains(&format!(
                "selected_project_handle_marker_{LAST_RETURNED_RESPONSE_MARKER:03}"
            ))),
        "selected project retrieve should return the full selected-project response: {payload}"
    );

    for (id, label, arguments) in [
        (
            3,
            "active/default retrieval",
            json!({"handle": handle, "format": "json"}),
        ),
        (
            4,
            "wrong-project retrieval",
            json!({
                "handle": handle,
                "project_selector": {"project_id": active_project_id},
                "format": "json"
            }),
        ),
    ] {
        let missing = server
            .handle_request(&tracedecay_mcp::transport::JsonRpcRequest {
                jsonrpc: "2.0".to_owned(),
                id: Some(json!(id)),
                method: "tools/call".to_owned(),
                params: Some(json!({
                    "name": "tracedecay_retrieve",
                    "arguments": arguments
                })),
            })
            .await
            .unwrap_or_else(|| panic!("{label} response"));
        let missing = missing
            .result
            .unwrap_or_else(|| panic!("{label} failed: {:?}", missing.error));
        let missing_payload: Value = missing["content"]
            .as_array()
            .and_then(|items| {
                items.iter().find_map(|item| {
                    item["text"]
                        .as_str()
                        .and_then(|text| serde_json::from_str(text).ok())
                })
            })
            .unwrap_or_else(|| panic!("{label} JSON payload"));
        assert_eq!(
            missing_payload["reason_code"], "handle_not_found",
            "{label} must not read the selected target's handle: {missing_payload}"
        );
        assert_eq!(missing_payload["expired"], Value::Null, "{label}");
        assert_eq!(missing_payload["content"], Value::Null, "{label}");
    }
}

/// Runs a git command in `root`, panicking on failure. The git-dispatch
/// deadline tests need resolvable refs, so they drive a real repository.
fn run_git_in(root: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "TraceDecay Test")
        .env("GIT_AUTHOR_EMAIL", "test@tracedecay.invalid")
        .env("GIT_COMMITTER_NAME", "TraceDecay Test")
        .env("GIT_COMMITTER_EMAIL", "test@tracedecay.invalid")
        .output()
        .expect("git command should run");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout_in(root: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("git command should run");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_owned()
}

/// A deadline `offset_micros` from now, used to hand the git dispatcher a live
/// budget the way the admission layer does.
fn deadline_from_now(offset_micros: i64) -> tracedecay_application::Deadline {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after unix epoch")
        .as_micros() as i64;
    tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(
        now.saturating_add(offset_micros),
    ))
    .expect("deadline")
}

/// An already-elapsed deadline must short-circuit *before* the expensive body
/// runs, so neither the `pr_context` walk nor the `admin_branch_add` index
/// build can proceed once the horizon is gone.
#[tokio::test]
async fn git_dispatch_rejects_an_already_elapsed_deadline_without_running_the_handler() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("git-deadline-elapsed");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn probe() {}\n").unwrap();
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-git-deadline-elapsed",
    )
    .await
    .unwrap();

    for tool_name in ["tracedecay_pr_context", "tracedecay_admin_branch_add"] {
        let options = ToolCallRegistryOptions {
            application_deadline: Some(
                tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(1)).unwrap(),
            ),
            ..ToolCallRegistryOptions::default()
        };
        let started = std::time::Instant::now();
        let result = dispatch_git_tools(
            tool_name,
            &cg,
            json!({ "base_ref": "main", "head_ref": "HEAD", "branch": "feature" }),
            options,
        )
        .await
        .expect("an elapsed-deadline dispatch returns a typed result, not a hard error");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "{tool_name} elapsed-deadline path must return fast, took {:?}",
            started.elapsed()
        );
        assert_eq!(
            result.semantic_error(),
            Some(true),
            "{tool_name} must surface the exhausted deadline as a semantic error",
        );
        assert!(
            result
                .failure_message()
                .is_some_and(|message| message.contains("dispatch deadline")),
            "{tool_name} must report the dispatch deadline, got {:?}",
            result.failure_message(),
        );
    }

    cg.close();
}

/// An unresolvable ref must fail fast with a typed git error well inside the
/// carried deadline — never spinning until the horizon.
#[tokio::test]
async fn pr_context_unresolvable_ref_fails_fast_within_deadline() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("git-pr-context-badref");
    fs::create_dir_all(project.join("src")).unwrap();
    run_git_in(&project, &["init", "-b", "main"]);
    fs::write(project.join("src/lib.rs"), "pub fn only() {}\n").unwrap();
    run_git_in(&project, &["add", "."]);
    run_git_in(&project, &["commit", "-m", "initial"]);

    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-git-pr-context-badref",
    )
    .await
    .unwrap();
    let options = verified_graph_options(
        &cg,
        ToolCallRegistryOptions {
            application_deadline: Some(deadline_from_now(30_000_000)),
            ..ToolCallRegistryOptions::default()
        },
    );
    let started = std::time::Instant::now();
    let result = dispatch_git_tools(
        "tracedecay_pr_context",
        &cg,
        json!({ "base_ref": "does-not-exist-ref", "head_ref": "HEAD" }),
        options,
    )
    .await
    .expect("an unresolvable ref returns a typed result");

    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "unresolvable ref must fail fast, took {:?}",
        started.elapsed(),
    );
    assert_eq!(result.semantic_error(), Some(true));
    let message = result.failure_message().unwrap_or_default().to_owned();
    assert!(
        message.contains("does-not-exist-ref"),
        "the failure must name the unresolvable ref, got {message:?}",
    );
    assert!(
        !message.contains("dispatch deadline"),
        "a fast ref failure must not be reported as a deadline timeout: {message:?}",
    );

    cg.close();
}

#[tokio::test]
async fn pr_context_returns_git_evidence_while_verified_graph_is_unavailable() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("git-pr-context-cold-graph");
    fs::create_dir_all(project.join("src")).unwrap();
    run_git_in(&project, &["init", "-b", "main"]);
    fs::write(project.join("src/lib.rs"), "pub fn before() {}\n").unwrap();
    run_git_in(&project, &["add", "."]);
    run_git_in(&project, &["commit", "-m", "initial"]);
    run_git_in(&project, &["switch", "-c", "feature"]);
    fs::write(
        project.join("src/lib.rs"),
        "pub fn before() {}\npub fn after() {}\n",
    )
    .unwrap();
    run_git_in(&project, &["add", "."]);
    run_git_in(&project, &["commit", "-m", "change source"]);

    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-git-pr-context-cold-graph",
    )
    .await
    .unwrap();
    let base_oid = git_stdout_in(&project, &["rev-parse", "main^{commit}"]);
    let head_oid = git_stdout_in(&project, &["rev-parse", "HEAD^{commit}"]);
    let merge_base = git_stdout_in(&project, &["merge-base", "main", "HEAD"]);
    for (error, expected_reason) in [
        (
            tracedecay_graph_query::CodeGraphReadError::Unavailable {
                detail: "the exact generation is still warming".to_owned(),
            },
            "code-graph-unavailable",
        ),
        (
            tracedecay_graph_query::CodeGraphReadError::Stale {
                detail: "the exact generation advanced".to_owned(),
            },
            "code-graph-stale",
        ),
    ] {
        let result = dispatch_git_tools(
            "tracedecay_pr_context",
            &cg,
            json!({
                "base_ref": "main",
                "head_ref": "HEAD",
                "format": "json",
            }),
            verified_graph_error_options(&cg, ToolCallRegistryOptions::default(), error),
        )
        .await
        .expect("transient verified graph failure must preserve available Git evidence");
        assert_ne!(result.semantic_error(), Some(true));
        let payload = serde_json::from_str::<Value>(
            result.value["content"][0]["text"]
                .as_str()
                .expect("PR context JSON text"),
        )
        .expect("parse PR context JSON");

        assert_eq!(payload["base"], "main");
        assert_eq!(payload["head"], "HEAD");
        assert_eq!(payload["base_oid"], base_oid);
        assert_eq!(payload["head_oid"], head_oid);
        assert_eq!(payload["merge_base"], merge_base);
        assert_eq!(payload["status"], "partial");
        assert_eq!(payload["files_changed"], 1);
        assert_eq!(payload["changes"][0]["path"], "src/lib.rs");
        assert_eq!(payload["changes"][0]["status"], "modified");
        assert_eq!(payload["analysis_coverage"]["complete"], false);
        assert_eq!(
            payload["verified_graph_evidence"]["reason_code"],
            expected_reason
        );
        assert_eq!(payload["verified_graph_evidence"]["status"], "unavailable");
    }

    cg.close();
}

#[tokio::test]
async fn pr_context_propagates_terminal_graph_failures_without_a_cursor() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("git-pr-context-terminal-graph");
    fs::create_dir_all(project.join("src")).unwrap();
    run_git_in(&project, &["init", "-b", "main"]);
    fs::write(project.join("src/lib.rs"), "pub fn before() {}\n").unwrap();
    run_git_in(&project, &["add", "."]);
    run_git_in(&project, &["commit", "-m", "initial"]);
    run_git_in(&project, &["switch", "-c", "feature"]);
    fs::write(
        project.join("src/lib.rs"),
        "pub fn before() {}\npub fn after() {}\n",
    )
    .unwrap();
    run_git_in(&project, &["add", "."]);
    run_git_in(&project, &["commit", "-m", "change source"]);

    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-git-pr-context-terminal-graph",
    )
    .await
    .unwrap();
    let terminal_errors = [
        tracedecay_graph_query::map_code_graph_read_runtime_error(
            tracedecay_graph_query::CodeGraphReadError::Cancelled,
        ),
        tracedecay_graph_query::map_code_graph_read_runtime_error(
            tracedecay_graph_query::CodeGraphReadError::Denied,
        ),
        tracedecay_graph_query::map_code_graph_read_runtime_error(
            tracedecay_graph_query::CodeGraphReadError::Corrupt {
                detail: "corrupt projection".to_owned(),
            },
        ),
        tracedecay_graph_query::map_code_graph_read_runtime_error(
            tracedecay_graph_query::CodeGraphReadError::ResetRequired {
                detail: "generation reset required".to_owned(),
            },
        ),
        tracedecay_graph_query::map_code_graph_read_runtime_error(
            tracedecay_graph_query::CodeGraphReadError::InvalidRequest {
                detail: "invalid graph request".to_owned(),
            },
        ),
        TraceDecayError::Config {
            message: "graph configuration is invalid".to_owned(),
        },
    ];

    for error in terminal_errors {
        let detail = error.to_string();
        let result = git::handle_pr_context(
            &cg,
            async move { Err::<tracedecay_graph_query::VerifiedGraphQuery, _>(error) },
            json!({"base_ref": "main", "head_ref": "HEAD", "format": "json"}),
            None,
            None,
            None,
        )
        .await;
        assert!(
            result.is_err(),
            "terminal graph failure must not become partial success: {detail}"
        );
    }

    cg.close();
}

#[tokio::test]
async fn graph_tools_reject_blank_node_ids_and_zero_depth_with_typed_errors() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("blank-node-id");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "pub fn blank_probe_callee() {}\npub fn blank_probe() { blank_probe_callee(); }\n",
    )
    .unwrap();
    let (cg, _runtime) =
        TraceDecay::init_test_fixture_with_registered_runtime(&project, "project.blank-node-id")
            .await
            .unwrap();
    for tool_name in [
        "tracedecay_impact",
        "tracedecay_callers",
        "tracedecay_callees",
        "tracedecay_node",
    ] {
        for blank in ["", "   "] {
            let error = dispatch_graph_tools(
                tool_name,
                &cg,
                json!({"node_id": blank}),
                None,
                verified_graph_options(&cg, ToolCallRegistryOptions::default()),
            )
            .await
            .expect_err(&format!("{tool_name} must reject a blank node_id"));
            assert!(
                matches!(&error, TraceDecayError::Config { message }
                    if message.contains("node_id must not be empty")),
                "{tool_name} returned the wrong error for a blank node_id: {error}",
            );
        }
    }

    // Handlers clamp depth with `min(max)`, which leaves an explicit zero
    // intact, so a valid node id still reaches the guard from this side.
    let node_id = "symbol.blank-probe";
    for tool_name in [
        "tracedecay_impact",
        "tracedecay_callers",
        "tracedecay_callees",
    ] {
        let error = dispatch_graph_tools(
            tool_name,
            &cg,
            json!({"node_id": node_id, "max_depth": 0}),
            None,
            verified_graph_options(&cg, ToolCallRegistryOptions::default()),
        )
        .await
        .expect_err(&format!("{tool_name} must reject max_depth 0"));
        assert!(
            matches!(&error, TraceDecayError::Config { message }
                if message.contains("at least 1")),
            "{tool_name} returned the wrong error for max_depth 0: {error}",
        );
    }
}

// ---------------------------------------------------------------------------
// Universal dispatch ceiling
// ---------------------------------------------------------------------------

use super::dispatch_groups::{
    LONG_RUNNING_TOOL_DISPATCH_CEILING, TOOL_DISPATCH_CEILING, tool_dispatch_budget,
    tool_dispatch_ceiling, tool_dispatch_deadline_error,
};

/// One tool from every dispatch group. `tracedecay_context` is listed first
/// because it is the one that actually hung: it is neither an
/// application-surface operation nor a controlled read, so
/// `dispatch_deadline_horizon_micros` returned `None` for it and it reached its
/// handler with no bound at all.
const DISPATCH_GROUP_SPOT_CHECKS: &[&str] = &[
    "tracedecay_context",
    "tracedecay_search",
    "tracedecay_callers",
    "tracedecay_status",
    "tracedecay_files",
    "tracedecay_dead_code",
    "tracedecay_complexity",
    "tracedecay_health",
    "tracedecay_test_map",
    "tracedecay_pr_context",
    "tracedecay_affected",
    "tracedecay_analytics",
    "tracedecay_skill_list",
    "tracedecay_dashboard",
    "tracedecay_diagnose",
    "tracedecay_hook_runtime",
    "tracedecay_str_replace",
    "tracedecay_fact_store_search",
];

/// Absent a carried deadline every tool still dispatches under a bound. Before
/// the universal ceiling only the git and memory groups were wrapped, so most
/// of these names had no ceiling of any kind.
#[test]
fn every_dispatch_group_has_a_ceiling_without_a_carried_deadline() {
    for tool_name in DISPATCH_GROUP_SPOT_CHECKS {
        let budget = tool_dispatch_budget(tool_name, None)
            .expect("a tool with no carried deadline still dispatches under its ceiling");
        assert_eq!(
            budget,
            tool_dispatch_ceiling(tool_name),
            "{tool_name} must inherit the universal ceiling",
        );
        assert!(
            budget <= LONG_RUNNING_TOOL_DISPATCH_CEILING,
            "{tool_name} must never dispatch near the 900s client hang this replaced",
        );
    }
}

/// The interactive ceiling is the default; only the explicitly listed
/// long-running jobs get the larger one, and even those stay bounded.
#[test]
fn long_running_tools_are_bounded_above_the_interactive_ceiling() {
    assert_eq!(
        tool_dispatch_ceiling("tracedecay_context"),
        TOOL_DISPATCH_CEILING,
    );
    assert_eq!(
        tool_dispatch_ceiling("tracedecay_run_affected_tests"),
        LONG_RUNNING_TOOL_DISPATCH_CEILING,
    );
    assert!(
        TOOL_DISPATCH_CEILING < LONG_RUNNING_TOOL_DISPATCH_CEILING,
        "the interactive ceiling must be the tighter of the two",
    );
    assert!(
        LONG_RUNNING_TOOL_DISPATCH_CEILING < std::time::Duration::from_mins(15),
        "nothing may ever run 900 seconds",
    );
}

/// A carried admission deadline wins whenever it is shorter, and the ceiling
/// still clamps one that is implausibly distant, so the ceiling can never be
/// escaped by carrying a longer deadline.
#[test]
fn carried_deadline_is_preferred_when_shorter_and_clamped_when_longer() {
    let short = deadline_from_now(5_000_000);
    let budget = tool_dispatch_budget("tracedecay_context", Some(&short))
        .expect("a live carried deadline yields a budget");
    assert!(
        budget <= std::time::Duration::from_secs(5),
        "the shorter carried deadline must win, got {budget:?}",
    );

    let distant = deadline_from_now(3_600_000_000);
    let budget = tool_dispatch_budget("tracedecay_context", Some(&distant))
        .expect("a distant carried deadline still yields a budget");
    assert_eq!(
        budget, TOOL_DISPATCH_CEILING,
        "a carried deadline beyond the ceiling must be clamped to it",
    );
}

/// An already-elapsed carried deadline is rejected rather than dispatched, for
/// every group — the same rule the git and memory wraps already applied.
#[test]
fn an_elapsed_carried_deadline_is_rejected_for_every_group() {
    let elapsed =
        tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(1)).expect("deadline");
    for tool_name in DISPATCH_GROUP_SPOT_CHECKS {
        assert!(
            tool_dispatch_budget(tool_name, Some(&elapsed)).is_none(),
            "{tool_name} must refuse to dispatch under an elapsed deadline",
        );
    }
}

/// The ceiling reports the same typed, retryable problem shape the memory wrap
/// established, so the MCP boundary surfaces structure instead of a hang.
#[test]
fn the_ceiling_reports_a_typed_retryable_problem() {
    let error = tool_dispatch_deadline_error("tracedecay_context", TOOL_DISPATCH_CEILING);
    let (reason_code, retryable, detail) = error
        .project_route_context()
        .expect("the ceiling must surface a typed project-route problem, not an opaque error");
    assert_eq!(
        reason_code, "tool_dispatch_deadline_exceeded",
        "the ceiling must keep its own reason code so callers can branch on it",
    );
    assert!(
        retryable,
        "a dispatch-ceiling cancellation is retryable: nothing was committed",
    );
    // The typed fields carry the shape; only the detail names the subject, so
    // these two checks assert information the typed fields cannot express.
    assert!(
        detail.contains("tracedecay_context"),
        "the problem must name the tool it cancelled, got {detail:?}",
    );
    assert!(
        detail.contains(&format!("{}s", TOOL_DISPATCH_CEILING.as_secs())),
        "the problem must name the budget it enforced, got {detail:?}",
    );
}

/// The wrap itself: a handler that never returns must surface the typed
/// deadline at the ceiling rather than holding the transport open forever.
/// This is the 900-second hang reduced to a unit.
#[tokio::test]
async fn a_handler_that_never_returns_hits_the_typed_ceiling() {
    let budget = std::time::Duration::from_millis(50);
    let never = std::future::pending::<Result<ToolResult>>();
    let started = std::time::Instant::now();
    let outcome = match tokio::time::timeout(budget, never).await {
        Ok(result) => result,
        Err(_elapsed) => Err(tool_dispatch_deadline_error("tracedecay_context", budget)),
    };
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "the ceiling must fire promptly, took {:?}",
        started.elapsed(),
    );
    let error = outcome.expect_err("a never-returning handler must not report success");
    assert!(
        error.to_string().contains("dispatch ceiling"),
        "got {error}"
    );
}

/// Equivalence: a warm call that finishes well inside the ceiling is untouched
/// by it — the bound changes failure, not work.
#[tokio::test]
async fn a_warm_call_is_unaffected_by_the_ceiling() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("dispatch-ceiling-warm");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn probe() {}\n").unwrap();
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-dispatch-ceiling-warm",
    )
    .await
    .unwrap();
    let started = std::time::Instant::now();
    let result = handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_context",
        json!({ "task": "probe" }),
        None,
        None,
        verified_graph_options(&cg, ToolCallRegistryOptions::default()),
    )
    .await
    .expect("a warm context call succeeds under the ceiling");
    assert!(
        started.elapsed() < TOOL_DISPATCH_CEILING,
        "a warm call must finish far inside the ceiling, took {:?}",
        started.elapsed(),
    );
    assert!(result.value["content"][0]["text"].is_string());

    cg.close();
}

/// Serve-old-while-rebuilding is typed on the wire: when the one verified-
/// graph open funnel answered from the last complete seated generation, the
/// dispatch boundary appends the `code_graph_freshness` trailer naming the
/// serving generation; a proven-current open leaves the response untouched.
#[tokio::test]
async fn a_stale_served_graph_read_carries_the_typed_freshness_trailer() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("stale-graph-trailer");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn probe() {}\n").unwrap();
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-stale-graph-trailer",
    )
    .await
    .unwrap();

    let stale = handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_files",
        json!({}),
        None,
        None,
        verified_graph_stale_options(&cg, ToolCallRegistryOptions::default()),
    )
    .await
    .expect("a stale-served graph read still answers");
    let rendered = serde_json::to_string(&stale.value).unwrap();
    assert!(
        rendered.contains("code_graph_freshness: stale"),
        "a stale-served response must carry the typed freshness trailer: {rendered}",
    );
    assert!(
        rendered.contains("generation.mcp-verified-graph-fixture.1"),
        "the trailer must name the serving generation: {rendered}",
    );
    assert!(
        rendered.contains("(sealed 1m ago) while the code index rebuilds"),
        "a rebuild-in-flight serve must state the seat age and the rebuild: {rendered}",
    );

    let wedged = handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_files",
        json!({}),
        None,
        None,
        verified_graph_wedged_options(&cg, ToolCallRegistryOptions::default()),
    )
    .await
    .expect("a wedged stale serve still answers");
    let rendered = serde_json::to_string(&wedged.value).unwrap();
    assert!(
        rendered.contains("no rebuild pass in flight"),
        "a wedged route must not claim a rebuild is in flight: {rendered}",
    );
    assert!(
        !rendered.contains("while the code index rebuilds"),
        "a wedged route must not present itself as a routine rebuild: {rendered}",
    );

    let current = handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_files",
        json!({}),
        None,
        None,
        verified_graph_options(&cg, ToolCallRegistryOptions::default()),
    )
    .await
    .expect("a proven-current graph read answers");
    let rendered = serde_json::to_string(&current.value).unwrap();
    assert!(
        !rendered.contains("code_graph_freshness"),
        "a proven-current response must not carry a freshness trailer: {rendered}",
    );

    cg.close();
}

#[test]
fn unavailable_effect_contract_fails_before_handler_dispatch() {
    assert!(super::ensure_mcp_dispatch_available("tracedecay_lcm_doctor").is_ok());
    assert!(super::ensure_mcp_dispatch_available("tracedecay_lcm_compress").is_err());
    assert!(super::ensure_mcp_dispatch_available("tracedecay_dashboard").is_ok());
    assert!(super::ensure_mcp_dispatch_available("tracedecay_search").is_ok());
}

#[tokio::test]
async fn user_lcm_doctor_reports_a_missing_store_without_opening_it() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("unavailable-user-lcm-effect");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn probe() {}\n").unwrap();
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-unavailable-user-lcm-effect",
    )
    .await
    .unwrap();
    let profile_root = dir.path().join("unavailable-user-lcm-profile");
    let sessions_db = tracedecay_sessions::runtime::user_sessions_db_path(&profile_root);
    let profile_identity =
        tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)
            .expect("missing-store profile identity");
    let profile_id = profile_identity.profile_id().as_str();
    let suffix = profile_id
        .strip_prefix("profile.")
        .expect("canonical profile identity prefix");
    let session_identity = tracedecay_session_memory::context::ResolvedSessionIdentity::for_profile(
        tracedecay_session_memory::context::ProfileId::new(profile_id.to_owned())
            .expect("profile session identity"),
        tracedecay_session_memory::context::SessionStoreId::new(format!("store.profile.{suffix}"))
            .expect("profile store identity"),
        tracedecay_session_memory::context::SessionRootId::new(format!("root.profile.{suffix}"))
            .expect("profile root identity"),
    );
    let profile_retained_authority =
        crate::daemon::retained_owner::profile_retained_connection_authority(
            &profile_identity,
            &session_identity,
        )
        .expect("canonical profile retained authority");

    let result = handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_lcm_doctor",
        json!({ "storage_scope": "user", "format": "json" }),
        None,
        None,
        ToolCallRegistryOptions {
            profile_root: Some(&profile_root),
            session_authorities: SessionAuthorities::default()
                .with_profile_retained_authority(Some(&profile_retained_authority)),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let payload: serde_json::Value = serde_json::from_str(
        result.value["content"][0]["text"]
            .as_str()
            .expect("LCM Doctor text response"),
    )
    .expect("LCM Doctor unavailable payload");
    assert_eq!(
        payload["problem"]["kind"], "unavailable",
        "missing-store LCM Doctor renders the application problem kind, got {payload}"
    );
    assert!(
        !sessions_db.exists(),
        "read-only LCM Doctor must not open a missing profile store"
    );
    cg.close();
}

#[tokio::test]
async fn unavailable_user_lcm_effect_is_rejected_before_profile_store_open() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("retired-user-lcm-effect");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn probe() {}\n").unwrap();
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-retired-user-lcm-effect",
    )
    .await
    .unwrap();
    let profile_root = dir.path().join("retired-user-lcm-profile");
    let sessions_db = tracedecay_sessions::runtime::user_sessions_db_path(&profile_root);

    let error = handle_tool_call_with_registry_options(
        &cg,
        "tracedecay_lcm_compress",
        json!({
            "storage_scope": "user",
            "provider": "codex",
            "session_id": "retired",
        }),
        None,
        None,
        ToolCallRegistryOptions {
            profile_root: Some(&profile_root),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();

    let message = error.to_string();
    assert!(
        message.contains(
            "storage_scope=user is unavailable for non-retained tool `tracedecay_lcm_compress`"
        ),
        "a known-but-unavailable LCM effect must report its typed reason, got {message}"
    );
    assert!(
        !sessions_db.exists(),
        "unavailable LCM must not open its profile store"
    );
    cg.close();
}
