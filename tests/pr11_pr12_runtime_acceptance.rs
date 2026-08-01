mod common;

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;
use tracedecay::application::ProjectSourceAccessSnapshot;
use tracedecay::application::feedback::concrete::open_pr12_feedback_runtime;
use tracedecay::application::feedback::owner::{
    FeedbackReadInvocationResultV1, FeedbackReadOperationV1, FeedbackReadOwnerErrorV1,
};
use tracedecay::application::operation_stream::{
    OperationCancelOutcome, OperationEventAuthority, OperationEventError, OperationId,
    OperationKind, OperationStreamConfig,
};
use tracedecay::application::primitives::{Pr12PrimitiveRequest, StorageStatusPrimitiveRequest};
use tracedecay::application_output::json::json_line as canonical_json_line;
use tracedecay::application_output::markdown::render as render_markdown;
use tracedecay::application_output::view::CanonicalHumanView;
use tracedecay::application_surface::{
    ApplicationSurfaceInvocationResult, ApplicationSurfaceOperation, ApplicationSurfaceRequest,
    FeedbackSurfaceRequest, GitApplySurfaceRequest, GitPreviewSurfaceRequest,
    execute_application_surface, http_application_router, parse_application_surface_request,
    resolve_application_surface_dispatch_with_controls, resolve_http_application_surface,
};
use tracedecay::daemon::{DaemonHandshake, call_default_tool};
use tracedecay::daemon_client::{
    DaemonInvocationClient, DaemonLspSessionClient, RequestedOutputFormat,
};
use tracedecay::mcp::response_handles::{ResponseHandleLookup, retrieve_response_handle};
use tracedecay::mcp::tools::dispatch::resolve_mcp_application_surface;
use tracedecay_api::sse_response;
use tracedecay_application::feedback::{
    FEEDBACK_DIAGNOSTICS_CAPABILITY_ID_V1, FEEDBACK_LIST_CAPABILITY_ID_V1,
    FeedbackDiagnosticsReadRequestV1,
};
use tracedecay_application::{
    ApplicationEnvelope, ApplicationOutcome, ApplicationProblemKind, CancellationContext,
    CancellationObservation, CancellationSignal, CancellationStage, CapabilityGrantId,
    CapabilityGrantSnapshot, CoverageCompleteness, Deadline, DisclosureClass, IdempotencyKey,
    OperationBudgetUsage, OperationReceipt, OperationTermination, PageRequest, RequestContext,
    RequestId, ResolvedScope,
};
use tracedecay_domain::configuration::{
    AuthorityRef, ConfigurationRevisionId, ScopeSourceBinding, SourceBindingId, SourceKindV1,
};
use tracedecay_domain::{
    ActorId, CommitId, GitCommitIdentityV1, GitIndexCommitIntentV1, GitIndexPreviewId,
    GitIndexPreviewV1, GitIndexReceiptOutcomeV1, GitIndexSigningPolicyV1,
    GitIndexTransactionOperationV1, GitIndexTransactionReceiptV1, LocatorDigest, ManifestDigest,
    ProjectId, RefId, RepositoryId, UtcMicros, WorktreeId,
};
use tracedecay_lsp::{FramePoll, FrameSend, TRACEDECAY_CONTEXT_REVISION};
use tracedecay_tool_catalog::{BindingSurface, CapabilityId, UseCaseId};

struct RuntimeFixture {
    _daemon: common::DaemonProcess,
    client: DaemonInvocationClient,
    handshake: DaemonHandshake,
    project: PathBuf,
    _environment: common::IsolatedEnv,
}

impl RuntimeFixture {
    fn home(&self) -> &Path {
        self._environment.home()
    }
}

async fn runtime_fixture() -> RuntimeFixture {
    let (environment, project) = common::IsolatedEnv::acquire().await;
    let daemon = common::spawn_tracedecay_daemon(environment.home());
    initialize_project(environment.home(), &project);
    let handshake = DaemonHandshake::for_current_client(Some(project.clone()), None, false, false)
        .expect("daemon handshake");
    let client = DaemonInvocationClient::for_current(handshake.clone()).expect("daemon client");
    RuntimeFixture {
        _daemon: daemon,
        client,
        handshake,
        project,
        _environment: environment,
    }
}

async fn lsp_runtime_fixture() -> RuntimeFixture {
    let host_rustup_home = std::env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".rustup")))
        .filter(|path| path.is_dir());
    let host_rustup_toolchain = std::env::var_os("RUSTUP_TOOLCHAIN")
        .or_else(|| option_env!("RUSTUP_TOOLCHAIN").map(OsString::from));
    let (environment, project) = common::IsolatedEnv::acquire().await;
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/context_eval_project"),
        &project,
    );
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pr12_managed_run_overlay"),
        &project,
    );
    git(&project, &["init", "--quiet"]);
    git(&project, &["config", "user.name", "TraceDecay Test"]);
    git(
        &project,
        &["config", "user.email", "tracedecay@example.com"],
    );
    git(&project, &["add", "."]);
    git(&project, &["commit", "--quiet", "-m", "base"]);
    let daemon = common::spawn_tracedecay_daemon_with(environment.home(), |command| {
        // Keep TraceDecay state under the isolated home while allowing a rustup
        // proxy on PATH to resolve the host's already-installed analyzer.
        if let Some(rustup_home) = host_rustup_home {
            command.env("RUSTUP_HOME", rustup_home);
            if let Some(rustup_toolchain) = host_rustup_toolchain {
                command.env("RUSTUP_TOOLCHAIN", rustup_toolchain);
            }
        }
    });
    let output = common::tracedecay_command_with_home(environment.home())
        .arg("init")
        .current_dir(&project)
        .stdin(Stdio::null())
        .output()
        .expect("initialize indexed LSP fixture");
    assert_command_success("tracedecay init", &output);
    let storage = run_storage_status(environment.home(), &project, true);
    assert_command_success("open indexed LSP project", &storage);
    let handshake = DaemonHandshake::for_current_client(Some(project.clone()), None, false, false)
        .expect("daemon handshake");
    let client = DaemonInvocationClient::for_current(handshake.clone()).expect("daemon client");
    RuntimeFixture {
        _daemon: daemon,
        client,
        handshake,
        project,
        _environment: environment,
    }
}

#[cfg(unix)]
async fn git_runtime_fixture() -> RuntimeFixture {
    let (environment, project) = common::IsolatedEnv::acquire().await;
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/context_eval_project"),
        &project,
    );
    git(&project, &["init", "--quiet"]);
    git(&project, &["config", "user.name", "TraceDecay Test"]);
    git(
        &project,
        &["config", "user.email", "tracedecay@example.com"],
    );
    // Snapshot equality spans two processes with different homes: this test
    // captures in-process under the developer's real HOME, while the daemon
    // recaptures under the isolated one. Without pinning excludes, whatever
    // the developer ignores globally is merely untracked to the daemon, and
    // the same files land in different snapshot digests on every machine.
    git(&project, &["config", "core.excludesFile", "/dev/null"]);
    git(&project, &["add", "."]);
    git(&project, &["commit", "--quiet", "-m", "base"]);

    let daemon = common::spawn_tracedecay_daemon(environment.home());
    let output = common::tracedecay_command_with_home(environment.home())
        .arg("init")
        .current_dir(&project)
        .stdin(Stdio::null())
        .output()
        .expect("run tracedecay init");
    assert_command_success("tracedecay init", &output);

    git(&project, &["add", "."]);
    let staged = std::process::Command::new(common::git_program())
        .current_dir(&project)
        .args(["diff", "--cached", "--quiet"])
        .status()
        .expect("inspect daemon enrollment");
    if !staged.success() {
        git(&project, &["commit", "--quiet", "-m", "daemon enrollment"]);
    }
    std::fs::write(
        project.join("src/main.rs"),
        "mod cli;\n\nfn main() {\n    cli::run();\n}\n\n// PR12 transport parity\n",
    )
    .expect("write staged Git change");
    git(&project, &["add", "src/main.rs"]);

    let handshake = DaemonHandshake::for_current_client(Some(project.clone()), None, false, false)
        .expect("daemon handshake");
    let client = DaemonInvocationClient::for_current(handshake.clone()).expect("daemon client");
    RuntimeFixture {
        _daemon: daemon,
        client,
        handshake,
        project,
        _environment: environment,
    }
}

#[cfg(unix)]
fn git(project: &Path, args: &[&str]) {
    let output = std::process::Command::new(common::git_program())
        .current_dir(project)
        .args(args)
        .output()
        .expect("run Git fixture command");
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
fn git_stdout(project: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new(common::git_program())
        .current_dir(project)
        .args(args)
        .output()
        .expect("run Git fixture command");
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("Git fixture output is UTF-8")
        .trim()
        .to_owned()
}

async fn poll_lsp_response(session: &mut DaemonLspSessionClient, response_id: u64) -> Value {
    // Semantic requests are answered asynchronously: while an operation is in
    // flight the gateway writes no frame at all, so silence means "not yet"
    // rather than "never" and a client has to keep reading. The old bound of
    // 200 polls gave up after roughly two seconds, which a real analyzer's
    // cold start — sysroot load and crate graph build — cannot beat.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    while std::time::Instant::now() < deadline {
        match session
            .poll_daemon_frame()
            .await
            .expect("poll daemon LSP frame")
        {
            FramePoll::Frame(frame) => {
                let value: Value =
                    serde_json::from_slice(frame.as_slice()).expect("daemon LSP JSON");
                session
                    .acknowledge_daemon_frame()
                    .await
                    .expect("acknowledge daemon LSP frame");
                if value.get("id").and_then(Value::as_u64) == Some(response_id) {
                    return value;
                }
            }
            FramePoll::Pending => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
            FramePoll::Closed => panic!("daemon LSP session closed before response {response_id}"),
        }
    }
    panic!("daemon LSP response {response_id} timed out after 90s")
}

async fn send_lsp(session: &mut DaemonLspSessionClient, value: Value) {
    assert_eq!(
        session
            .try_send_client_frame(&value.to_string())
            .await
            .expect("send daemon LSP frame"),
        FrameSend::Sent
    );
}

async fn shutdown_lsp(session: &mut DaemonLspSessionClient, request_id: u64) {
    let shutdown_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "method": "shutdown",
        "params": {},
    });
    match session
        .try_send_client_frame(&shutdown_request.to_string())
        .await
        .expect("send daemon LSP shutdown frame")
    {
        FrameSend::Closed => return,
        FrameSend::Sent => {}
        FrameSend::Backpressured => panic!("daemon LSP shutdown frame was backpressured"),
    }
    let shutdown = poll_lsp_response(session, request_id).await;
    assert_eq!(shutdown["result"], Value::Null);
    let exit_notification = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "exit",
        "params": {},
    });
    match session
        .try_send_client_frame(&exit_notification.to_string())
        .await
        .expect("send daemon LSP exit frame")
    {
        FrameSend::Closed => return,
        FrameSend::Sent => {}
        FrameSend::Backpressured => panic!("daemon LSP exit frame was backpressured"),
    }
    for _ in 0..100 {
        match session
            .poll_daemon_frame()
            .await
            .expect("poll closed daemon LSP session")
        {
            FramePoll::Closed => return,
            FramePoll::Pending => {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            FramePoll::Frame(frame) => {
                panic!(
                    "daemon LSP session emitted a frame after exit: {}",
                    String::from_utf8_lossy(&frame)
                );
            }
        }
    }
    panic!("daemon LSP session did not close after shutdown and exit")
}

async fn poll_lsp_context(
    session: &mut DaemonLspSessionClient,
    document_uri: &str,
    kind: &str,
    first_request_id: u64,
) -> Value {
    let mut last_response = Value::Null;
    for request_id in first_request_id..first_request_id.saturating_add(500) {
        send_lsp(
            session,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "method": "tracedecay/context",
                "params": {
                    "kind": kind,
                    "documentUri": document_uri,
                },
            }),
        )
        .await;
        let response = poll_lsp_response(session, request_id).await;
        if response.get("result").is_some() {
            return response;
        }
        last_response = response;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("production {kind} context projection did not become ready: {last_response}")
}

