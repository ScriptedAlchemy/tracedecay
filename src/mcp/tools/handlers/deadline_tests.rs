use std::fs;
use std::future::pending;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::json;
use tempfile::TempDir;
use tracedecay_application::{
    ApplicationInvocation, ApplicationInvocationExecutor, ApplicationInvocationFuture,
    ApplicationResponse, CancellationSignal, InvocationError, RequestId,
};
use tracedecay_domain::{ManifestDigest, UtcMicros};

use super::dispatch_test_support::{SelectorEnv, SelectorRegistry};
use super::*;
use crate::config::lock_user_data_dir_test_env;
use crate::daemon_client::{
    DaemonInvocationError, DaemonInvocationExecutor, DaemonInvocationExecutorFuture,
    InvocationCancellationPolicy,
};
use crate::daemon_contract::{DaemonInvocationRequest, DaemonInvocationResponse};
use crate::mcp::server::{RetainedProjectGraphFuture, RetainedProjectGraphResolver};
use crate::mcp::tools::McpToolExecutionPolicyV1;
use crate::tracedecay::TraceDecay;

fn dispatch_control(tool_name: &str, deadline_millis: u64) -> McpToolDispatchControl {
    let cancellation =
        CancellationSignal::active(format!("cancellation.test.{tool_name}")).expect("signal");
    McpToolDispatchControl::new(
        tool_name,
        McpToolExecutionPolicyV1::interactive_read(deadline_millis),
        cancellation,
    )
    .expect("dispatch control")
}

fn assert_dispatch_failure(
    error: crate::errors::TraceDecayError,
    expected_reason: &str,
    expected_stage: &str,
) {
    let (reason, stage, retryable, _) = error
        .mcp_tool_dispatch_context()
        .expect("typed MCP dispatch failure");
    assert_eq!(reason, expected_reason);
    assert_eq!(stage, expected_stage);
    assert!(retryable);
}

#[tokio::test]
async fn registered_project_warm_open_uses_the_original_absolute_deadline() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("tempdir");
    let _env = SelectorEnv::new(dir.path());
    let active_project = dir.path().join("active");
    let selected_project = dir.path().join("selected");
    fs::create_dir_all(active_project.join("src")).expect("active source directory");
    fs::create_dir_all(selected_project.join("src")).expect("selected source directory");
    fs::write(active_project.join("src/lib.rs"), "pub fn active() {}\n").expect("active source");
    fs::write(
        selected_project.join("src/lib.rs"),
        "pub fn selected() {}\n",
    )
    .expect("selected source");

    let (active, _active_runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &active_project,
        "project.deadline-active",
    )
    .await
    .expect("active graph");
    let (selected, _selected_runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &selected_project,
        "project.deadline-selected",
    )
    .await
    .expect("selected graph");
    let selected = Arc::new(selected);
    let selected_project_id = selected
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .expect("registered project id")
        .to_owned();
    let registry = SelectorRegistry::open().await;
    let stalled_resolver: RetainedProjectGraphResolver = Arc::new(|_| {
        Box::pin(async { pending::<crate::errors::Result<Option<Arc<TraceDecay>>>>().await })
            as RetainedProjectGraphFuture
    });
    let started = std::time::Instant::now();
    let error = handle_tool_call_with_registry_and_implicit_project(
        &active,
        "tracedecay_files",
        json!({"project_id": selected_project_id, "path": "src"}),
        None,
        None,
        ToolCallRegistryOptions {
            global_db: Some(registry.database()),
            retained_project_graph_resolver: Some(stalled_resolver),
            dispatch_control: Some(dispatch_control("tracedecay_files", 50)),
            ..ToolCallRegistryOptions::default()
        },
    )
    .await
    .expect_err("a selected-project warm open must not outlive the dispatch deadline");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "warm-open deadline returned too late: {:?}",
        started.elapsed()
    );
    assert_dispatch_failure(error, "tool_dispatch_deadline_exceeded", "warm_open");

    active.close();
    Arc::into_inner(selected)
        .expect("stalled resolver must not retain the selected graph")
        .close();
}

struct PendingProfileRetrieval {
    started: AtomicBool,
}

impl PendingProfileRetrieval {
    fn new() -> Self {
        Self {
            started: AtomicBool::new(false),
        }
    }
}

impl SessionRetrievalServicePort for PendingProfileRetrieval {
    fn execute(&self, _command: SessionRetrievalCommand) -> SessionRetrievalServiceFuture<'_> {
        self.started.store(true, Ordering::Release);
        Box::pin(pending())
    }
}

