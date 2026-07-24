mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use axum::body::to_bytes;
use axum::response::IntoResponse;
use serde_json::Value;
use tempfile::TempDir;
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
use tracedecay::application_surface::{
    ApplicationSurfaceInvocationResult, ApplicationSurfaceOperation, ApplicationSurfaceRequest,
    FeedbackSurfaceRequest, resolve_http_application_surface,
};
use tracedecay::daemon::DaemonHandshake;
use tracedecay::daemon::lsp_gateway::TRACEDECAY_CONTEXT_REVISION;
use tracedecay::daemon_client::{
    DaemonInvocationClient, DaemonLspSessionClient, RequestedOutputFormat,
};
use tracedecay::lsp_bridge::{FramePoll, FrameSend};
use tracedecay::mcp::response_handles::{ResponseHandleLookup, retrieve_response_handle};
use tracedecay::mcp::tools::dispatch::resolve_mcp_application_surface;
use tracedecay_api::sse_response;
use tracedecay_application::feedback::{
    FEEDBACK_DIAGNOSTICS_CAPABILITY_ID_V1, FEEDBACK_LIST_CAPABILITY_ID_V1,
    FeedbackDiagnosticsReadRequestV1,
};
use tracedecay_application::{
    ApplicationOutcome, ApplicationProblemKind, CancellationContext, CancellationObservation,
    CancellationStage, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    OperationBudgetUsage, OperationReceipt, OperationTermination, RequestContext, RequestId,
    ResolvedScope,
};
use tracedecay_domain::configuration::{
    AuthorityRef, ConfigurationRevisionId, ScopeSourceBinding, SourceBindingId, SourceKindV1,
};
use tracedecay_domain::{
    ActorId, CommitId, LocatorDigest, ManifestDigest, ProjectId, RefId, RepositoryId, UtcMicros,
    WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

struct RuntimeFixture {
    _daemon: common::DaemonProcess,
    client: DaemonInvocationClient,
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
    initialize_project(environment.home(), &project);
    let daemon = common::spawn_tracedecay_daemon(environment.home());
    let handshake = DaemonHandshake::for_current_client(Some(project.clone()), None, false, false)
        .expect("daemon handshake");
    let client = DaemonInvocationClient::for_current(handshake).expect("daemon client");
    RuntimeFixture {
        _daemon: daemon,
        client,
        project,
        _environment: environment,
    }
}

async fn poll_lsp_response(session: &mut DaemonLspSessionClient, response_id: u64) -> Value {
    for _ in 0..200 {
        match session
            .poll_daemon_frame()
            .await
            .expect("poll daemon LSP frame")
        {
            FramePoll::Frame(frame) => {
                let value: Value =
                    serde_json::from_slice(frame.as_bytes()).expect("daemon LSP JSON");
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
    panic!("daemon LSP response {response_id} timed out")
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

fn initialize_project(home: &Path, project: &Path) {
    std::fs::create_dir_all(project.join("src")).expect("project source directory");
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn runtime_acceptance() -> &'static str { \"ready\" }\n",
    )
    .expect("project source");
    let output = common::tracedecay_command_with_home(home)
        .arg("init")
        .current_dir(project)
        .stdin(Stdio::null())
        .output()
        .expect("run tracedecay init");
    assert_command_success("tracedecay init", &output);
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

fn feedback_diagnostics_request(request_handle: &str) -> ApplicationSurfaceRequest {
    ApplicationSurfaceRequest::Feedback(
        FeedbackSurfaceRequest::new(request_handle.to_owned()).expect("feedback request"),
    )
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

#[tokio::test(flavor = "multi_thread")]
async fn project_open_application_boundary() {
    let fixture = runtime_fixture().await;

    let cli = run_storage_status(fixture.home(), &fixture.project, true);
    assert_command_success("CLI storage_status", &cli);
    let cli_value: Value = serde_json::from_slice(&cli.stdout).expect("CLI application JSON");
    assert_eq!(cli_value["outcome"], "evidence");
    assert!(cli_value["scope"]["project_id"].as_str().is_some());
    assert_eq!(cli_value["value"]["execution"]["termination"], "completed");

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
async fn production_lsp_negotiates_and_projects_canonical_context() {
    let fixture = runtime_fixture().await;
    let root_uri = url::Url::from_directory_path(&fixture.project)
        .expect("project root URI")
        .to_string();
    let document_uri = url::Url::from_file_path(fixture.project.join("src/lib.rs"))
        .expect("document URI")
        .to_string();
    let source =
        std::fs::read_to_string(fixture.project.join("src/lib.rs")).expect("project source");
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
        "post_edit_impact",
        "affected_tests",
        "test_run_results",
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
        initialized["result"]["capabilities"]["positionEncoding"],
        "utf-16"
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

    let mut ready = None;
    for request_id in 2..=100 {
        send_lsp(
            &mut session,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "method": "tracedecay/context",
                "params": {
                    "kind": "diagnostics",
                    "documentUri": document_uri,
                },
            }),
        )
        .await;
        let response = poll_lsp_response(&mut session, request_id).await;
        if response.get("result").is_some() {
            ready = Some(response);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let projection = ready.expect("production context projection became ready");
    assert_eq!(projection["result"]["rootUri"], root_uri);
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

    let lsp_handle = projection["result"]["retrievalHandle"]
        .as_str()
        .expect("LSP canonical projection retrieval handle");
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
    let http = resolve_http_application_surface(
        ApplicationSurfaceOperation::FeedbackDiagnostics,
        RequestId::new("request.feedback-parity.http").expect("request id"),
        feedback_diagnostics_request(canonical_handle),
        RequestedOutputFormat::Json,
        Some(&fixture.client),
    )
    .await
    .expect("HTTP feedback dispatch");
    let mcp_payload = successful_application(&mcp);
    let http_payload = successful_application(&http);
    assert_eq!(mcp_payload, http_payload);
    assert_eq!(cli["value"]["payload"], *mcp_payload);

    send_lsp(
        &mut session,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 101,
            "method": "tracedecay/context/expand",
            "params": { "retrievalHandle": lsp_handle },
        }),
    )
    .await;
    let expanded = poll_lsp_response(&mut session, 101).await;
    assert_eq!(expanded["result"]["coverage"], "complete");
    assert_eq!(
        expanded["result"]["evidence"]["Ok"]["outcome"]["value"]["payload"],
        *mcp_payload
    );
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
    let FeedbackReadInvocationResultV1::Diagnostics(Err(problem)) = diagnostics else {
        panic!("empty bootstrap diagnostics must return a structured problem");
    };
    assert_eq!(problem.problem.kind(), ApplicationProblemKind::Unavailable);
    assert_eq!(problem.problem.revision, 1);

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

    let markdown = run_storage_status(fixture.home(), &fixture.project, false);
    let json = run_storage_status(fixture.home(), &fixture.project, true);
    assert_command_success("CLI markdown storage_status", &markdown);
    assert_command_success("CLI JSON storage_status", &json);
    let markdown = String::from_utf8(markdown.stdout).expect("UTF-8 markdown");
    let json: Value = serde_json::from_slice(&json.stdout).expect("CLI JSON");

    assert!(markdown.contains("## storage_status"));
    assert!(markdown.contains("- Status: `success`"));
    assert!(markdown.contains("- Outcome: `evidence`"));
    assert!(markdown.contains("object(keys="));
    assert!(markdown.contains("read_only"));
    assert!(markdown.contains("status"));
    assert_eq!(json["outcome"], "evidence");
    assert_eq!(
        &json["value"]["payload"],
        successful_application(&json_result)
    );
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
    assert!(body.contains("event: open"));
    assert!(body.contains("event: resume_gap"));
    assert!(body.contains("\"first_missing_sequence\":1"));
    assert!(body.contains("\"last_missing_sequence\":2"));
    assert!(body.contains("event: cancelled"));
    assert!(body.contains("\"termination\":\"cancelled\""));

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