fn initialize_project(home: &Path, project: &Path) {
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/context_eval_project"),
        project,
    );
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pr12_managed_run_overlay"),
        project,
    );
    let output = common::tracedecay_command_with_home(home)
        .arg("init")
        .current_dir(project)
        .stdin(Stdio::null())
        .output()
        .expect("run tracedecay init");
    assert_command_success("tracedecay init", &output);
}

fn copy_dir(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).expect("fixture destination");
    for entry in std::fs::read_dir(source).expect("fixture directory") {
        let entry = entry.expect("fixture entry");
        let target = destination.join(entry.file_name());
        if entry.file_type().expect("fixture entry type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("copy checked-in fixture");
        }
    }
}

fn storage_status_request() -> ApplicationSurfaceRequest {
    ApplicationSurfaceRequest::Primitive(Pr12PrimitiveRequest::StorageStatus(
        StorageStatusPrimitiveRequest {
            include_details: false,
        },
    ))
}

fn run_storage_status(home: &Path, project: &Path, json_output: bool) -> Output {
    let project_arg = project.to_string_lossy().into_owned();
    let mut command = common::tracedecay_command_with_home(home);
    command
        .current_dir(project)
        .args([
            "tool",
            "--project",
            project_arg.as_str(),
            "storage_status",
            "--args",
            r#"{"include_details":false}"#,
        ])
        .stdin(Stdio::null());
    if json_output {
        command.arg("--json");
    }
    command.output().expect("run storage_status")
}

