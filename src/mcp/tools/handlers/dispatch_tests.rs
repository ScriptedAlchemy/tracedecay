use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use serde_json::json;
use tempfile::TempDir;

use super::super::get_tool_definitions;
use super::dispatch_test_support::*;
use super::*;
use crate::config::lock_user_data_dir_test_env;

#[derive(Default)]
struct UnavailableEffectExecutor {
    invocations: AtomicUsize,
}

impl tracedecay_application::ApplicationInvocationExecutor for UnavailableEffectExecutor {
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
        self.invocations.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(tracedecay_application::InvocationError::Unavailable) })
    }
}

impl crate::daemon_client::DaemonInvocationExecutor for UnavailableEffectExecutor {
    fn invoke_controlled(
        &self,
        _request: crate::daemon_contract::DaemonInvocationRequest,
        _deadline: tracedecay_application::Deadline,
        _cancellation: tracedecay_application::CancellationSignal,
        _policy: crate::daemon_client::InvocationCancellationPolicy,
    ) -> crate::daemon_client::DaemonInvocationExecutorFuture<
        '_,
        std::result::Result<
            crate::daemon_contract::DaemonInvocationResponse,
            crate::daemon_client::DaemonInvocationError,
        >,
    > {
        self.invocations.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(crate::daemon_client::DaemonInvocationError::Unavailable) })
    }

    fn observe_plan26_feedback(
        &self,
        _subject_digest: tracedecay_domain::ManifestDigest,
        _observed_at: tracedecay_domain::UtcMicros,
        _event: crate::application::feedback::observations::Plan26FeedbackSourceEventV1,
    ) -> crate::daemon_client::DaemonInvocationExecutorFuture<'_, crate::errors::Result<()>> {
        Box::pin(async { Ok(()) })
    }
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
    cg.index_all().await.unwrap();

    let options = ToolCallRegistryOptions {
        application_deadline: Some(deadline_from_now(30_000_000)),
        ..ToolCallRegistryOptions::default()
    };
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

/// A normal PR comparison across a diverged feature branch still succeeds under
/// a live deadline — the enforcement wrapper does not break the happy path.
#[tokio::test]
async fn pr_context_succeeds_within_deadline_on_a_diverged_branch() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("git-pr-context-ok");
    fs::create_dir_all(project.join("src")).unwrap();
    run_git_in(&project, &["init", "-b", "main"]);
    fs::write(project.join("src/base.rs"), "pub fn base_fn() {}\n").unwrap();
    run_git_in(&project, &["add", "."]);
    run_git_in(&project, &["commit", "-m", "base commit"]);
    run_git_in(&project, &["switch", "-c", "feature"]);
    fs::write(project.join("src/feature.rs"), "pub fn feature_fn() {}\n").unwrap();
    run_git_in(&project, &["add", "."]);
    run_git_in(&project, &["commit", "-m", "feature commit"]);

    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-git-pr-context-ok",
    )
    .await
    .unwrap();
    cg.index_all().await.unwrap();

    let options = ToolCallRegistryOptions {
        application_deadline: Some(deadline_from_now(30_000_000)),
        ..ToolCallRegistryOptions::default()
    };
    let result = dispatch_git_tools(
        "tracedecay_pr_context",
        &cg,
        json!({ "base_ref": "main", "head_ref": "HEAD" }),
        options,
    )
    .await
    .expect("a normal PR comparison succeeds");

    assert_ne!(
        result.semantic_error(),
        Some(true),
        "the happy path must not be flagged as an error",
    );
    let rendered = serde_json::to_string(&result.value).unwrap();
    assert!(
        rendered.contains("feature.rs"),
        "the diverged file must appear in the comparison: {rendered}",
    );
    assert!(
        rendered.contains("files_changed"),
        "the payload must carry the PR-context summary: {rendered}",
    );

    cg.close();
}