#[tokio::test]
async fn user_storage_scope_is_covered_by_the_handler_deadline() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("tempdir");
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("project");
    fs::create_dir_all(project.join("src")).expect("source directory");
    fs::write(project.join("src/lib.rs"), "pub fn probe() {}\n").expect("source");
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.deadline-user-scope",
    )
    .await
    .expect("graph");
    let retrieval = PendingProfileRetrieval::new();
    let started = std::time::Instant::now();
    let error = handle_tool_call_with_registry_and_implicit_project(
        &cg,
        "tracedecay_lcm_grep",
        json!({"storage_scope": "user", "query": "needle", "scope": "all"}),
        None,
        None,
        ToolCallRegistryOptions {
            profile_root: Some(dir.path()),
            session_authorities: SessionAuthorities::default()
                .with_retrieval_services(None, Some(&retrieval)),
            dispatch_control: Some(dispatch_control("tracedecay_lcm_grep", 50)),
            ..ToolCallRegistryOptions::default()
        },
    )
    .await
    .expect_err("profile retrieval must not bypass the handler deadline");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "user-scope deadline returned too late: {:?}",
        started.elapsed()
    );
    assert!(
        retrieval.started.load(Ordering::Acquire),
        "fixture must reach the profile retrieval authority"
    );
    assert_dispatch_failure(error, "tool_dispatch_deadline_exceeded", "handler");

    cg.close();
}

struct PendingApplicationExecutor {
    controlled_started: AtomicBool,
}

impl PendingApplicationExecutor {
    fn new() -> Self {
        Self {
            controlled_started: AtomicBool::new(false),
        }
    }
}

impl ApplicationInvocationExecutor for PendingApplicationExecutor {
    fn invoke(
        &self,
        _invocation: ApplicationInvocation,
    ) -> ApplicationInvocationFuture<'_, std::result::Result<ApplicationResponse, InvocationError>>
    {
        Box::pin(pending())
    }
}

impl DaemonInvocationExecutor for PendingApplicationExecutor {
    fn invoke_controlled(
        &self,
        _request: DaemonInvocationRequest,
        _deadline: tracedecay_application::Deadline,
        _cancellation: CancellationSignal,
        _policy: InvocationCancellationPolicy,
    ) -> DaemonInvocationExecutorFuture<
        '_,
        std::result::Result<DaemonInvocationResponse, DaemonInvocationError>,
    > {
        self.controlled_started.store(true, Ordering::Release);
        Box::pin(pending())
    }

    fn observe_plan26_feedback(
        &self,
        _subject_digest: ManifestDigest,
        _observed_at: UtcMicros,
        _event: crate::application::feedback::observations::Plan26FeedbackSourceEventV1,
    ) -> DaemonInvocationExecutorFuture<'_, crate::errors::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn application_surface_dispatch_is_covered_before_handler_fallbacks() {
    let _env_lock = lock_user_data_dir_test_env();
    let dir = TempDir::new().expect("tempdir");
    let _env = SelectorEnv::new(dir.path());
    let project = dir.path().join("project");
    fs::create_dir_all(project.join("src")).expect("source directory");
    fs::write(project.join("src/lib.rs"), "pub fn probe() {}\n").expect("source");
    let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        &project,
        "project.deadline-application-route",
    )
    .await
    .expect("graph");
    let executor = PendingApplicationExecutor::new();
    let started = std::time::Instant::now();
    let error = handle_tool_call_with_registry_and_implicit_project(
        &cg,
        "tracedecay_git_status",
        json!({}),
        None,
        None,
        ToolCallRegistryOptions {
            application_invocation_executor: Some(&executor),
            application_request_id: Some(
                RequestId::new("request.deadline.application.route").expect("request id"),
            ),
            dispatch_control: Some(dispatch_control("tracedecay_git_status", 50)),
            ..ToolCallRegistryOptions::default()
        },
    )
    .await
    .expect_err("catalog application dispatch must not bypass the route deadline");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "application-route deadline returned too late: {:?}",
        started.elapsed()
    );
    assert!(
        executor.controlled_started.load(Ordering::Acquire),
        "fixture must reach the application invocation executor"
    );
    assert_dispatch_failure(
        error,
        "tool_dispatch_deadline_exceeded",
        "application_route",
    );

    cg.close();
}