fn run_feedback_diagnostics(home: &Path, project: &Path, request_handle: &str) -> Output {
    let project_arg = project.to_string_lossy().into_owned();
    let arguments = serde_json::json!({ "request_handle": request_handle }).to_string();
    common::tracedecay_command_with_home(home)
        .current_dir(project)
        .args([
            "tool",
            "--project",
            project_arg.as_str(),
            "feedback_diagnostics",
            "--args",
            arguments.as_str(),
            "--json",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("run feedback_diagnostics")
}

fn run_application_tool(
    home: &Path,
    project: &Path,
    operation: ApplicationSurfaceOperation,
    arguments: &Value,
) -> Output {
    let project_arg = project.to_string_lossy().into_owned();
    let arguments = arguments.to_string();
    common::tracedecay_command_with_home(home)
        .current_dir(project)
        .args([
            "tool",
            "--project",
            project_arg.as_str(),
            operation.as_str(),
            "--args",
            arguments.as_str(),
            "--json",
        ])
        .stdin(Stdio::null())
        .output()
        .expect("run application tool")
}

#[cfg(all(unix, feature = "test-transport"))]
async fn preview_commit_via_mcp(
    fixture: &RuntimeFixture,
    scope: &ResolvedScope,
    request_id: &str,
    message: &str,
) -> GitIndexPreviewV1 {
    let captured_at = wall_clock_micros();
    let snapshot = tracedecay::daemon::capture_exact_git_snapshot_for_test(
        &fixture.project,
        scope.project_id.clone(),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        captured_at,
    )
    .expect("exact preview snapshot");
    let identity = GitCommitIdentityV1 {
        name: "TraceDecay Test".to_owned(),
        email: "tracedecay@example.com".to_owned(),
        at: captured_at,
    };
    let request = GitPreviewSurfaceRequest {
        operation: GitIndexTransactionOperationV1::CommitIndex,
        preview_id: GitIndexPreviewId::new("preview.transport-input").expect("preview id"),
        repository_snapshot: snapshot,
        selected_hunks: Vec::new(),
        commit_intent: Some(
            GitIndexCommitIntentV1::new(
                message.to_owned(),
                identity.clone(),
                identity,
                GitIndexSigningPolicyV1::UnsignedPermitted,
            )
            .expect("commit intent"),
        ),
    };
    let result = resolve_mcp_application_surface(
        ApplicationSurfaceOperation::GitPreview,
        RequestId::new(request_id).expect("MCP preview request id"),
        ApplicationSurfaceRequest::GitPreview(request),
        RequestedOutputFormat::Json,
        Some(&fixture.client),
    )
    .await
    .expect("MCP Git preview dispatch");
    let ApplicationOutcome::Preview(preview) = &result
        .result
        .as_ref()
        .expect("MCP Git preview result")
        .outcome
    else {
        panic!("MCP git_preview must return a preview outcome");
    };
    serde_json::from_value(preview.payload.clone().expect("MCP immutable Git preview"))
        .expect("typed MCP Git preview")
}

async fn assert_application_transport_parity(
    fixture: &RuntimeFixture,
    case: &str,
    operation: ApplicationSurfaceOperation,
    arguments: Value,
) -> Value {
    let cli = run_application_tool(fixture.home(), &fixture.project, operation, &arguments);
    assert_command_success(operation.as_str(), &cli);
    let cli: Value = serde_json::from_slice(&cli.stdout).expect("CLI application JSON");
    let mcp = resolve_mcp_application_surface(
        operation,
        RequestId::new(format!("request.primitive-parity.mcp.{case}")).expect("MCP request id"),
        parse_application_surface_request(operation, arguments.clone())
            .expect("MCP surface request"),
        RequestedOutputFormat::Json,
        Some(&fixture.client),
    )
    .await
    .expect("MCP application dispatch");
    let http = resolve_http_application_surface(
        operation,
        RequestId::new(format!("request.primitive-parity.http.{case}")).expect("HTTP request id"),
        parse_application_surface_request(operation, arguments).expect("HTTP surface request"),
        RequestedOutputFormat::Json,
        Some(&fixture.client),
    )
    .await
    .expect("HTTP application dispatch");
    assert_eq!(mcp.operation, operation);
    assert_eq!(http.operation, operation);
    assert_eq!(mcp.requested_format, RequestedOutputFormat::Json);
    assert_eq!(http.requested_format, RequestedOutputFormat::Json);
    assert_eq!(
        mcp.binding_id.as_str(),
        format!("binding.mcp.{}.v1", operation.as_str())
    );
    assert_eq!(
        http.binding_id.as_str(),
        format!("binding.http.{}.v1", operation.as_str())
    );
    let expected_contract = if operation == ApplicationSurfaceOperation::TestResults {
        "schema.application.feedback.test-results.result".to_owned()
    } else {
        format!(
            "schema.application.primitive.{}.result",
            operation.as_str().replace('_', "-")
        )
    };
    for result in [&mcp, &http] {
        let envelope = result
            .result
            .as_ref()
            .expect("successful application result");
        assert_eq!(envelope.contract.schema_id().as_str(), expected_contract);
        assert_eq!(envelope.contract.schema_revision(), 1);
        let ApplicationOutcome::Evidence(evidence) = &envelope.outcome else {
            panic!("primitive parity requires an evidence outcome");
        };
        assert_eq!(
            evidence.execution.termination,
            OperationTermination::Completed
        );
        assert_eq!(evidence.coverage.returned, evidence.page.returned);
    }
    assert_eq!(
        cli["contract"]["schema_id"],
        Value::String(expected_contract)
    );
    assert_eq!(cli["contract"]["schema_revision"], 1);
    assert_eq!(cli["outcome"]["outcome"], "evidence");
    assert_eq!(
        cli["outcome"]["value"]["execution"]["termination"],
        "completed"
    );

    let mut cli_envelope = cli.clone();
    let mut mcp_envelope =
        serde_json::to_value(mcp.result.as_ref().expect("MCP result")).expect("MCP envelope");
    let mut http_envelope =
        serde_json::to_value(http.result.as_ref().expect("HTTP result")).expect("HTTP envelope");
    normalize_application_envelope(&mut cli_envelope);
    normalize_application_envelope(&mut mcp_envelope);
    normalize_application_envelope(&mut http_envelope);
    assert_eq!(mcp_envelope, http_envelope);
    assert_eq!(cli_envelope, mcp_envelope);

    let mcp_payload = successful_application(&mcp);
    let http_payload = successful_application(&http);
    assert_eq!(mcp_payload, http_payload);
    assert_eq!(cli["outcome"]["value"]["payload"], *mcp_payload);
    assert_eq!(
        cli["scope"]["project_id"],
        serde_json::to_value(&mcp.result.as_ref().expect("MCP result").scope.project_id)
            .expect("MCP project id")
    );
    assert_eq!(
        mcp.result.as_ref().expect("MCP result").scope,
        http.result.as_ref().expect("HTTP result").scope
    );
    mcp_payload.clone()
}

fn normalize_application_envelope(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for volatile in [
                "request_id",
                "trace_id",
                "requested_at",
                "resolved_at",
                "revalidated_at",
                "started_at",
                "ended_at",
                "effective_deadline",
                "expires_at",
                "observed_at",
                "elapsed_micros",
            ] {
                fields.remove(volatile);
            }
            for value in fields.values_mut() {
                normalize_application_envelope(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                normalize_application_envelope(value);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn feedback_diagnostics_request(request_handle: &str) -> ApplicationSurfaceRequest {
    ApplicationSurfaceRequest::Feedback(
        FeedbackSurfaceRequest::new(request_handle.to_owned()).expect("feedback request"),
    )
}

async fn run_application_http(fixture: &RuntimeFixture, path: &str, arguments: &Value) -> Value {
    let app = http_application_router(
        fixture.client.clone(),
        OperationEventAuthority::default(),
        ProjectId::new("project.runtime-http-mount").expect("HTTP mount project"),
    )
    .expect("production HTTP application router");
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(arguments.to_string()))
                .expect("HTTP application request"),
        )
        .await
        .expect("HTTP application response");
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(
        &to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("HTTP application body"),
    )
    .expect("HTTP application JSON")
}

fn successful_http_payload(result: &Value) -> &Value {
    assert_eq!(result["kind"], "success");
    assert_eq!(result["value"]["outcome"]["outcome"], "evidence");
    &result["value"]["outcome"]["value"]["payload"]
}

fn assert_command_success(label: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{label} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn successful_application(result: &ApplicationSurfaceInvocationResult) -> &Value {
    let envelope = result.result.as_ref().unwrap_or_else(|problem| {
        panic!(
            "{} returned {:?}: {:?}",
            result.operation.as_str(),
            problem.problem.kind(),
            problem.problem
        )
    });
    match &envelope.outcome {
        ApplicationOutcome::Evidence(evidence) => {
            assert_eq!(
                evidence.execution.termination,
                OperationTermination::Completed
            );
            evidence.payload.as_ref().expect("evidence payload")
        }
        other => panic!("expected evidence outcome, got {other:?}"),
    }
}

/// The dashboard's project-settings write must commit through the daemon-owned
/// configuration control plane.
///
/// The in-process `dashboard_api_test` fixture serves a dashboard with no
/// control plane mounted, so it proves the honest refusal
/// (`configuration_authority_unavailable`) and cannot prove the write. This is
/// the production topology instead: a real daemon hosts the dashboard, so the
/// mutation travels the installed invocation executor rather than a double,
/// and the same process that commits the revision republishes the pinned
/// runtime configuration the next read serves.
#[tokio::test(flavor = "multi_thread")]
async fn dashboard_project_settings_commit_through_the_daemon_control_plane() {
    let fixture = runtime_fixture().await;
    let base_url = start_daemon_hosted_dashboard(&fixture).await;
    let agent = common::http_agent();
    let settings_url = format!("{base_url}/api/settings");
    let project_url = format!("{settings_url}/project");

    let (status, envelope) = get_dashboard_json(&agent, &settings_url);
    assert_eq!(status, 200, "GET settings failed: {envelope}");
    let settings = &envelope["payload"];
    let revision = settings["project"]["configuration_revision_id"]
        .as_str()
        .unwrap_or_else(|| panic!("settings must expose a revision: {settings}"))
        .to_owned();
    let legacy_config_path = PathBuf::from(
        settings["project"]["legacy_config_path"]
            .as_str()
            .unwrap_or_else(|| panic!("settings must expose the legacy path: {settings}")),
    );
    let legacy_config_before = std::fs::read(&legacy_config_path).ok();
    let original_max_file_size = settings["project"]["config"]["max_file_size"].clone();

    // The mirror of the in-process assertion: with the control plane mounted,
    // the project apply is advertised as a legal action rather than withheld.
    let advertises_project_apply = envelope["legal_actions"]
        .as_array()
        .unwrap_or_else(|| panic!("expected legal actions: {envelope}"))
        .iter()
        .any(|action| {
            action["kind"] == "request_apply" && action["operation"] == "configuration_batch"
        });
    assert!(
        advertises_project_apply,
        "a daemon-hosted dashboard must advertise the project apply: {envelope}"
    );

    let (status, applied) = patch_dashboard_json(
        &agent,
        &project_url,
        &serde_json::json!({
            "expected_revision_id": revision,
            "exclude": ["target/**", "dist/**"],
            "include": [".github/**"],
            "max_file_size": 2048
        }),
    );
    assert_eq!(
        status, 200,
        "the installed production client must commit the project mutation: {applied}"
    );
    let applied_payload = &applied["payload"];
    assert_eq!(applied_payload["resync_recommended"], true);
    assert_eq!(
        applied_payload["project"]["config"]["max_file_size"], 2048,
        "the response must publish the daemon-returned snapshot: {applied_payload}"
    );
    let applied_revision = applied_payload["project"]["configuration_revision_id"]
        .as_str()
        .unwrap_or_else(|| panic!("mutated response omitted a revision: {applied_payload}"))
        .to_owned();
    assert_ne!(
        applied_revision, revision,
        "a committed mutation must publish a new configuration revision"
    );
    assert_ne!(
        applied_payload["project"]["config"]["max_file_size"], original_max_file_size,
        "the committed value must differ from the pre-change reading"
    );
    assert_eq!(
        std::fs::read(&legacy_config_path).ok(),
        legacy_config_before,
        "a typed mutation must not fall back to config.json"
    );

    // Re-read: the commit is durable and the pinned runtime configuration the
    // dashboard serves advanced with it, rather than the response alone.
    let (status, reloaded) = get_dashboard_json(&agent, &settings_url);
    assert_eq!(status, 200);
    assert_eq!(
        reloaded["payload"]["project"]["config"]["max_file_size"],
        2048
    );
    assert_eq!(
        reloaded["payload"]["project"]["config"]["include"][0],
        ".github/**"
    );
    assert_eq!(
        reloaded["payload"]["project"]["configuration_revision_id"],
        applied_revision.as_str()
    );

    // CAS still holds against the real store: the superseded revision cannot
    // apply a second time.
    let (status, stale) = patch_dashboard_json(
        &agent,
        &project_url,
        &serde_json::json!({
            "expected_revision_id": revision,
            "track_call_sites": false
        }),
    );
    assert_eq!(status, 409, "a stale project patch must conflict: {stale}");
    assert_eq!(stale["code"], "configuration_revision_conflict");
    assert_eq!(stale["actual_revision_id"], applied_revision.as_str());

    let _ = call_default_tool(
        &fixture.handshake,
        "tracedecay_dashboard",
        serde_json::json!({ "action": "stop", "format": "json" }),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_configuration_write_persists_and_rejects_stale_cas() {
    let fixture = runtime_fixture().await;
    let observed = resolve_mcp_application_surface(
        ApplicationSurfaceOperation::ConfigurationObservedState,
        RequestId::new("request.configuration.mcp-observed").unwrap(),
        parse_application_surface_request(
            ApplicationSurfaceOperation::ConfigurationObservedState,
            serde_json::json!({}),
        )
        .unwrap(),
        RequestedOutputFormat::Json,
        Some(&fixture.client),
    )
    .await
    .expect("MCP observed-state dispatch");
    let initial_revision = successful_application(&observed)[0]["desired_revision_id"]
        .as_str()
        .expect("initial configuration revision")
        .to_owned();
    let project_id = observed
        .result
        .as_ref()
        .expect("observed-state application result")
        .scope
        .project_id
        .clone();
    let get_arguments = serde_json::json!({ "key": "diagnostics.prewarm.v1" });
    let current = resolve_mcp_application_surface(
        ApplicationSurfaceOperation::ConfigurationGet,
        RequestId::new("request.configuration.mcp-get-before").unwrap(),
        parse_application_surface_request(
            ApplicationSurfaceOperation::ConfigurationGet,
            get_arguments.clone(),
        )
        .unwrap(),
        RequestedOutputFormat::Json,
        Some(&fixture.client),
    )
    .await
    .expect("MCP configuration get");
    let initial_value = successful_application(&current)["effective_value"]["value"]
        .as_bool()
        .expect("initial diagnostics prewarm value");
    let next_value = !initial_value;
    let set_arguments = serde_json::json!({
        "layer": {
            "kind": "project",
            "project_id": project_id,
        },
        "key": "diagnostics.prewarm.v1",
        "value": {
            "kind": "boolean",
            "value": next_value,
        },
        "expected_revision": initial_revision,
    });
    let applied = resolve_mcp_application_surface(
        ApplicationSurfaceOperation::ConfigurationSet,
        RequestId::new("request.configuration.mcp-set").unwrap(),
        parse_application_surface_request(
            ApplicationSurfaceOperation::ConfigurationSet,
            set_arguments,
        )
        .unwrap(),
        RequestedOutputFormat::Json,
        Some(&fixture.client),
    )
    .await
    .expect("MCP configuration set");
    let applied_envelope = applied.result.as_ref().expect("successful MCP mutation");
    assert!(
        matches!(&applied_envelope.outcome, ApplicationOutcome::Effect(_)),
        "configuration mutation must return an effect receipt"
    );

    let reloaded = resolve_mcp_application_surface(
        ApplicationSurfaceOperation::ConfigurationGet,
        RequestId::new("request.configuration.mcp-get-after").unwrap(),
        parse_application_surface_request(
            ApplicationSurfaceOperation::ConfigurationGet,
            get_arguments,
        )
        .unwrap(),
        RequestedOutputFormat::Json,
        Some(&fixture.client),
    )
    .await
    .expect("MCP configuration re-read");
    assert_eq!(
        successful_application(&reloaded)["effective_value"]["value"],
        next_value,
        "the MCP mutation must affect the retained configuration"
    );

    let stale_arguments = serde_json::json!({
        "layer": {
            "kind": "project",
            "project_id": project_id,
        },
        "key": "diagnostics.prewarm.v1",
        "value": {
            "kind": "boolean",
            "value": initial_value,
        },
        "expected_revision": initial_revision,
    });
    let stale = resolve_mcp_application_surface(
        ApplicationSurfaceOperation::ConfigurationSet,
        RequestId::new("request.configuration.mcp-stale").unwrap(),
        parse_application_surface_request(
            ApplicationSurfaceOperation::ConfigurationSet,
            stale_arguments,
        )
        .unwrap(),
        RequestedOutputFormat::Json,
        Some(&fixture.client),
    )
    .await
    .expect("MCP stale configuration dispatch");
    let problem = stale
        .result
        .expect_err("a stale MCP configuration write must conflict");
    assert_eq!(problem.problem.kind(), ApplicationProblemKind::Conflict);
    assert_eq!(problem.problem.code, "configuration.conflict");
}

/// Starts the dashboard inside the running daemon and returns its base URL.
///
/// This is the same route `tracedecay dashboard` takes: the CLI asks the daemon
/// to serve, so the dashboard shares the daemon's process and its application
/// authorities instead of dialing back over a socket.
async fn start_daemon_hosted_dashboard(fixture: &RuntimeFixture) -> String {
    let started = call_default_tool(
        &fixture.handshake,
        "tracedecay_dashboard",
        serde_json::json!({
            "action": "start",
            "host": "127.0.0.1",
            "port": 0,
            "format": "json"
        }),
    )
    .await
    .expect("start the daemon-hosted dashboard");
    let payload = tracedecay::daemon::tool_json_payload(&started, "tracedecay_dashboard")
        .expect("dashboard tool payload");
    assert!(
        matches!(
            payload["status"].as_str(),
            Some("started" | "already_running")
        ),
        "dashboard did not start: {payload}"
    );
    payload["url"]
        .as_str()
        .unwrap_or_else(|| panic!("dashboard tool returned no url: {payload}"))
        .trim_end_matches('/')
        .to_owned()
}

fn get_dashboard_json(agent: &ureq::Agent, url: &str) -> (u16, Value) {
    let response = common::http_call_with_retry(&format!("GET {url}"), || agent.get(url).call());
    common::response_to_json(response)
}

fn patch_dashboard_json(agent: &ureq::Agent, url: &str, body: &Value) -> (u16, Value) {
    let response =
        common::http_call_with_retry(&format!("PATCH {url}"), || agent.patch(url).send_json(body));
    common::response_to_json(response)
}

#[tokio::test(flavor = "multi_thread")]
async fn project_open_application_boundary() {
    let fixture = runtime_fixture().await;

    let cli = run_storage_status(fixture.home(), &fixture.project, true);
    assert_command_success("CLI storage_status", &cli);
    let cli_value: Value = serde_json::from_slice(&cli.stdout).expect("CLI application JSON");
    assert_eq!(cli_value["outcome"]["outcome"], "evidence");
    assert!(cli_value["scope"]["project_id"].as_str().is_some());
    assert_eq!(
        cli_value["outcome"]["value"]["execution"]["termination"],
        "completed"
    );

    let mcp = resolve_mcp_application_surface(
        ApplicationSurfaceOperation::StorageStatus,
        RequestId::new("request.runtime-acceptance.mcp").expect("request id"),
        storage_status_request(),
        RequestedOutputFormat::Json,
        Some(&fixture.client),
    )
    .await
    .expect("MCP application dispatch");
    let http = resolve_http_application_surface(
        ApplicationSurfaceOperation::StorageStatus,
        RequestId::new("request.runtime-acceptance.http").expect("request id"),
        storage_status_request(),
        RequestedOutputFormat::Json,
        Some(&fixture.client),
    )
    .await
    .expect("HTTP application dispatch");

    let mcp_payload = successful_application(&mcp);
    let http_payload = successful_application(&http);
    assert_eq!(mcp_payload, http_payload);
    assert!(mcp_payload["read_only"].is_boolean());
    assert_ne!(mcp.binding_id, http.binding_id);

    let mcp_scope = &mcp.result.as_ref().expect("MCP result").scope;
    let http_scope = &http.result.as_ref().expect("HTTP result").scope;
    assert_eq!(mcp_scope, http_scope);
    assert_eq!(
        serde_json::to_value(&mcp_scope.project_id).expect("project id"),
        cli_value["scope"]["project_id"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn production_primitive_code_routes_have_cli_mcp_http_parity() {
    let fixture = runtime_fixture().await;
    let page = serde_json::json!({ "page_size": 10, "cursor": null });
    let authenticate = assert_application_transport_parity(
        &fixture,
        "qualified-name-authenticate",
        ApplicationSurfaceOperation::QualifiedName,
        serde_json::json!({
            "qualified_name": "src/auth/login.rs::authenticate",
            "page": page,
        }),
    )
    .await;
    let authenticate_id = authenticate["symbols"][0]["node_id"]
        .as_str()
        .expect("authenticate fixture symbol");
    assert_eq!(authenticate["symbols"][0]["file"], "src/auth/login.rs");

    let create_session = assert_application_transport_parity(
        &fixture,
        "qualified-name-create-session",
        ApplicationSurfaceOperation::QualifiedName,
        serde_json::json!({
            "qualified_name": "src/auth/session.rs::create_session",
            "page": { "page_size": 10, "cursor": null },
        }),
    )
    .await;
    let create_session_id = create_session["symbols"][0]["node_id"]
        .as_str()
        .expect("create_session fixture symbol");

    let call_chain = assert_application_transport_parity(
        &fixture,
        "call-chain",
        ApplicationSurfaceOperation::CallChain,
        serde_json::json!({
            "from_node_id": authenticate_id,
            "to_node_id": create_session_id,
            "maximum_depth": 8,
        }),
    )
    .await;
    assert_eq!(call_chain["node_ids"][0], authenticate_id);
    assert_eq!(
        call_chain["node_ids"]
            .as_array()
            .expect("call-chain nodes")
            .last()
            .and_then(Value::as_str),
        Some(create_session_id)
    );

    let dependents = assert_application_transport_parity(
        &fixture,
        "file-dependents",
        ApplicationSurfaceOperation::FileDependents,
        serde_json::json!({ "file": "src/auth/session.rs" }),
    )
    .await;
    assert_eq!(dependents["file"], "src/auth/session.rs");
    assert!(dependents["dependent_files"].as_array().is_some());

    let source_path = fixture.project.join("src/auth/login.rs");
    let source_len = std::fs::metadata(source_path)
        .expect("fixture source metadata")
        .len();
    let source_lines = assert_application_transport_parity(
        &fixture,
        "source-lines",
        ApplicationSurfaceOperation::SourceLines,
        serde_json::json!({
            "file": "src/auth/login.rs",
            "span": { "start_byte": 0, "end_byte": source_len },
            "meta": {
                "temporal": { "kind": "current" },
                "page": { "page_size": 10, "cursor": null },
                "projection": "references_only",
                "order": "source_position",
            },
        }),
    )
    .await;
    assert_eq!(
        source_lines["references"][0]["span"],
        serde_json::json!({ "start_byte": 0, "end_byte": source_len })
    );

    let source_body = assert_application_transport_parity(
        &fixture,
        "source-body",
        ApplicationSurfaceOperation::SourceBody,
        serde_json::json!({ "node_id": authenticate_id }),
    )
    .await;
    assert_eq!(source_body["file"], "src/auth/login.rs");
    assert!(
        source_body["body"]
            .as_str()
            .expect("authenticate source body")
            .contains("create_session(username)")
    );
}

#[cfg(all(unix, feature = "test-transport"))]
#[tokio::test(flavor = "multi_thread")]
async fn git_preview_and_apply_have_real_cli_mcp_runtime_parity() {
    let fixture = git_runtime_fixture().await;
    let status = resolve_mcp_application_surface(
        ApplicationSurfaceOperation::GitStatus,
        RequestId::new("request.git-parity.status").expect("status request id"),
        parse_application_surface_request(
            ApplicationSurfaceOperation::GitStatus,
            serde_json::json!({}),
        )
        .expect("Git status request"),
        RequestedOutputFormat::Json,
        Some(&fixture.client),
    )
    .await
    .expect("Git status dispatch");
    let scope = status
        .result
        .as_ref()
        .expect("Git status result")
        .scope
        .clone();
    // The daemon checks the caller's snapshot identity against the ids on the
    // scope the invoking surface resolved, so a CLI scope that differs from
    // the MCP scope this snapshot is built from would be rejected before any
    // repository state is compared. Prove the two surfaces agree first, or the
    // preview failure below is unattributable.
    let cli_status = run_application_tool(
        fixture.home(),
        &fixture.project,
        ApplicationSurfaceOperation::GitStatus,
        &serde_json::json!({}),
    );
    assert_command_success("CLI git_status", &cli_status);
    let cli_status: ApplicationEnvelope<Value> =
        serde_json::from_slice(&cli_status.stdout).expect("CLI status envelope");
    assert_eq!(
        cli_status.scope.project_id, scope.project_id,
        "CLI and MCP must resolve the same project for one repository"
    );
    assert_eq!(
        cli_status.scope.repository_id, scope.repository_id,
        "CLI and MCP must resolve the same repository identity"
    );
    assert_eq!(
        cli_status.scope.worktree_id, scope.worktree_id,
        "CLI and MCP must resolve the same worktree identity"
    );

    let original_head = git_stdout(&fixture.project, &["rev-parse", "HEAD"]);
    let captured_at = wall_clock_micros();
    let snapshot = tracedecay::daemon::capture_exact_git_snapshot_for_test(
        &fixture.project,
        scope.project_id.clone(),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        captured_at,
    )
    .expect("exact transaction snapshot");
    let identity = GitCommitIdentityV1 {
        name: "TraceDecay Test".to_owned(),
        email: "tracedecay@example.com".to_owned(),
        at: captured_at,
    };
    let preview_request = GitPreviewSurfaceRequest {
        operation: GitIndexTransactionOperationV1::CommitIndex,
        preview_id: GitIndexPreviewId::new("preview.transport-parity").expect("preview id"),
        repository_snapshot: snapshot,
        selected_hunks: Vec::new(),
        commit_intent: Some(
            GitIndexCommitIntentV1::new(
                "test: prove Git transport parity\n".to_owned(),
                identity.clone(),
                identity,
                GitIndexSigningPolicyV1::UnsignedPermitted,
            )
            .expect("commit intent"),
        ),
    };
    let mut preview_arguments = serde_json::to_value(&preview_request).expect("preview arguments");
    preview_arguments
        .as_object_mut()
        .expect("preview object")
        .remove("preview_id");

    let cli_preview = run_application_tool(
        fixture.home(),
        &fixture.project,
        ApplicationSurfaceOperation::GitPreview,
        &preview_arguments,
    );
    assert_command_success("CLI git_preview", &cli_preview);
    let cli_preview: ApplicationEnvelope<Value> =
        serde_json::from_slice(&cli_preview.stdout).expect("CLI preview envelope");
    let ApplicationOutcome::Preview(cli_preview) = cli_preview.outcome else {
        panic!("CLI git_preview must return a preview outcome");
    };
    let cli_preview_payload: GitIndexPreviewV1 =
        serde_json::from_value(cli_preview.payload.expect("CLI immutable preview"))
            .expect("CLI immutable preview");

    let mcp_preview = resolve_mcp_application_surface(
        ApplicationSurfaceOperation::GitPreview,
        RequestId::new("request.git-parity.preview.mcp").expect("MCP preview request id"),
        parse_application_surface_request(
            ApplicationSurfaceOperation::GitPreview,
            preview_arguments,
        )
        .expect("MCP preview request"),
        RequestedOutputFormat::Json,
        Some(&fixture.client),
    )
    .await
    .expect("MCP preview dispatch");
    assert_eq!(
        mcp_preview.binding_id.as_str(),
        "binding.mcp.git_preview.v1"
    );
    let ApplicationOutcome::Preview(mcp_preview) = &mcp_preview
        .result
        .as_ref()
        .expect("MCP preview result")
        .outcome
    else {
        panic!("MCP git_preview must return a preview outcome");
    };
    assert_eq!(
        mcp_preview.execution.termination,
        OperationTermination::Completed
    );
    let mcp_preview_payload: GitIndexPreviewV1 =
        serde_json::from_value(mcp_preview.payload.clone().expect("MCP immutable preview"))
            .expect("typed MCP preview");
    assert_eq!(
        cli_preview_payload.repository_snapshot,
        mcp_preview_payload.repository_snapshot
    );
    assert_eq!(
        cli_preview_payload.candidate_index_tree,
        mcp_preview_payload.candidate_index_tree
    );
    assert_eq!(
        cli_preview_payload.commit_intent_digest,
        mcp_preview_payload.commit_intent_digest
    );
    assert_eq!(
        cli_preview_payload.disposition,
        mcp_preview_payload.disposition
    );

    let apply_arguments = serde_json::to_value(GitApplySurfaceRequest {
        preview: cli_preview_payload,
        idempotency_key: IdempotencyKey::new("idempotency.git-transport-parity")
            .expect("idempotency key"),
    })
    .expect("apply arguments");
    let cli_apply = run_application_tool(
        fixture.home(),
        &fixture.project,
        ApplicationSurfaceOperation::GitApply,
        &apply_arguments,
    );
    assert_command_success("CLI git_apply", &cli_apply);
    let cli_apply_wire: Value = serde_json::from_slice(&cli_apply.stdout).expect("CLI apply JSON");
    assert!(
        cli_apply_wire.get("problem").is_none(),
        "CLI git_apply problem: {cli_apply_wire:#}"
    );
    let cli_apply: ApplicationEnvelope<Value> =
        serde_json::from_value(cli_apply_wire).expect("CLI apply envelope");
    let ApplicationOutcome::Effect(cli_apply) = cli_apply.outcome else {
        panic!("CLI git_apply must return an effect outcome");
    };
    assert_eq!(
        cli_apply.execution.termination,
        OperationTermination::Completed
    );
    let cli_receipt: GitIndexTransactionReceiptV1 =
        serde_json::from_value(cli_apply.payload.clone().expect("CLI durable Git receipt"))
            .expect("typed CLI Git receipt");
    assert_eq!(cli_receipt.outcome, GitIndexReceiptOutcomeV1::Committed);
    let committed_head = git_stdout(&fixture.project, &["rev-parse", "HEAD"]);
    assert_ne!(
        committed_head, original_head,
        "CLI apply must commit the previewed native Git index"
    );
    assert_eq!(
        git_stdout(&fixture.project, &["log", "-1", "--format=%s"]),
        "test: prove Git transport parity"
    );

    let mcp_apply = resolve_mcp_application_surface(
        ApplicationSurfaceOperation::GitApply,
        RequestId::new("request.git-parity.apply.mcp").expect("MCP apply request id"),
        parse_application_surface_request(ApplicationSurfaceOperation::GitApply, apply_arguments)
            .expect("MCP apply request"),
        RequestedOutputFormat::Json,
        Some(&fixture.client),
    )
    .await
    .expect("MCP apply replay");
    assert_eq!(mcp_apply.binding_id.as_str(), "binding.mcp.git_apply.v1");
    let ApplicationOutcome::Effect(mcp_apply) =
        &mcp_apply.result.as_ref().expect("MCP apply result").outcome
    else {
        panic!("MCP git_apply must return an effect outcome");
    };
    assert_eq!(
        mcp_apply.execution.termination,
        OperationTermination::Completed
    );
    let normalize_replay = |effect: &tracedecay_application::EffectResult<Value>| {
        let mut value = serde_json::to_value(effect).expect("effect wire value");
        value["authority"]
            .as_object_mut()
            .expect("authority object")
            .remove("revalidated_at");
        value
            .as_object_mut()
            .expect("effect object")
            .remove("execution");
        value["receipt"]
            .as_object_mut()
            .expect("receipt object")
            .remove("request_id");
        value
    };
    assert_eq!(normalize_replay(&cli_apply), normalize_replay(mcp_apply));
    assert_eq!(
        git_stdout(&fixture.project, &["rev-parse", "HEAD"]),
        committed_head,
        "an MCP replay must return the original receipt without a duplicate commit"
    );
    git(&fixture.project, &["diff", "--cached", "--quiet"]);

    std::fs::write(
        fixture.project.join("src/main.rs"),
        "mod cli;\n\nfn main() {\n    cli::run();\n}\n\n// conflicting replay\n",
    )
    .expect("write conflicting replay change");
    git(&fixture.project, &["add", "src/main.rs"]);
    let conflicting_preview = preview_commit_via_mcp(
        &fixture,
        &scope,
        "request.git-parity.conflicting-preview",
        "test: conflicting replay\n",
    )
    .await;
    let before_conflicting_replay_tree = git_stdout(&fixture.project, &["write-tree"]);
    let conflicting_replay = resolve_mcp_application_surface(
        ApplicationSurfaceOperation::GitApply,
        RequestId::new("request.git-parity.conflicting-replay")
            .expect("conflicting replay request id"),
        ApplicationSurfaceRequest::GitApply(GitApplySurfaceRequest {
            preview: conflicting_preview,
            idempotency_key: IdempotencyKey::new("idempotency.git-transport-parity")
                .expect("reused idempotency key"),
        }),
        RequestedOutputFormat::Json,
        Some(&fixture.client),
    )
    .await
    .expect("MCP conflicting replay dispatch");
    let conflicting_problem = conflicting_replay
        .result
        .expect_err("changed input under one idempotency key must be rejected");
    assert_eq!(
        conflicting_problem.problem.kind(),
        ApplicationProblemKind::Conflict
    );
    assert_eq!(
        git_stdout(&fixture.project, &["rev-parse", "HEAD"]),
        committed_head
    );
    assert_eq!(
        git_stdout(&fixture.project, &["write-tree"]),
        before_conflicting_replay_tree,
        "conflicting replay rejection must not alter the native index"
    );

    let stale_preview = preview_commit_via_mcp(
        &fixture,
        &scope,
        "request.git-parity.stale-preview",
        "test: stale CAS must not commit\n",
    )
    .await;
    std::fs::write(
        fixture.project.join("src/main.rs"),
        "mod cli;\n\nfn main() {\n    cli::run();\n}\n\n// drift after preview\n",
    )
    .expect("write post-preview drift");
    git(&fixture.project, &["add", "src/main.rs"]);
    let drifted_index_tree = git_stdout(&fixture.project, &["write-tree"]);
    let stale_apply_arguments = serde_json::to_value(GitApplySurfaceRequest {
        preview: stale_preview,
        idempotency_key: IdempotencyKey::new("idempotency.git-transport-parity.stale")
            .expect("stale apply idempotency key"),
    })
    .expect("stale apply arguments");
    let stale_cli_apply = run_application_tool(
        fixture.home(),
        &fixture.project,
        ApplicationSurfaceOperation::GitApply,
        &stale_apply_arguments,
    );
    assert_command_success("CLI stale git_apply", &stale_cli_apply);
    let stale_cli_apply: ApplicationEnvelope<Value> =
        serde_json::from_slice(&stale_cli_apply.stdout).expect("CLI stale apply envelope");
    let ApplicationOutcome::Effect(stale_cli_apply) = stale_cli_apply.outcome else {
        panic!("stale CLI git_apply must return an authoritative no-change effect");
    };
    assert_eq!(
        stale_cli_apply.execution.termination,
        OperationTermination::Failed
    );
    let stale_receipt: GitIndexTransactionReceiptV1 = serde_json::from_value(
        stale_cli_apply
            .payload
            .expect("CLI stale apply no-change receipt"),
    )
    .expect("typed CLI stale apply receipt");
    assert_eq!(
        stale_receipt.outcome,
        GitIndexReceiptOutcomeV1::AbortedNoChange
    );
    assert_eq!(
        git_stdout(&fixture.project, &["rev-parse", "HEAD"]),
        committed_head,
        "CAS drift must not create a commit"
    );
    assert_eq!(
        git_stdout(&fixture.project, &["write-tree"]),
        drifted_index_tree,
        "CAS drift rejection must preserve the caller's newer index"
    );

    let cancellation_preview = preview_commit_via_mcp(
        &fixture,
        &scope,
        "request.git-parity.cancellation-preview",
        "test: cancelled apply must not commit\n",
    )
    .await;
    let cancellation_head = git_stdout(&fixture.project, &["rev-parse", "HEAD"]);
    let cancellation_tree = git_stdout(&fixture.project, &["write-tree"]);
    let cancellation_deadline =
        Deadline::new(UtcMicros(wall_clock_micros().0.saturating_add(60_000_000)))
            .expect("cancellation deadline");
    let mut canonical_cancellation = None;
    for (surface, surface_name) in [(BindingSurface::Cli, "cli"), (BindingSurface::Mcp, "mcp")] {
        let request_id = RequestId::new(format!("request.git-parity.cancelled.{surface_name}"))
            .expect("cancelled apply request id");
        let cancellation = CancellationSignal::active(format!("cancel.git-parity.{surface_name}"))
            .expect("cancelled apply signal");
        assert!(cancellation.cancel(wall_clock_micros()));
        let dispatched = resolve_application_surface_dispatch_with_controls(
            surface,
            ApplicationSurfaceOperation::GitApply,
            request_id,
            ApplicationSurfaceRequest::GitApply(GitApplySurfaceRequest {
                preview: cancellation_preview.clone(),
                idempotency_key: IdempotencyKey::new("idempotency.git-transport-parity.cancelled")
                    .expect("cancelled apply idempotency key"),
            }),
            PageRequest::first(10).expect("page"),
            Some(cancellation_deadline.clone()),
            cancellation,
            RequestedOutputFormat::Json,
        )
        .expect("cancelled Git apply dispatch");
        let result = execute_application_surface(
            ApplicationSurfaceOperation::GitApply,
            dispatched,
            Some(&fixture.client),
        )
        .await
        .expect("cancelled Git apply invocation");
        assert_eq!(
            result.binding_id.as_str(),
            format!("binding.{surface_name}.git_apply.v1")
        );
        let problem = result
            .result
            .expect_err("pre-cancelled Git apply must not be admitted");
        assert_eq!(problem.problem.kind(), ApplicationProblemKind::Cancelled);
        assert_eq!(
            problem.problem.cancellation_stage,
            Some(CancellationStage::BeforeAdmission)
        );
        let mut normalized = serde_json::to_value(problem).expect("cancelled Git apply problem");
        normalize_application_envelope(&mut normalized);
        if let Some(expected) = &canonical_cancellation {
            assert_eq!(&normalized, expected);
        } else {
            canonical_cancellation = Some(normalized);
        }
    }
    assert_eq!(
        git_stdout(&fixture.project, &["rev-parse", "HEAD"]),
        cancellation_head
    );
    assert_eq!(
        git_stdout(&fixture.project, &["write-tree"]),
        cancellation_tree,
        "CLI/MCP cancellation must leave native Git state unchanged"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn production_lsp_negotiates_and_projects_canonical_context() {
    let fixture = lsp_runtime_fixture().await;
    let root_uri = url::Url::from_directory_path(&fixture.project)
        .expect("project root URI")
        .to_string();
    let document_uri = url::Url::from_file_path(fixture.project.join("src/auth/login.rs"))
        .expect("document URI")
        .to_string();
    let source = std::fs::read_to_string(fixture.project.join("src/auth/login.rs"))
        .expect("checked-in fixture source");
    let mut session = DaemonLspSessionClient::open(
        fixture.client.clone(),
        "3.17",
        Some(root_uri.clone()),
        Vec::new(),
    )
    .await
    .expect("open production daemon LSP session");

    let projections = [
        "diagnostics",
        "postEditImpact",
        "affectedTests",
        "testRunResults",
    ]
    .into_iter()
    .map(|kind| {
        serde_json::json!({
            "kind": kind,
            "revision": TRACEDECAY_CONTEXT_REVISION,
        })
    })
    .collect::<Vec<_>>();
    send_lsp(
        &mut session,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "rootUri": root_uri,
                "capabilities": {
                    "general": { "positionEncodings": ["utf-16"] },
                    // Standard methods are negotiated through standard client
                    // capabilities, independently of the TraceDecay extension
                    // below. A client that never declares them is correctly
                    // refused, so this test declares the two it drives later.
                    "textDocument": {
                        "documentSymbol": { "dynamicRegistration": false },
                        "hover": { "dynamicRegistration": false },
                    },
                    "experimental": {
                        "tracedecay": {
                            "revision": TRACEDECAY_CONTEXT_REVISION,
                            "opaqueExpansion": true,
                            "projections": projections,
                        }
                    }
                }
            }
        }),
    )
    .await;
    let initialized = poll_lsp_response(&mut session, 1).await;
    assert_eq!(
        initialized["result"]["capabilities"]["positionEncoding"], "utf-16",
        "unexpected initialize response: {initialized}"
    );
    let negotiated = &initialized["result"]["capabilities"]["experimental"]["tracedecay"];
    assert_eq!(negotiated["revision"], TRACEDECAY_CONTEXT_REVISION);
    assert_eq!(negotiated["opaqueExpansion"], true);
    assert_eq!(
        negotiated["projections"]
            .as_array()
            .expect("negotiated projection registrations")
            .len(),
        4
    );

    send_lsp(
        &mut session,
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {},
        }),
    )
    .await;
    send_lsp(
        &mut session,
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": document_uri,
                    "languageId": "rust",
                    "version": 1,
                    "text": source,
                }
            }
        }),
    )
    .await;
    send_lsp(
        &mut session,
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didSave",
            "params": { "textDocument": { "uri": document_uri } },
        }),
    )
    .await;

    let projection = poll_lsp_context(&mut session, &document_uri, "diagnostics", 2).await;
    // The gateway compares roots by parsed path rather than by raw string,
    // because a directory URI legitimately arrives with or without a trailing
    // slash, so the projection is checked the same way it is admitted.
    let projected_root = projection["result"]["rootUri"]
        .as_str()
        .expect("projected root URI");
    assert_eq!(
        url::Url::parse(projected_root)
            .ok()
            .and_then(|url| url.to_file_path().ok()),
        Some(fixture.project.clone()),
        "the projection must name the admitted root, got {projected_root}"
    );
    assert_eq!(projection["result"]["documentUri"], document_uri);
    assert_eq!(projection["result"]["kind"], "diagnostics");
    assert_eq!(
        projection["result"]["revision"],
        TRACEDECAY_CONTEXT_REVISION
    );
    assert!(projection["result"]["generation"].as_u64().is_some());
    assert!(
        projection["result"]["identity"]["headCommitId"]
            .as_str()
            .is_some()
    );
    assert!(
        projection["result"]["identity"]["codeGenerationId"]
            .as_str()
            .is_some()
    );
    // This project has ingested no diagnostics, which is every project's first
    // run. The feedback authority answers such a read with terminal evidence
    // and no cycle, and the LSP runtime used to turn that into a hard failure,
    // so a fresh project could not obtain any context projection at all. The
    // projection must arrive with real identity and state its own degradation
    // rather than either failing or claiming a completeness it does not have.
    let coverage = projection["result"]["coverage"]
        .as_str()
        .expect("projection coverage");
    let producer_state = projection["result"]["producerState"]
        .as_str()
        .expect("projection producer state");
    let items = projection["result"]["items"]
        .as_array()
        .expect("projection items");
    if items.is_empty() {
        assert_ne!(
            coverage, "complete",
            "an empty projection must not report complete coverage: {projection}"
        );
        assert_ne!(
            producer_state, "complete",
            "an empty projection must not report a complete producer: {projection}"
        );
    }
    for (request_id, method, params) in [
        (
            403,
            "textDocument/documentSymbol",
            serde_json::json!({ "textDocument": { "uri": document_uri } }),
        ),
        (
            404,
            "textDocument/hover",
            serde_json::json!({
                "textDocument": { "uri": document_uri },
                "position": { "line": 0, "character": 0 },
            }),
        ),
    ] {
        // A warming analyzer is answered with ServerCancelled carrying
        // retriggerRequest, which is the gateway's "ask again" signal rather
        // than a failure. A compliant client re-sends on it, under one outer
        // budget, instead of treating the first not-ready reply as the answer.
        // Each attempt needs its own id; the gateway rejects a duplicate.
        let retrigger_deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        let mut attempt = 0_u64;
        let navigation = loop {
            let attempt_id = request_id + attempt * 10_000;
            send_lsp(
                &mut session,
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": attempt_id,
                    "method": method,
                    "params": params.clone(),
                }),
            )
            .await;
            let response = poll_lsp_response(&mut session, attempt_id).await;
            let retrigger = response["error"]["data"]["retriggerRequest"]
                .as_bool()
                .unwrap_or(false);
            if !retrigger || std::time::Instant::now() >= retrigger_deadline {
                break response;
            }
            attempt += 1;
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        };
        assert!(
            navigation.get("result").is_some(),
            "{method} must return a standard LSP result: {navigation}"
        );
    }

    send_lsp(
        &mut session,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 405,
            "method": "tracedecay/arbitrary",
            "params": {},
        }),
    )
    .await;
    let arbitrary_method = poll_lsp_response(&mut session, 405).await;
    assert_eq!(arbitrary_method["error"]["code"], -32601);
    assert_eq!(
        arbitrary_method["error"]["data"]["reason"],
        "capabilityNotNegotiated"
    );

    send_lsp(
        &mut session,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 406,
            "method": "tracedecay/context",
            "params": {
                "kind": "diagnostics",
                "arbitraryPayload": { "command": "run" },
            },
        }),
    )
    .await;
    let arbitrary_payload = poll_lsp_response(&mut session, 406).await;
    assert_eq!(arbitrary_payload["error"]["code"], -32602);

    let mut related_lsp_handles = Vec::new();
    for (kind, first_request_id) in [("postEditImpact", 102), ("affectedTests", 202)] {
        let related = poll_lsp_context(&mut session, &document_uri, kind, first_request_id).await;
        assert_eq!(
            related["result"]["rootUri"],
            projection["result"]["rootUri"]
        );
        assert_eq!(related["result"]["documentUri"], document_uri);
        assert_eq!(related["result"]["kind"], kind);
        assert_eq!(related["result"]["revision"], TRACEDECAY_CONTEXT_REVISION);
        assert_eq!(
            related["result"]["identity"],
            projection["result"]["identity"]
        );
        if let Some(handle) = related["result"]["retrievalHandle"].as_str() {
            related_lsp_handles.push(handle.to_owned());
        } else {
            assert_eq!(
                related["result"]["coverage"], "failed",
                "a handle-less {kind} projection must expose failed coverage: {related}"
            );
            assert_eq!(related["result"]["producerState"], "failed");
        }
    }

    let managed_run = call_default_tool(
        &fixture.handshake,
        "tracedecay_run_affected_tests",
        serde_json::json!({
            "changed_paths": ["src/auth/login.rs"],
            "profile": "debug",
            "timeout_secs": 60,
            "max_tests": 1,
            "format": "json",
        }),
    )
    .await;
    let managed_run = managed_run.expect("run affected tests through production daemon");
    let managed_run: Value = serde_json::from_str(
        managed_run["content"][0]["text"]
            .as_str()
            .expect("managed test-run JSON content"),
    )
    .expect("managed test-run payload");
    assert_eq!(managed_run["passed"], 1);
    assert_eq!(managed_run["failed"], 0);

    let test_results = poll_lsp_context(&mut session, &document_uri, "testRunResults", 302).await;
    assert_eq!(
        test_results["result"]["rootUri"],
        projection["result"]["rootUri"]
    );
    assert_eq!(test_results["result"]["documentUri"], document_uri);
    assert_eq!(test_results["result"]["kind"], "testRunResults");
    assert_eq!(
        test_results["result"]["revision"],
        TRACEDECAY_CONTEXT_REVISION
    );
    assert_eq!(test_results["result"]["coverage"], "complete");
    assert_eq!(test_results["result"]["producerState"], "complete");
    assert_eq!(
        test_results["result"]["identity"],
        projection["result"]["identity"]
    );
    assert_eq!(
        test_results["result"]["items"]
            .as_array()
            .expect("managed test-run projection items")
            .len(),
        1
    );
    let canonical_test_results = assert_application_transport_parity(
        &fixture,
        "test-results",
        ApplicationSurfaceOperation::TestResults,
        serde_json::json!({}),
    )
    .await;
    let http_test_results =
        run_application_http(&fixture, "/tests/results", &serde_json::json!({})).await;
    assert_eq!(
        successful_http_payload(&http_test_results),
        &canonical_test_results
    );
    assert_eq!(
        canonical_test_results["head_commit_id"],
        test_results["result"]["identity"]["headCommitId"]
    );
    assert_eq!(
        canonical_test_results["code_generation_id"],
        test_results["result"]["identity"]["codeGenerationId"]
    );
    assert_eq!(
        canonical_test_results["results"].as_array().map(Vec::len),
        Some(1)
    );

    'feedback_handle_parity: {
        let Some(lsp_handle) = projection["result"]["retrievalHandle"].as_str() else {
            assert_eq!(projection["result"]["coverage"], "failed");
            assert_eq!(projection["result"]["producerState"], "failed");
            assert!(
                projection["result"]["omissionReasons"]
                    .as_array()
                    .is_some_and(|reasons| reasons
                        .iter()
                        .any(|reason| reason == "producer-failed")),
                "a handle-less projection must expose the producer failure: {projection}"
            );
            break 'feedback_handle_parity;
        };
        assert_eq!(
            related_lsp_handles.len(),
            2,
            "cycle-backed related projections must expose retrieval handles"
        );
        let handle_record = match retrieve_response_handle(
            &fixture.project,
            lsp_handle,
            wall_clock_micros().0.div_euclid(1_000_000),
        )
        .expect("read LSP response handle through its authority")
        {
            ResponseHandleLookup::Found(record) => record,
            other => panic!("LSP response handle unavailable: {other:?}"),
        };
        let handle_record: Value =
            serde_json::from_str(&handle_record.content).expect("LSP response handle record");
        let canonical_handle = handle_record["canonical_handle"]
            .as_str()
            .expect("canonical feedback handle");

        let cli = run_feedback_diagnostics(fixture.home(), &fixture.project, canonical_handle);
        assert_command_success("CLI feedback_diagnostics", &cli);
        let cli: Value = serde_json::from_slice(&cli.stdout).expect("CLI feedback JSON");
        let mcp = resolve_mcp_application_surface(
            ApplicationSurfaceOperation::FeedbackDiagnostics,
            RequestId::new("request.feedback-parity.mcp").expect("request id"),
            feedback_diagnostics_request(canonical_handle),
            RequestedOutputFormat::Json,
            Some(&fixture.client),
        )
        .await
        .expect("MCP feedback dispatch");
        let feedback_arguments = serde_json::json!({ "request_handle": canonical_handle });
        let http =
            run_application_http(&fixture, "/feedback/diagnostics", &feedback_arguments).await;
        let mcp_payload = successful_application(&mcp);
        let http_payload = successful_http_payload(&http);
        assert_eq!(mcp_payload, http_payload);
        assert_eq!(cli["outcome"]["value"]["payload"], *mcp_payload);
        assert_eq!(
            http["value"]["scope"],
            serde_json::to_value(&mcp.result.as_ref().expect("MCP result").scope)
                .expect("MCP feedback scope")
        );

        for lsp_handle in related_lsp_handles {
            let record = match retrieve_response_handle(
                &fixture.project,
                &lsp_handle,
                wall_clock_micros().0.div_euclid(1_000_000),
            )
            .expect("read related LSP response handle")
            {
                ResponseHandleLookup::Found(record) => record,
                other => panic!("related LSP response handle unavailable: {other:?}"),
            };
            let record: Value =
                serde_json::from_str(&record.content).expect("related LSP handle record");
            assert_eq!(record["canonical_handle"], canonical_handle);
        }

        for (operation, path, expected_contract, projection_key) in [
            (
                ApplicationSurfaceOperation::FeedbackImpact,
                "/feedback/impact",
                "schema.application.feedback.impact.result",
                "impact",
            ),
            (
                ApplicationSurfaceOperation::AffectedTests,
                "/tests/affected",
                "schema.application.feedback.affected-tests.result",
                "affected_tests",
            ),
        ] {
            let cli = run_application_tool(
                fixture.home(),
                &fixture.project,
                operation,
                &feedback_arguments,
            );
            assert_command_success(operation.as_str(), &cli);
            let cli: Value =
                serde_json::from_slice(&cli.stdout).expect("CLI feedback projection JSON");
            let mcp = resolve_mcp_application_surface(
                operation,
                RequestId::new(format!(
                    "request.feedback-parity.mcp.{}",
                    operation.as_str()
                ))
                .expect("request id"),
                parse_application_surface_request(operation, feedback_arguments.clone())
                    .expect("feedback projection request"),
                RequestedOutputFormat::Json,
                Some(&fixture.client),
            )
            .await
            .expect("MCP feedback projection dispatch");
            let http = run_application_http(&fixture, path, &feedback_arguments).await;
            assert_eq!(mcp.operation, operation);
            assert_eq!(
                mcp.binding_id.as_str(),
                format!("binding.mcp.{}.v1", operation.as_str())
            );
            let mcp_envelope = mcp.result.as_ref().expect("MCP feedback projection");
            assert_eq!(
                mcp_envelope.contract.schema_id().as_str(),
                expected_contract
            );
            assert_eq!(mcp_envelope.contract.schema_revision(), 1);
            let ApplicationOutcome::Evidence(evidence) = &mcp_envelope.outcome else {
                panic!("feedback projection requires an evidence outcome");
            };
            assert_eq!(
                evidence.execution.termination,
                OperationTermination::Completed
            );
            assert_eq!(evidence.coverage.returned, evidence.page.returned);
            assert_eq!(
                cli["contract"]["schema_id"],
                Value::String(expected_contract.to_owned())
            );
            assert_eq!(cli["outcome"]["outcome"], "evidence");
            assert_eq!(
                cli["outcome"]["value"]["execution"]["termination"],
                "completed"
            );
            assert_eq!(http["value"]["contract"]["schema_id"], expected_contract);
            assert_eq!(
                http["value"]["binding_id"],
                format!("binding.http.{}.v1", operation.as_str())
            );
            let projected_payload = successful_application(&mcp);
            assert_ne!(projected_payload, mcp_payload);
            assert!(
                projected_payload.get(projection_key).is_some(),
                "{} must expose its distinct {projection_key} projection",
                operation.as_str()
            );
            assert_eq!(successful_http_payload(&http), projected_payload);
            assert_eq!(cli["outcome"]["value"]["payload"], *projected_payload);

            let mut cli_envelope = cli.clone();
            let mut mcp_envelope =
                serde_json::to_value(mcp_envelope).expect("MCP feedback projection envelope");
            let mut http_envelope = http["value"].clone();
            http_envelope
                .as_object_mut()
                .expect("HTTP success envelope")
                .remove("binding_id");
            normalize_application_envelope(&mut cli_envelope);
            normalize_application_envelope(&mut mcp_envelope);
            normalize_application_envelope(&mut http_envelope);
            assert_eq!(cli_envelope, mcp_envelope);
            assert_eq!(mcp_envelope, http_envelope);
        }

        send_lsp(
            &mut session,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 402,
                "method": "tracedecay/context/expand",
                "params": { "retrievalHandle": lsp_handle },
            }),
        )
        .await;
        let expanded = poll_lsp_response(&mut session, 402).await;
        assert_eq!(expanded["result"]["coverage"], "complete");
        assert_eq!(
            expanded["result"]["evidence"]["Ok"]["outcome"]["value"]["payload"],
            *mcp_payload
        );
    }

    let mut incompatible = DaemonLspSessionClient::open(
        fixture.client.clone(),
        "3.17",
        Some(root_uri.clone()),
        Vec::new(),
    )
    .await
    .expect("open incompatible-version daemon LSP session");
    send_lsp(
        &mut incompatible,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 501,
            "method": "initialize",
            "params": {
                "rootUri": root_uri,
                "capabilities": {
                    "general": { "positionEncodings": ["utf-16"] },
                    "experimental": {
                        "tracedecay": {
                            "revision": TRACEDECAY_CONTEXT_REVISION + 1,
                            "opaqueExpansion": true,
                            "projections": [{
                                "kind": "diagnostics",
                                "revision": TRACEDECAY_CONTEXT_REVISION + 1,
                            }],
                        }
                    }
                }
            }
        }),
    )
    .await;
    let incompatible_initialize = poll_lsp_response(&mut incompatible, 501).await;
    assert!(
        incompatible_initialize["result"]["capabilities"]["experimental"]["tracedecay"].is_null(),
        "an incompatible revision must not be negotiated"
    );
    send_lsp(
        &mut incompatible,
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {},
        }),
    )
    .await;
    send_lsp(
        &mut incompatible,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 502,
            "method": "tracedecay/context",
            "params": {
                "kind": "diagnostics",
                "documentUri": document_uri,
            },
        }),
    )
    .await;
    let incompatible_context = poll_lsp_response(&mut incompatible, 502).await;
    assert_eq!(incompatible_context["error"]["code"], -32601);
    assert_eq!(
        incompatible_context["error"]["data"]["reason"],
        "capabilityNotNegotiated"
    );
    shutdown_lsp(&mut incompatible, 503).await;

    let other_project = TempDir::new().expect("cross-scope project");
    initialize_project(fixture.home(), other_project.path());
    let other_root_uri = url::Url::from_directory_path(other_project.path())
        .expect("cross-scope project root URI")
        .to_string();
    let other_handshake = DaemonHandshake::for_current_client(
        Some(other_project.path().to_path_buf()),
        None,
        false,
        false,
    )
    .expect("cross-scope daemon handshake");
    let other_client =
        DaemonInvocationClient::for_current(other_handshake).expect("cross-scope daemon client");
    let mut cross_scope = DaemonLspSessionClient::open(
        other_client,
        "3.17",
        Some(other_root_uri.clone()),
        Vec::new(),
    )
    .await
    .expect("open cross-scope daemon LSP session");
    send_lsp(
        &mut cross_scope,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 601,
            "method": "initialize",
            "params": {
                "rootUri": other_root_uri,
                "capabilities": {
                    "general": { "positionEncodings": ["utf-16"] },
                    "experimental": {
                        "tracedecay": {
                            "revision": TRACEDECAY_CONTEXT_REVISION,
                            "opaqueExpansion": true,
                            "projections": [{
                                "kind": "diagnostics",
                                "revision": TRACEDECAY_CONTEXT_REVISION,
                            }],
                        }
                    }
                }
            }
        }),
    )
    .await;
    let cross_scope_initialize = poll_lsp_response(&mut cross_scope, 601).await;
    assert_eq!(
        cross_scope_initialize["result"]["capabilities"]["experimental"]["tracedecay"]["revision"],
        TRACEDECAY_CONTEXT_REVISION
    );
    send_lsp(
        &mut cross_scope,
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "initialized",
            "params": {},
        }),
    )
    .await;
    if let Some(lsp_handle) = projection["result"]["retrievalHandle"].as_str() {
        send_lsp(
            &mut cross_scope,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 602,
                "method": "tracedecay/context/expand",
                "params": { "retrievalHandle": lsp_handle },
            }),
        )
        .await;
        let cross_scope_expansion = poll_lsp_response(&mut cross_scope, 602).await;
        assert_eq!(cross_scope_expansion["error"]["code"], -32601);
        assert!(
            cross_scope_expansion.get("result").is_none(),
            "cross-project handles must not reveal evidence"
        );
    }
    shutdown_lsp(&mut cross_scope, 603).await;

    session
        .reconnect()
        .await
        .expect("reconnect production LSP session");
    send_lsp(
        &mut session,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 701,
            "method": "textDocument/documentSymbol",
            "params": { "textDocument": { "uri": document_uri } },
        }),
    )
    .await;
    let after_reconnect = poll_lsp_response(&mut session, 701).await;
    assert!(
        after_reconnect.get("result").is_some(),
        "reconnected client must retain standard navigation: {after_reconnect}"
    );
    shutdown_lsp(&mut session, 702).await;
}