/// The retained memory dispatch bounds every memory operation by the
/// admission-carried client deadline, exactly as the git dispatcher does: an
/// already-elapsed budget rejects each operation with the typed, retryable
/// deadline problem before the store is ever touched.
#[tokio::test]
async fn memory_dispatch_rejects_an_already_elapsed_deadline() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("memory-deadline-elapsed");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn probe() {}\n").unwrap();
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-memory-deadline-elapsed",
    )
    .await
    .unwrap();

    for operation in [
        RetainedSurfaceOperation::FactStore,
        RetainedSurfaceOperation::FactFeedback,
        RetainedSurfaceOperation::MemoryStatus,
    ] {
        let options = ToolCallRegistryOptions {
            application_deadline: Some(
                tracedecay_application::Deadline::new(tracedecay_domain::UtcMicros(1)).unwrap(),
            ),
            ..ToolCallRegistryOptions::default()
        };
        let started = std::time::Instant::now();
        let error = dispatch_memory_operation(
            operation,
            &cg,
            json!({ "action": "add", "content": "elapsed-deadline fixture" }),
            &options,
        )
        .await
        .expect_err("an elapsed deadline must reject the memory operation");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "{operation:?} elapsed-deadline path must return fast, took {:?}",
            started.elapsed(),
        );
        let (reason_code, retryable, _detail) = error
            .project_route_context()
            .expect("the elapsed deadline must surface a typed project-route problem");
        assert_eq!(
            reason_code, "memory_operation_deadline_exceeded",
            "{operation:?} must report the memory deadline problem",
        );
        assert!(retryable, "a deadline backstop is safe to retry");
    }

    cg.close();
}

/// A standalone caller carries no admission deadline, so the same memory
/// dispatch runs the operation unbounded to completion — the central bound is a
/// backstop, never a limit healthy operations hit.
#[tokio::test]
async fn memory_dispatch_without_a_deadline_runs_to_completion() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("memory-no-deadline");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn probe() {}\n").unwrap();
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-memory-no-deadline",
    )
    .await
    .unwrap();

    let options = ToolCallRegistryOptions::default();
    let result = dispatch_memory_operation(
        RetainedSurfaceOperation::FactStore,
        &cg,
        json!({ "action": "add", "content": "unbounded memory add fixture" }),
        &options,
    )
    .await
    .expect("a deadline-free memory add runs to completion");
    assert_ne!(
        result.semantic_error(),
        Some(true),
        "the unbounded add must succeed, not surface a semantic error",
    );
    assert!(
        result.failure_message().is_none(),
        "the unbounded add must not report a failure: {:?}",
        result.failure_message(),
    );

    // The persisted fact is visible to a follow-up status that also runs
    // unbounded through the same central dispatch.
    let status = dispatch_memory_operation(
        RetainedSurfaceOperation::MemoryStatus,
        &cg,
        json!({ "format": "json" }),
        &options,
    )
    .await
    .expect("memory_status runs unbounded when no deadline is carried");
    let rendered = serde_json::to_string(&status.value).unwrap();
    assert!(
        rendered.contains("fact_count"),
        "status must report the fact_count field: {rendered}",
    );

    cg.close();
}

/// Blank ids and zero depths are caller input, not internal invariants: they
/// arrive straight from tool arguments (`tracedecay tool impact --args
/// '{"node_id":""}'`). Every node-id graph tool must therefore answer with a
/// typed argument error naming the parameter, never an assertion that unwinds
/// the daemon's client task into a bare dropped connection.
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
    cg.index_all().await.unwrap();

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
                None,
                None,
                None,
                None,
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
    let node_id = cg
        .get_nodes_by_name("blank_probe")
        .await
        .unwrap()
        .first()
        .expect("indexed probe symbol")
        .id
        .clone();
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
            None,
            None,
            None,
            None,
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

