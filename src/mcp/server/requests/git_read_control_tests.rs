use super::*;
use tempfile::TempDir;

use crate::config::lock_user_data_dir_test_env;

struct EnvRestore {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvRestore {
    fn set(key: &'static str, value: &std::path::Path) -> Self {
        let previous = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        unsafe {
            match self.previous.take() {
                Some(previous) => std::env::set_var(self.key, previous),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

struct NonCooperativeInvocationExecutor {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

impl tracedecay_application::ApplicationInvocationExecutor for NonCooperativeInvocationExecutor {
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
        Box::pin(async move {
            self.entered.notify_one();
            self.release.notified().await;
            Err(tracedecay_application::InvocationError::Unavailable)
        })
    }
}

impl crate::daemon_client::DaemonInvocationExecutor for NonCooperativeInvocationExecutor {
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
        Box::pin(async move {
            self.entered.notify_one();
            self.release.notified().await;
            Err(crate::daemon_client::DaemonInvocationError::Unavailable)
        })
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

#[tokio::test]
async fn execution_settlement_reports_not_started_and_joined() {
    let not_started = DispatchExecutionSettlement::new();
    assert_eq!(
        not_started.snapshot(),
        super::request_receipts::ToolCallWorkerSettlement::NotStarted
    );

    let joined = Arc::new(DispatchExecutionSettlement::new());
    assert_eq!(Arc::clone(&joined).observe(async { 7_u8 }).await, 7);
    assert_eq!(
        joined.snapshot(),
        super::request_receipts::ToolCallWorkerSettlement::Joined
    );
}

#[tokio::test]
async fn pre_cancelled_fact_store_never_crosses_the_sqlite_commit_boundary() {
    let _env_lock = lock_user_data_dir_test_env();
    let fixture = TempDir::new().expect("project fixture");
    let home = fixture.path().join("home");
    let data_dir = home.join(".tracedecay");
    let project = home.join("project");
    std::fs::create_dir_all(&project).expect("project fixture");
    std::fs::create_dir_all(&data_dir).expect("isolated profile");
    let _home = EnvRestore::set("HOME", &home);
    let _userprofile = EnvRestore::set("USERPROFILE", &home);
    let _data_dir = EnvRestore::set(crate::config::USER_DATA_DIR_ENV, &data_dir);
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-pre-cancelled-fact",
    )
    .await
    .expect("graph fixture");
    let cg = Arc::new(cg);
    let server = McpServer::new_with_context(
        McpServerConstructionContext::direct(Arc::clone(&cg), None)
            .with_startup_catch_up_enabled(false),
    )
    .await;
    let params = json!({
        "name": "tracedecay_fact_store",
        "arguments": {
            "action": "add",
            "content": "This fact must never commit.",
            "category": "general",
            "source": "mcp-pre-cancelled"
        }
    });

    let response = server
        .handle_tools_call(
            json!(1),
            Some(&params),
            false,
            &HookProjectRouteCache::default(),
            None,
            "pre-cancelled-fact-connection",
            true,
        )
        .await;
    assert_eq!(
        response
            .error
            .as_ref()
            .and_then(|error| error.data.as_ref())
            .and_then(|data| data.get("reason_code"))
            .and_then(Value::as_str),
        Some("tool_dispatch_cancelled")
    );
    assert!(
        cg.list_facts(None, None, 10)
            .await
            .expect("read facts")
            .is_empty(),
        "a cancellation that wins before try_begin_commit must leave SQLite unchanged"
    );
    server.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn fact_store_commit_survives_deadline_and_settles_mounted_accounting() {
    let _env_lock = lock_user_data_dir_test_env();
    let fixture = TempDir::new().expect("project fixture");
    let home = fixture.path().join("home");
    let data_dir = home.join(".tracedecay");
    let project = home.join("project");
    std::fs::create_dir_all(project.join("src")).expect("project source");
    std::fs::create_dir_all(&data_dir).expect("isolated profile");
    std::fs::write(project.join("src/lib.rs"), "pub fn probe() {}\n").expect("fixture source");
    let _home = EnvRestore::set("HOME", &home);
    let _userprofile = EnvRestore::set("USERPROFILE", &home);
    let _data_dir = EnvRestore::set(crate::config::USER_DATA_DIR_ENV, &data_dir);
    let project_id = tracedecay_domain::ProjectId::new("project.mcp-fact-commit-race")
        .expect("project identity");
    let (cg, runtime) =
        TraceDecay::init_test_fixture_with_registered_runtime(&project, project_id.as_str())
            .await
            .expect("graph fixture");
    let cg = Arc::new(cg);
    let accounting = runtime
        .registered_database_arc(crate::application::host_admission::HostAdmissionScope::Profile)
        .expect("mounted profile accounting database");
    let server = McpServer::new_with_context(
        McpServerConstructionContext::direct(Arc::clone(&cg), None)
            .with_direct_databases(Some(Arc::clone(&accounting)), None, None, None)
            .with_startup_catch_up_enabled(false),
    )
    .await;
    let accounting_blocker = accounting
        .begin_write_transaction()
        .await
        .expect("hold mounted accounting writer");
    let call_server = Arc::clone(&server);
    let call = tokio::spawn(async move {
        let params = json!({
            "name": "tracedecay_fact_store",
            "arguments": {
                "action": "add",
                "content": "The real fact commit wins its MCP deadline race.",
                "category": "general",
                "source": "mcp-commit-race"
            }
        });
        call_server
            .handle_tools_call(
                json!(1),
                Some(&params),
                false,
                &HookProjectRouteCache::default(),
                None,
                "commit-race-connection",
                false,
            )
            .await
    });

    let mut committed_fact = None;
    for _ in 0..1_000 {
        committed_fact = cg
            .list_facts(None, None, 10)
            .await
            .expect("read committed facts")
            .into_iter()
            .find(|fact| fact.content.contains("real fact commit"));
        if committed_fact.is_some() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        committed_fact.is_some(),
        "the production fact-store SQLite commit must become visible"
    );
    tokio::time::advance(std::time::Duration::from_secs(60)).await;
    tokio::task::yield_now().await;
    assert!(
        !call.is_finished(),
        "a committed fact response must retain its mounted accounting write until settlement"
    );

    accounting_blocker
        .rollback()
        .await
        .expect("release mounted accounting writer");
    let response = call.await.expect("tools/call task");
    assert!(
        response.error.is_none(),
        "committed success must not become a transport deadline failure: {response:?}"
    );
    let receipt = response
        .result
        .as_ref()
        .and_then(|result| result.get("_meta"))
        .and_then(|meta| meta.get("tracedecay/execution_receipt"))
        .unwrap_or_else(|| panic!("committed response receipt missing from {response:?}"));
    assert_eq!(receipt["terminal"], "completed");
    assert_eq!(receipt["worker_settlement"], "joined");
    assert!(!serialize_response_line(&response).contains("tool_dispatch_deadline_exceeded"));
    let project_key = RegisteredGlobalDb::canonical_project_key(&project);
    let events = accounting
        .query_analytics_events(&crate::global_db::AnalyticsEventQuery {
            project_id: Some(project_key),
            event_kind: Some("mcp_tool_call".to_owned()),
            limit: 10,
            ..Default::default()
        })
        .await
        .expect("settled accounting events");
    assert!(
        events
            .iter()
            .any(|event| event.tool_name.as_deref() == Some("tracedecay_fact_store")),
        "tools/call must not return before its mounted analytics write is visible"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn warm_fact_store_tools_call_p95_includes_mounted_accounting_and_serialization() {
    let _env_lock = lock_user_data_dir_test_env();
    let profile = TempDir::new().expect("profile fixture");
    let home = profile.path().join("home");
    let data_dir = home.join(".tracedecay");
    std::fs::create_dir_all(&data_dir).expect("isolated profile");
    let _home = EnvRestore::set("HOME", &home);
    let _userprofile = EnvRestore::set("USERPROFILE", &home);
    let _data_dir = EnvRestore::set(crate::config::USER_DATA_DIR_ENV, &data_dir);
    let project = profile.path().join("production-tool-call-p95");
    std::fs::create_dir_all(&project).expect("project fixture");
    let (cg, runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-production-tool-call-p95",
    )
    .await
    .expect("graph fixture");
    let accounting = runtime
        .registered_database_arc(crate::application::host_admission::HostAdmissionScope::Profile)
        .expect("mounted profile accounting database");
    let server = McpServer::new_with_context(
        McpServerConstructionContext::direct(cg, None)
            .with_direct_databases(Some(Arc::clone(&accounting)), None, None, None)
            .with_startup_catch_up_enabled(false),
    )
    .await;
    let project_key = RegisteredGlobalDb::canonical_project_key(&project);
    let initial_event_count = accounting
        .count_analytics_events(Some(&project_key), 0)
        .await
        .expect("initial mounted accounting event count");
    let routes = HookProjectRouteCache::default();
    let params_for = |ordinal: u64| {
        json!({
            "name": "tracedecay_fact_store",
            "arguments": {
                "action": "add",
                "content": format!("Warm production fact-store sample {ordinal}."),
                "category": "general",
                "source": "mcp-p95"
            }
        })
    };

    let params = params_for(1);
    let response = server
        .handle_tools_call(
            json!(1),
            Some(&params),
            false,
            &routes,
            None,
            "benchmark-connection",
            false,
        )
        .await;
    let receipt = response
        .result
        .as_ref()
        .and_then(|result| result.get("_meta"))
        .and_then(|meta| meta.get("tracedecay/execution_receipt"))
        .unwrap_or_else(|| panic!("production execution receipt missing from {response:?}"));
    assert_eq!(receipt["terminal"], "completed");
    assert_eq!(receipt["worker_settlement"], "joined");
    assert!(
        receipt["route_admission_us"].as_u64().is_some()
            && receipt["handler_us"].as_u64().is_some()
            && receipt["result_materialization_us"].as_u64().is_some()
    );
    let encoded = serialize_response_line(&response);
    assert!(encoded.contains("tracedecay/execution_receipt"));

    let mut samples = Vec::with_capacity(40);
    for request_id in 2..42 {
        let params = params_for(request_id);
        let started = std::time::Instant::now();
        let response = server
            .handle_tools_call(
                json!(request_id),
                Some(&params),
                false,
                &routes,
                None,
                "benchmark-connection",
                false,
            )
            .await;
        let encoded = serialize_response_line(&response);
        assert!(encoded.contains("tracedecay/execution_receipt"));
        samples.push(started.elapsed());
    }
    samples.sort_unstable();
    let p95 = samples[37];
    eprintln!("warm production tools/call p95 across 40 samples: {p95:?}");
    assert!(
        p95 < std::time::Duration::from_secs(1),
        "warm production fact-store tools/call p95 must remain interactive, got {p95:?}",
    );
    assert_eq!(
        accounting
            .count_analytics_events(Some(&project_key), 0)
            .await
            .expect("mounted accounting event count")
            - initial_event_count,
        41,
        "each measured tools/call must settle its mounted accounting event before return"
    );

    server.shutdown().await;
}

#[tokio::test]
async fn project_shutdown_closes_transport_without_waiting_for_noncooperative_tool_handler() {
    let _env_lock = lock_user_data_dir_test_env();
    let fixture = TempDir::new().expect("project fixture");
    let home = fixture.path().join("home");
    let data_dir = home.join(".tracedecay");
    let project = home.join("project");
    std::fs::create_dir_all(project.join("src")).expect("project source");
    std::fs::create_dir_all(&data_dir).expect("isolated profile");
    std::fs::write(project.join("src/lib.rs"), "pub fn probe() {}\n").expect("fixture source");
    let _home = EnvRestore::set("HOME", &home);
    let _userprofile = EnvRestore::set("USERPROFILE", &home);
    let _data_dir = EnvRestore::set(crate::config::USER_DATA_DIR_ENV, &data_dir);
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.mcp-bounded-transport-shutdown",
    )
    .await
    .expect("graph fixture");
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let executor = Arc::new(NonCooperativeInvocationExecutor {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    });
    let server = McpServer::new_with_context(
        McpServerConstructionContext::direct(cg, None)
            .with_application_invocation_executor(executor)
            .with_startup_catch_up_enabled(false),
    )
    .await;
    let (mut transport, input, _output) = crate::mcp::transport::ChannelTransport::new();
    let connection_server = Arc::clone(&server);
    let mut connection =
        tokio::spawn(async move { connection_server.run_connection(&mut transport).await });
    input
        .send(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "tracedecay_git_status",
                    "arguments": {}
                }
            })
            .to_string(),
        )
        .expect("tool request");
    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
        .await
        .expect("non-cooperative handler entered");

    server.abort_project_server_requests();
    let closed = tokio::time::timeout(std::time::Duration::from_millis(10), &mut connection).await;
    if closed.is_err() {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), &mut connection).await;
    }
    assert!(
        closed.is_ok(),
        "project shutdown must close the transport without awaiting a non-cooperative handler"
    );
    assert!(
        !server.shutdown_background_tasks().await,
        "bounded shutdown must not claim drainage while retained work is alive"
    );
    release.notify_waiters();
    assert!(
        server
            .retained_tool_dispatch_tasks
            .reconcile_within(std::time::Duration::from_secs(2))
            .await,
        "retained work must become drained only after its handler joins"
    );
}

#[test]
fn controlled_operations_receive_live_registration_and_bounded_deadlines() {
    assert!(tool_supports_live_cancellation("tracedecay_search"));
    assert!(tool_supports_live_cancellation(
        "tracedecay_run_affected_tests"
    ));
    assert!(!tool_supports_live_cancellation("tracedecay_outline"));
    for tool_name in [
        "tracedecay_git_status",
        "tracedecay_git_diff",
        "tracedecay_git_history",
        "tracedecay_git_blame",
        "tracedecay_git_hunks",
    ] {
        assert!(tool_supports_live_cancellation(tool_name));
        let application_surface =
            crate::application_surface::ApplicationSurfaceOperation::from_tool_name(tool_name);
        assert!(
            application_surface.is_some(),
            "Git reads must enter the catalog-owned application surface",
        );
        let controlled_read = is_controlled_read_tool(tool_name);
        assert!(controlled_read);
        assert_eq!(
            dispatch_deadline_horizon_micros(application_surface.is_some(), controlled_read),
            Some(30_000_000)
        );
    }
    for tool_name in [
        "tracedecay_str_replace",
        "tracedecay_multi_str_replace",
        "tracedecay_insert_at",
        "tracedecay_ast_grep_rewrite",
        "tracedecay_replace_symbol",
        "tracedecay_insert_at_symbol",
        "tracedecay_move_symbol",
        "tracedecay_api_migration_apply",
        "tracedecay_source_edit_reconcile",
    ] {
        assert!(is_source_edit_tool(tool_name));
        assert_eq!(
            dispatch_deadline_horizon_micros(true, true),
            Some(30_000_000)
        );
    }

    let request_id = "request.git-read-controls".to_owned();
    let signal = tracedecay_application::CancellationSignal::active(
        "cancellation.request.git-read-controls",
    )
    .expect("signal");
    let registry = std::sync::Mutex::new(HashMap::from([(request_id.clone(), signal.clone())]));
    {
        let _registration = ApplicationCancellationRegistration {
            registry: &registry,
            request_id: Some(request_id.clone()),
        };
        signal.cancel(tracedecay_domain::UtcMicros(1));
        assert!(registry.lock().expect("registry").contains_key(&request_id));
    }
    assert!(!registry.lock().expect("registry").contains_key(&request_id));
}

/// These tools walk git trees but are not application-surface operations
/// and are not source edits, so the horizon predicate used to return `None`
/// for them: they dispatched with no deadline at all while the cheaper
/// `tracedecay_git_status` was bounded at thirty seconds.
#[test]
fn git_reading_tools_receive_a_bounded_deadline() {
    for tool_name in [
        "tracedecay_admin_branch_add",
        "tracedecay_affected",
        "tracedecay_diff_context",
        "tracedecay_changelog",
        "tracedecay_commit_context",
        "tracedecay_pr_context",
        "tracedecay_branch_search",
        "tracedecay_branch_diff",
        "tracedecay_branch_list",
    ] {
        assert!(
            crate::application_surface::ApplicationSurfaceOperation::from_tool_name(tool_name)
                .is_none(),
            "{tool_name} is not an application-surface operation, so only the \
             git-dispatch predicate can bound it",
        );
        assert!(!is_source_edit_tool(tool_name));
        assert!(
            is_controlled_read_tool(tool_name),
            "{tool_name} walks a git tree and must be a controlled read",
        );
        assert_eq!(
            dispatch_deadline_horizon_micros(
                false,
                is_controlled_read_tool(tool_name) || is_source_edit_tool(tool_name),
            ),
            Some(30_000_000),
            "{tool_name} must dispatch with a bounded horizon",
        );
    }
}

/// The horizon predicate reads the canonical binding table, so it must not
/// sweep in reads from other dispatch families.
#[test]
fn non_git_reads_stay_outside_the_controlled_read_horizon() {
    for tool_name in [
        "tracedecay_outline",
        "tracedecay_body",
        "tracedecay_dead_code",
        "tracedecay_health",
        "tracedecay_context",
    ] {
        assert!(
            !is_controlled_read_tool(tool_name),
            "{tool_name} is not a git-walking read",
        );
    }
    assert!(is_controlled_read_tool("tracedecay_search"));
}