#[tokio::test]
async fn feedback_handle_bootstrap_reads() {
    let project = TempDir::new().expect("temporary feedback project");
    let (database, _) = common::initialize_test_database(&project.path().join("graph.db"))
        .await
        .expect("feedback database");
    let scope = resolved_scope("feedback");
    let observed_at = wall_clock_micros();
    let access = feedback_access(&scope, observed_at);
    let runtime = open_pr12_feedback_runtime(database, project.path(), scope, access)
        .await
        .expect("feedback runtime");
    let owner = runtime.owner();

    let list_handle = runtime
        .mint_list("request.feedback-bootstrap.list", None, 1, observed_at)
        .expect("list handle");
    assert!(list_handle.starts_with("rh_"));
    let listed = owner
        .invoke(FeedbackReadOperationV1::List, &list_handle, observed_at)
        .await
        .expect("list owner invocation");
    let FeedbackReadInvocationResultV1::List(Ok(listed)) = listed else {
        panic!("bootstrap list did not return canonical evidence");
    };
    let ApplicationOutcome::Evidence(listed) = listed.outcome else {
        panic!("bootstrap list did not return evidence");
    };
    assert_eq!(
        listed.execution.termination,
        OperationTermination::Completed
    );
    assert!(listed.payload.expect("list payload").findings.is_empty());
    assert!(listed.page.cursor.is_none());

    let diagnostics_handle = runtime
        .mint_diagnostics(
            "request.feedback-bootstrap.diagnostics",
            FeedbackDiagnosticsReadRequestV1 {
                head_commit_id: CommitId::new("commit.feedback-bootstrap").expect("commit id"),
            },
            observed_at,
        )
        .expect("diagnostics handle");
    let diagnostics = owner
        .invoke(
            FeedbackReadOperationV1::Diagnostics,
            &diagnostics_handle,
            observed_at,
        )
        .await
        .expect("diagnostics owner invocation");
    let FeedbackReadInvocationResultV1::Diagnostics(Ok(diagnostics)) = diagnostics else {
        panic!("empty bootstrap diagnostics must return terminal evidence");
    };
    let ApplicationOutcome::Evidence(diagnostics) = diagnostics.outcome else {
        panic!("empty bootstrap diagnostics must preserve evidence");
    };
    assert_eq!(
        diagnostics.execution.termination,
        OperationTermination::Failed
    );
    assert!(diagnostics.payload.is_none());
    assert_eq!(
        diagnostics.coverage.completeness,
        CoverageCompleteness::Unknown
    );

    let concealed = owner
        .invoke(
            FeedbackReadOperationV1::List,
            "rh_missing-feedback-bootstrap",
            observed_at,
        )
        .await;
    assert!(matches!(
        concealed,
        Err(FeedbackReadOwnerErrorV1::NotFoundOrNotAuthorized)
    ));
}