/// The daemon serves each client from a task in a `JoinSet`, so a panicking
/// request unwinds only that task while the graph handle and the server-side
/// registries it touched stay shared with every later request. Neither a
/// rejected argument nor a real unwind on a task holding the same graph may
/// leave a later call broken or hanging.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graph_tools_still_answer_after_a_panicking_worker_task() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("panic-recovery");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "pub fn recovery_callee() {}\npub fn recovery_probe() { recovery_callee(); }\n",
    )
    .unwrap();
    let (cg, _runtime) =
        TraceDecay::init_test_fixture_with_registered_runtime(&project, "project.panic-recovery")
            .await
            .unwrap();
    cg.index_all().await.unwrap();
    let cg = Arc::new(cg);
    let node_id = cg
        .get_nodes_by_name("recovery_callee")
        .await
        .unwrap()
        .first()
        .expect("indexed probe symbol")
        .id
        .clone();

    // 1. A rejected argument fails as an ordinary error.
    assert!(
        dispatch_graph_tools(
            "tracedecay_impact",
            &cg,
            json!({"node_id": ""}),
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .is_err()
    );

    // 2. A genuine unwind on a task that shares the graph handle, mirroring a
    //    panicking daemon client task.
    let panicking = {
        let cg = Arc::clone(&cg);
        let node_id = node_id.clone();
        tokio::spawn(async move {
            let _ = dispatch_graph_tools(
                "tracedecay_impact",
                &cg,
                json!({"node_id": node_id}),
                None,
                None,
                None,
                None,
                None,
            )
            .await;
            panic!("simulated daemon client task panic");
        })
    };
    assert!(
        panicking.await.is_err_and(|error| error.is_panic()),
        "the worker task should have panicked"
    );

    // 3. The next valid request must still complete.
    let recovered = tokio::time::timeout(
        std::time::Duration::from_mins(1),
        dispatch_graph_tools(
            "tracedecay_impact",
            &cg,
            json!({"node_id": node_id, "max_depth": 2}),
            None,
            None,
            None,
            None,
            None,
        ),
    )
    .await
    .expect("impact must not hang after a worker panic")
    .expect("impact must succeed after a worker panic");
    assert!(recovered.value["content"][0]["text"].is_string());
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
    "tracedecay_fact_store",
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
    let rendered = error.to_string();
    assert!(
        rendered.contains("tracedecay_context"),
        "the problem must name the tool, got {rendered:?}",
    );
    assert!(
        rendered.contains("dispatch ceiling"),
        "the problem must name the ceiling, got {rendered:?}",
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
    cg.index_all().await.unwrap();

    let started = std::time::Instant::now();
    let result = handle_tool_call_with_registry_and_implicit_project(
        &cg,
        "tracedecay_context",
        json!({ "task": "probe" }),
        None,
        None,
        ToolCallRegistryOptions::default(),
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

#[test]
fn unavailable_effect_contract_fails_before_handler_dispatch() {
    let error = super::ensure_mcp_dispatch_available("tracedecay_lcm_doctor").unwrap_err();
    assert_eq!(
        error.project_route_context(),
        Some((
            "mcp_dispatch_effect_journey_unverified",
            false,
            "MCP tool 'tracedecay_lcm_doctor' is advertised but unavailable until its effect journey is verified",
        ))
    );
    assert!(super::ensure_mcp_dispatch_available("tracedecay_dashboard").is_ok());
    assert!(super::ensure_mcp_dispatch_available("tracedecay_search").is_ok());
}

#[tokio::test]
async fn unavailable_application_effect_is_rejected_before_canonical_executor_invocation() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().unwrap();
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("unavailable-application-effect");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "pub fn probe() {}\n").unwrap();
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-unavailable-application-effect",
    )
    .await
    .unwrap();
    let executor = UnavailableEffectExecutor::default();
    let request = serde_json::to_value(tracedecay_application::ConfigurationSetRequestV1 {
        layer: tracedecay_domain::configuration::ConfigurationLayerIdV1::Default,
        key: tracedecay_domain::configuration::SettingKey::new("mcp.tool_timings").unwrap(),
        value: tracedecay_domain::configuration::ConfigurationValueV1::Boolean(true),
        expected_revision: tracedecay_domain::configuration::ConfigurationRevisionId::new(
            "revision.mcp-unavailable-application-effect",
        )
        .unwrap(),
    })
    .unwrap();

    let error = handle_tool_call_with_registry_and_implicit_project(
        &cg,
        "tracedecay_configuration_set",
        request,
        None,
        None,
        ToolCallRegistryOptions {
            application_invocation_executor: Some(&executor),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();

    assert_eq!(
        error.project_route_context(),
        Some((
            "mcp_dispatch_effect_journey_unverified",
            false,
            "MCP tool 'tracedecay_configuration_set' is advertised but unavailable until its effect journey is verified",
        ))
    );
    assert_eq!(executor.invocations.load(Ordering::SeqCst), 0);
    cg.close();
}

#[tokio::test]
async fn unavailable_user_lcm_effect_is_rejected_before_profile_store_open() {
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
    let sessions_db = crate::sessions::user_sessions_db_path(&profile_root);

    let error = handle_tool_call_with_registry_and_implicit_project(
        &cg,
        "tracedecay_lcm_doctor",
        json!({
            "storage_scope": "user",
            "provider": "codex",
            "mode": "repair",
            "apply": true,
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

    assert_eq!(
        error.project_route_context(),
        Some((
            "mcp_dispatch_effect_journey_unverified",
            false,
            "MCP tool 'tracedecay_lcm_doctor' is advertised but unavailable until its effect journey is verified",
        ))
    );
    assert!(
        !sessions_db.exists(),
        "unavailable LCM must not open its profile store"
    );
    cg.close();
}