fn assert_exact_markdown_field(markdown: &str, label: &str, expected: &str) {
    let expected = format!("- {label}: `{expected}`");
    assert!(
        markdown.lines().any(|line| line == expected),
        "missing exact Markdown field {expected:?}\n{markdown}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn primitive_config_markdown_json_parity() {
    let fixture = runtime_fixture().await;
    let request_id = RequestId::new("request.primitive-config-parity").expect("shared request id");

    let markdown_result = resolve_mcp_application_surface(
        ApplicationSurfaceOperation::StorageStatus,
        request_id.clone(),
        storage_status_request(),
        RequestedOutputFormat::Markdown,
        Some(&fixture.client),
    )
    .await
    .expect("MCP markdown invocation");
    let json_result = resolve_http_application_surface(
        ApplicationSurfaceOperation::StorageStatus,
        request_id,
        storage_status_request(),
        RequestedOutputFormat::Json,
        Some(&fixture.client),
    )
    .await
    .expect("HTTP JSON invocation");

    assert_eq!(
        markdown_result.requested_format,
        RequestedOutputFormat::Markdown
    );
    assert_eq!(json_result.requested_format, RequestedOutputFormat::Json);
    assert_eq!(
        successful_application(&markdown_result),
        successful_application(&json_result)
    );
    assert_eq!(
        markdown_result
            .result
            .as_ref()
            .expect("markdown result")
            .contract,
        json_result.result.as_ref().expect("JSON result").contract
    );

    // Markdown/JSON parity is renderer equivalence over one result. Two CLI
    // processes cannot express it: each performs its own authorized read of a
    // live store, so the comparison would only ever assert that two reads
    // agree, which is a different and weaker claim. Both renderings below come
    // from a single CLI-bound invocation through the same two functions
    // `tracedecay tool` calls, so only the process and stdout plumbing around
    // them is outside this test.
    let cli_request_id =
        RequestId::new("request.primitive-config-parity.cli").expect("CLI request id");
    let cli_deadline = Deadline::new(UtcMicros(wall_clock_micros().0.saturating_add(60_000_000)))
        .expect("CLI deadline");
    let cli_dispatch = resolve_application_surface_dispatch_with_controls(
        BindingSurface::Cli,
        ApplicationSurfaceOperation::StorageStatus,
        cli_request_id,
        storage_status_request(),
        PageRequest::first(10).expect("page"),
        Some(cli_deadline),
        CancellationSignal::active("cancel.primitive-config-parity.cli").expect("cancellation"),
        RequestedOutputFormat::Markdown,
    )
    .expect("CLI surface dispatch");
    let cli_result = execute_application_surface(
        ApplicationSurfaceOperation::StorageStatus,
        cli_dispatch,
        Some(&fixture.client),
    )
    .await
    .expect("CLI application invocation");

    let markdown = render_markdown(
        CanonicalHumanView::from_application_result(
            cli_result.operation.as_str(),
            &cli_result.binding_id,
            &cli_result.result,
        )
        .expect("canonical human view"),
    )
    .as_str()
    .to_owned();
    let json: Value = serde_json::from_str(
        &canonical_json_line(&cli_result.result).expect("canonical JSON line"),
    )
    .expect("CLI JSON");

    assert_eq!(
        markdown.lines().next(),
        Some("## storage\\_status"),
        "the operation heading uses the canonical Markdown escaping contract"
    );
    assert_exact_markdown_field(&markdown, "Operation", "storage_status");
    assert_exact_markdown_field(&markdown, "Binding", "binding.cli.storage_status.v1");
    assert_exact_markdown_field(
        &markdown,
        "Contract",
        "schema.application.primitive.storage-status.result@1",
    );
    assert_exact_markdown_field(&markdown, "Status", "success");
    assert_exact_markdown_field(&markdown, "Outcome", "evidence");
    assert_exact_markdown_field(
        &markdown,
        "Scope project",
        json["scope"]["project_id"]
            .as_str()
            .expect("JSON project scope"),
    );
    assert_exact_markdown_field(
        &markdown,
        "Scope repository",
        json["scope"]["repository_id"]
            .as_str()
            .expect("JSON repository scope"),
    );
    assert_exact_markdown_field(
        &markdown,
        "Scope worktree",
        json["scope"]["worktree_id"]
            .as_str()
            .expect("JSON worktree scope"),
    );
    assert_exact_markdown_field(
        &markdown,
        "Scope reference",
        json["scope"]["reference"].as_str().unwrap_or("none"),
    );
    assert_exact_markdown_field(
        &markdown,
        "Scope digest",
        json["scope"]["scope_digest"]
            .as_str()
            .expect("JSON scope digest"),
    );
    assert_exact_markdown_field(
        &markdown,
        "Freshness",
        json["outcome"]["value"]["temporal"]["freshness"]
            .as_str()
            .expect("JSON freshness"),
    );
    assert_exact_markdown_field(
        &markdown,
        "Coverage",
        json["outcome"]["value"]["coverage"]["completeness"]
            .as_str()
            .expect("JSON coverage"),
    );
    assert_exact_markdown_field(
        &markdown,
        "Page returned",
        &json["outcome"]["value"]["page"]["returned"].to_string(),
    );
    let page_total = json["outcome"]["value"]["page"]["total"]
        .as_u64()
        .map_or_else(|| "unknown".to_owned(), |total| total.to_string());
    assert_exact_markdown_field(&markdown, "Page total", &page_total);
    let cursor = json["outcome"]["value"]["page"]["cursor"]
        .as_str()
        .unwrap_or("none");
    assert_exact_markdown_field(&markdown, "Cursor", cursor);
    assert_exact_markdown_field(&markdown, "Termination", "completed");
    assert_exact_markdown_field(&markdown, "Cancellation stage", "none");

    // The key list is derived from the payload rather than pinned to a literal,
    // so it tracks the contract instead of going stale the next time the
    // payload gains a field. The human view lists the first eight sorted keys
    // and elides the rest behind its `--json` pointer, so the expectation
    // reproduces that rule instead of assuming the payload stays small.
    const HUMAN_VIEW_VISIBLE_KEYS: usize = 8;
    let payload = &json["outcome"]["value"]["payload"];
    let payload_fields = payload
        .as_object()
        .expect("storage_status payload is an object");
    assert!(
        !payload_fields.is_empty(),
        "an empty payload would make the key parity assertion vacuous"
    );
    let payload_bytes = serde_json::to_vec(payload)
        .expect("serialize JSON payload")
        .len();
    let mut payload_keys = payload_fields.keys().cloned().collect::<Vec<_>>();
    payload_keys.sort_unstable();
    let visible_keys = &payload_keys[..payload_keys.len().min(HUMAN_VIEW_VISIBLE_KEYS)];
    let elision = if payload_keys.len() > visible_keys.len() {
        ", …"
    } else {
        ""
    };
    let rendered_keys = visible_keys.join(",").replace('_', "\\_");
    let payload_summary = format!(
        "- Payload: object(keys={rendered_keys}{elision}; json\\_bytes={payload_bytes}); complete: --json"
    );
    assert!(
        markdown.lines().any(|line| line == payload_summary),
        "missing exact Markdown payload summary {payload_summary:?}\n{markdown}"
    );
    assert_eq!(json["outcome"]["outcome"], "evidence");
    assert_eq!(
        &json["outcome"]["value"]["payload"],
        successful_application(&cli_result),
        "the JSON renderer must emit its own invocation's payload verbatim"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn pre_cancelled_application_has_cli_mcp_http_parity() {
    let fixture = runtime_fixture().await;
    let operation = ApplicationSurfaceOperation::StorageStatus;
    let observed_at = wall_clock_micros();
    let deadline =
        Deadline::new(UtcMicros(observed_at.0.saturating_add(60_000_000))).expect("deadline");
    let mut canonical_problem = None;

    for (surface, surface_name) in [
        (BindingSurface::Cli, "cli"),
        (BindingSurface::Mcp, "mcp"),
        (BindingSurface::Http, "http"),
    ] {
        let request_id = RequestId::new(format!("request.pre-cancelled-parity.{surface_name}"))
            .expect("request id");
        let cancellation =
            CancellationSignal::active(format!("cancel.pre-cancelled-parity.{surface_name}"))
                .expect("cancellation");
        assert!(cancellation.cancel(observed_at));
        let dispatched = resolve_application_surface_dispatch_with_controls(
            surface,
            operation,
            request_id,
            storage_status_request(),
            PageRequest::first(10).expect("page"),
            Some(deadline.clone()),
            cancellation,
            RequestedOutputFormat::Json,
        )
        .expect("pre-cancelled surface dispatch");
        let result = execute_application_surface(operation, dispatched, Some(&fixture.client))
            .await
            .expect("pre-cancelled application invocation");

        assert_eq!(result.operation, operation);
        assert_eq!(
            result.binding_id.as_str(),
            format!("binding.{surface_name}.storage_status.v1")
        );
        let problem = result
            .result
            .expect_err("pre-cancelled request must not be admitted");
        assert_eq!(
            problem.contract.schema_id().as_str(),
            "schema.application.primitive.storage-status.result"
        );
        assert_eq!(problem.contract.schema_revision(), 1);
        assert_eq!(problem.problem.kind(), ApplicationProblemKind::Cancelled);
        assert_eq!(
            problem.problem.cancellation_stage,
            Some(CancellationStage::BeforeAdmission)
        );
        assert!(problem.problem.is_pre_admission());

        let mut normalized = serde_json::to_value(problem).expect("application problem envelope");
        normalize_application_envelope(&mut normalized);
        if let Some(expected) = &canonical_problem {
            assert_eq!(&normalized, expected);
        } else {
            canonical_problem = Some(normalized);
        }
    }
}

fn parse_sse_frames(body: &str) -> Vec<(String, Option<u64>, Value)> {
    body.split("\n\n")
        .filter_map(|frame| {
            let mut event = None;
            let mut id = None;
            let mut data = None;
            for line in frame.lines() {
                if let Some(value) = line.strip_prefix("event: ") {
                    event = Some(value.to_owned());
                } else if let Some(value) = line.strip_prefix("id: ") {
                    id = Some(value.parse().expect("numeric SSE event id"));
                } else if let Some(value) = line.strip_prefix("data: ") {
                    data = Some(serde_json::from_str(value).expect("SSE JSON data"));
                }
            }
            event.map(|event| (event, id, data.expect("SSE event data")))
        })
        .collect()
}

#[tokio::test]
async fn cancellation_capacity_resume() {
    let authority = OperationEventAuthority::new(OperationStreamConfig {
        retained_event_capacity: 2,
        max_operations: 1,
        max_subscribers_per_operation: 1,
    })
    .expect("bounded operation authority");
    let context = operation_context("primary");
    let operation_id = OperationId::from_request(context.request_id().clone());
    let emitter = authority
        .begin(&context, OperationKind::GitPreview, UtcMicros(100))
        .await
        .expect("begin primary operation");

    let other_context = operation_context("other");
    let operation_saturation = authority
        .begin(&other_context, OperationKind::GitPreview, UtcMicros(100))
        .await;
    assert_eq!(
        operation_saturation.err(),
        Some(OperationEventError::Saturated),
        "live operation must retain the sole capacity slot"
    );

    let initial = authority
        .subscribe(&operation_id, &context, UtcMicros(101), 0, None)
        .await
        .expect("initial subscription");
    let resume_token = initial
        .frontier()
        .resume_token
        .clone()
        .expect("real resume token");
    let subscriber_saturation = authority
        .subscribe(&operation_id, &context, UtcMicros(101), 0, None)
        .await;
    assert_eq!(
        subscriber_saturation.err(),
        Some(OperationEventError::Saturated),
        "second subscriber must hit bounded capacity"
    );
    assert_eq!(
        authority
            .cancel(&operation_id, &context, UtcMicros(102))
            .await
            .expect("cancel operation"),
        OperationCancelOutcome::Requested
    );
    assert!(emitter.is_cancelled());
    drop(initial);

    for completed in 1..=3 {
        emitter
            .progress(completed, Some(3))
            .await
            .expect("publish progress");
    }
    emitter
        .terminal(cancelled_receipt(&context))
        .await
        .expect("publish cancelled terminal");

    let resumed = authority
        .subscribe(
            &operation_id,
            &context,
            UtcMicros(106),
            1,
            Some(&resume_token),
        )
        .await
        .expect("resume retained stream");
    let (correlation_id, frontier, stream) = resumed.into_sse_parts();
    let response = sse_response(correlation_id, frontier, stream).into_response();
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("SSE body")
            .to_vec(),
    )
    .expect("UTF-8 SSE");
    let frames = parse_sse_frames(&body);
    assert_eq!(
        frames
            .iter()
            .map(|(event, _, _)| event.as_str())
            .collect::<Vec<_>>(),
        ["open", "resume_gap", "progress", "cancelled"]
    );
    assert_eq!(
        frames.iter().map(|(_, id, _)| *id).collect::<Vec<_>>(),
        [None, Some(1), Some(3), Some(4)]
    );
    assert_eq!(frames[0].2["event"], "open");
    assert_eq!(
        frames[0].2["data"]["correlation_id"],
        context.request_id().as_str()
    );
    assert_eq!(frames[0].2["data"]["frontier"]["next_sequence"], 5);
    assert_eq!(frames[0].2["data"]["frontier"]["retained_from_sequence"], 3);
    assert_eq!(
        frames[0].2["data"]["frontier"]["resume_token"],
        resume_token.as_str()
    );
    assert_eq!(frames[1].2["event"], "resume_gap");
    assert_eq!(frames[1].2["data"]["sequence"], 1);
    assert_eq!(frames[1].2["data"]["gap"]["first_missing_sequence"], 1);
    assert_eq!(frames[1].2["data"]["gap"]["last_missing_sequence"], 2);
    assert_eq!(
        frames[1].2["data"]["gap"]["frontier"],
        frames[0].2["data"]["frontier"]
    );
    assert_eq!(frames[2].2["event"], "progress");
    assert_eq!(frames[2].2["data"]["sequence"], 3);
    assert_eq!(frames[2].2["data"]["completed"], 3);
    assert_eq!(frames[2].2["data"]["total"], 3);
    assert_eq!(frames[3].2["event"], "cancelled");
    assert_eq!(frames[3].2["data"]["sequence"], 4);
    assert_eq!(frames[3].2["data"]["terminal"]["termination"], "cancelled");
    assert_eq!(
        frames[3].2["data"]["terminal"]["receipt"]["termination"],
        "cancelled"
    );
    assert_eq!(
        frames[3].2["data"]["terminal"]["receipt"]["cancellation"]["stage"],
        "during_read"
    );

    authority
        .begin(&other_context, OperationKind::GitPreview, UtcMicros(107))
        .await
        .expect("terminal operation frees bounded operation capacity");
    let expired = authority
        .subscribe(
            &operation_id,
            &context,
            UtcMicros(108),
            1,
            Some(&resume_token),
        )
        .await;
    assert_eq!(
        expired.err(),
        Some(OperationEventError::ResumeExpired),
        "evicted resume token must expire"
    );
}

fn resolved_scope(suffix: &str) -> ResolvedScope {
    ResolvedScope::new(
        ProjectId::new(format!("project.runtime-acceptance.{suffix}")).expect("project id"),
        RepositoryId::new(format!("repository.runtime-acceptance.{suffix}"))
            .expect("repository id"),
        WorktreeId::new(format!("worktree.runtime-acceptance.{suffix}")).expect("worktree id"),
        Some(RefId::new(format!("refs/heads/runtime-acceptance-{suffix}")).expect("reference id")),
    )
    .expect("resolved scope")
}

fn feedback_access(scope: &ResolvedScope, observed_at: UtcMicros) -> ProjectSourceAccessSnapshot {
    ProjectSourceAccessSnapshot {
        scope: scope.clone(),
        requester: ActorId::new("actor.runtime-acceptance.feedback").expect("requester"),
        binding: ScopeSourceBinding::new(
            SourceBindingId::new("binding.runtime-acceptance.feedback").expect("binding id"),
            SourceKindV1::Cursor,
            LocatorDigest::new(format!("sha256:{}", "1".repeat(64))).expect("locator digest"),
            AuthorityRef::Project(scope.project_id.clone()),
        )
        .expect("source binding"),
        configuration_revision: ConfigurationRevisionId::new(
            "configuration.runtime-acceptance.feedback",
        )
        .expect("configuration revision"),
        configuration_digest: digest('2'),
        configuration_provenance_digest: digest('3'),
        effective_capabilities: BTreeSet::from([
            CapabilityId::new(FEEDBACK_DIAGNOSTICS_CAPABILITY_ID_V1)
                .expect("diagnostics capability"),
            CapabilityId::new(FEEDBACK_LIST_CAPABILITY_ID_V1).expect("list capability"),
        ]),
        // Wall-clock aligned so port interruption checks using now_micros admit.
        grant_expires_at: UtcMicros(observed_at.0.saturating_add(60_000_000)),
    }
}

fn wall_clock_micros() -> UtcMicros {
    UtcMicros(
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_micros())
                .unwrap_or(0),
        )
        .unwrap_or(i64::MAX),
    )
}

fn operation_context(suffix: &str) -> RequestContext {
    let scope = resolved_scope(suffix);
    let capability =
        CapabilityId::new("capability.runtime-acceptance.operation").expect("capability");
    let use_case = UseCaseId::new("use-case.runtime-acceptance.operation").expect("use case");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!("grant.runtime-acceptance.{suffix}")).expect("grant id"),
        1,
        digest('4'),
        ActorId::new("actor.runtime-acceptance.issuer").expect("issuer"),
        UtcMicros(1),
        UtcMicros(1_000),
        scope.clone(),
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        DisclosureClass::Metadata,
    )
    .expect("capability grant");
    RequestContext::new(
        ActorId::new("actor.runtime-acceptance.requester").expect("requester"),
        scope,
        grant,
        RequestId::new(format!("request.runtime-acceptance.{suffix}")).expect("request id"),
        Deadline::new(UtcMicros(1_000)).expect("deadline"),
        CancellationContext::active(format!("cancel.runtime-acceptance.{suffix}"))
            .expect("cancellation"),
    )
    .expect("request context")
}

fn cancelled_receipt(context: &RequestContext) -> OperationReceipt {
    let receipt = OperationReceipt {
        started_at: UtcMicros(100),
        ended_at: UtcMicros(105),
        effective_deadline: context.deadline().clone(),
        cancellation: Some(CancellationObservation {
            stage: CancellationStage::DuringRead,
            observed_at: UtcMicros(102),
        }),
        budget: OperationBudgetUsage::default(),
        termination: OperationTermination::Cancelled,
    };
    receipt.validate().expect("cancelled receipt");
    receipt
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("manifest digest")
}
