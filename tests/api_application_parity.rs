mod common;

use std::collections::BTreeSet;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use tracedecay::application_surface::{
    APPLICATION_SURFACE_OPERATIONS, AffectedTestsSurfaceRequest, ApplicationSurfaceOperation,
    ApplicationSurfaceRequest, FeedbackImpactSurfaceRequest, FeedbackSurfaceRequest,
    GitApplySurfaceRequest, GitPreviewSurfaceRequest, GitReadSurfaceRequest,
    TestResultsSurfaceRequest, resolve_application_surface_dispatch,
    resolve_http_application_surface, resolve_http_application_surface_dispatch,
};
use tracedecay::daemon::DaemonHandshake;
use tracedecay::daemon_client::{
    BindingResolution, BindingResolver, CatalogBindingResolver, DaemonInvocationClient,
    RequestedOutputFormat,
};
use tracedecay::mcp::tools::dispatch::{
    resolve_mcp_application_surface, resolve_mcp_application_surface_dispatch,
};
use tracedecay::mcp::tools::get_tool_definitions;
use tracedecay_api::{
    CanonicalInvocationResult, HttpApplicationControls, HttpApplicationRequest, HttpSseEvent,
    application_router,
};
use tracedecay_application::{
    APPLICATION_DEFAULT_PROFILE_ID, ApplicationProblem, ApplicationProblemEnvelope,
    ApplicationProblemKind, CancellationSignal, Deadline, IdempotencyKey, RequestId,
    ResultContractRef, RetryDirective, StreamEvent,
};
use tracedecay_domain::{
    GitCommitIdentityV1, GitCoverageV1, GitDiffScopeV1, GitHeadStateV1, GitIndexCommitIntentV1,
    GitIndexPreviewDispositionV1, GitIndexPreviewId, GitIndexPreviewV1, GitIndexSigningPolicyV1,
    GitIndexTransactionOperationV1, GitObjectFormatV1, GitOidV1, ManifestDigest, ProjectId,
    RepositoryId, RepositoryIndexSnapshotV1, RepositoryIndexStateV1, RepositoryStateSnapshotV1,
    RepositoryWorkingTreeSnapshotV1, RepositoryWorkingTreeStateV1, UtcMicros, WorktreeId,
};
use tracedecay_tool_catalog::{
    BindingId, BindingSurface, ProfileId, SchemaId, SurfaceOperationName,
};

const PARITY_FIXTURE: &str =
    include_str!("../benchmarks/pr12-transport-boundary/goldens/application-surface-parity.json");

#[tokio::test]
async fn http_routes_apply_the_canonical_page_default_when_query_is_omitted() {
    let observed = Arc::new(Mutex::new(None));
    let capture = Arc::clone(&observed);
    let owner = move |request: HttpApplicationRequest| {
        let capture = Arc::clone(&capture);
        async move {
            *capture.lock().expect("capture HTTP page") = Some(request.page);
            CanonicalInvocationResult::<serde_json::Value>::new(
                BindingId::new("binding.http.feedback_diagnostics.v1").expect("binding"),
                Err(ApplicationProblemEnvelope::new(
                    ResultContractRef::new(
                        SchemaId::new("schema.application.feedback.diagnostics.result")
                            .expect("schema"),
                        1,
                    )
                    .expect("result contract"),
                    request.request_id,
                    ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
                )),
            )
        }
    };
    let request = Request::builder()
        .method("POST")
        .uri("/feedback/diagnostics")
        .header("content-type", "application/json")
        .extension(RequestId::new("request.http-default-page").expect("request id"))
        .extension(HttpApplicationControls {
            deadline: Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
            cancellation: CancellationSignal::active("cancel.http-default-page")
                .expect("cancellation"),
        })
        .body(Body::from("{}"))
        .expect("HTTP request");

    let response = application_router(owner)
        .oneshot(request)
        .await
        .expect("HTTP response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let page = observed
        .lock()
        .expect("read captured HTTP page")
        .clone()
        .expect("owner invoked");
    assert_eq!(page.page_size, 10);
    assert!(page.cursor.is_none());
}

#[test]
fn cli_mcp_and_http_dispatch_the_same_callable_contracts() {
    let fixture: serde_json::Value =
        serde_json::from_str(PARITY_FIXTURE).expect("application parity fixture");
    let catalog = tracedecay::application_surface::application_surface_catalog()
        .expect("application catalog");
    let resolver = CatalogBindingResolver::new(&catalog);

    for operation in APPLICATION_SURFACE_OPERATIONS {
        let expected = &fixture["operations"][operation.as_str()];
        if !expected.is_object() {
            continue;
        }
        let resolution = BindingResolution {
            profile_id: ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).expect("profile"),
            operation: SurfaceOperationName::new(operation.as_str()).expect("operation"),
            protocol_revision: 1,
            negotiated_features: BTreeSet::new(),
        };
        let direct = [
            (
                "cli",
                resolve_application_surface_dispatch(
                    BindingSurface::Cli,
                    operation,
                    request_id(operation, "cli"),
                    application_request(operation, expected),
                    RequestedOutputFormat::Json,
                )
                .expect("CLI dispatch"),
            ),
            (
                "mcp",
                resolve_mcp_application_surface_dispatch(
                    operation,
                    request_id(operation, "mcp"),
                    application_request(operation, expected),
                    RequestedOutputFormat::Json,
                )
                .expect("MCP dispatch"),
            ),
            (
                "http",
                resolve_http_application_surface_dispatch(
                    operation,
                    request_id(operation, "http"),
                    application_request(operation, expected),
                    RequestedOutputFormat::Json,
                )
                .expect("HTTP dispatch"),
            ),
        ];
        for (surface, dispatched) in direct {
            assert_eq!(
                dispatched.invocation.binding_id.as_str(),
                expected["bindings"][surface].as_str().expect("binding id")
            );
            assert_eq!(
                dispatched.invocation.request_schema.schema_id().as_str(),
                expected["request_schema"].as_str().expect("request schema")
            );
            assert_eq!(
                dispatched.invocation.result_schema.schema_id().as_str(),
                expected["result_schema"].as_str().expect("result schema")
            );
        }

        for (surface, surface_name) in [
            (BindingSurface::Cli, "cli"),
            (BindingSurface::Mcp, "mcp"),
            (BindingSurface::Http, "http"),
        ] {
            let resolved = resolver
                .resolve_binding(surface, &resolution)
                .expect("binding");
            assert_eq!(
                resolved.binding_id.as_str(),
                expected["bindings"][surface_name]
                    .as_str()
                    .expect("fixture binding")
            );
        }
    }
}

#[test]
fn extended_primitive_reads_bind_cli_mcp_and_http() {
    let catalog = tracedecay::application_surface::application_surface_catalog()
        .expect("application catalog");
    let resolver = CatalogBindingResolver::new(&catalog);
    for operation in [
        ApplicationSurfaceOperation::CodeSymbolSearch,
        ApplicationSurfaceOperation::CodeSignatureSearch,
        ApplicationSurfaceOperation::CodeImplementations,
        ApplicationSurfaceOperation::CodeTypeHierarchy,
        ApplicationSurfaceOperation::CodeCallers,
        ApplicationSurfaceOperation::SessionLookup,
        ApplicationSurfaceOperation::QualifiedName,
        ApplicationSurfaceOperation::CallChain,
        ApplicationSurfaceOperation::FileDependents,
        ApplicationSurfaceOperation::SourceLines,
        ApplicationSurfaceOperation::SourceBody,
        ApplicationSurfaceOperation::SourceOutline,
        ApplicationSurfaceOperation::ModuleApi,
        ApplicationSurfaceOperation::FileMetadata,
        ApplicationSurfaceOperation::HealthRead,
        ApplicationSurfaceOperation::StorageStatus,
        ApplicationSurfaceOperation::DiagnosticsRead,
    ] {
        let resolution = BindingResolution {
            profile_id: ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).expect("profile"),
            operation: SurfaceOperationName::new(operation.as_str()).expect("operation"),
            protocol_revision: 1,
            negotiated_features: BTreeSet::new(),
        };
        for surface in [
            BindingSurface::Cli,
            BindingSurface::Mcp,
            BindingSurface::Http,
        ] {
            assert!(
                resolver.resolve_binding(surface, &resolution).is_some(),
                "{} must bind on {surface:?}",
                operation.as_str()
            );
        }
    }
}

#[test]
fn mcp_primitive_definitions_use_application_contracts() {
    let definitions = get_tool_definitions();
    for (operation, request, expected_properties, expected_required) in [
        (
            ApplicationSurfaceOperation::CallChain,
            serde_json::json!({
                "from_node_id": "node.from",
                "to_node_id": "node.to",
                "maximum_depth": 3
            }),
            &["from_node_id", "to_node_id", "maximum_depth"][..],
            &["from_node_id", "to_node_id"][..],
        ),
        (
            ApplicationSurfaceOperation::FileDependents,
            serde_json::json!({"file": "src/lib.rs"}),
            &["file"][..],
            &["file"][..],
        ),
        (
            ApplicationSurfaceOperation::ModuleApi,
            serde_json::json!({"path": "src"}),
            &["path"][..],
            &["path"][..],
        ),
        (
            ApplicationSurfaceOperation::StorageStatus,
            serde_json::json!({"include_details": true}),
            &["include_details"][..],
            &[][..],
        ),
    ] {
        let tool_name = format!("tracedecay_{}", operation.as_str());
        let definition = definitions
            .iter()
            .find(|definition| definition.name == tool_name)
            .unwrap_or_else(|| panic!("{tool_name} definition"));
        assert!(
            definition
                .description
                .contains("daemon-retained typed primitive owner"),
            "{tool_name} must advertise its application-owned primitive contract"
        );

        let properties = definition.input_schema["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{tool_name} properties"));
        for property in expected_properties {
            assert!(
                properties.contains_key(*property),
                "{tool_name} must declare {property}"
            );
        }
        let required = definition.input_schema["required"]
            .as_array()
            .unwrap_or_else(|| panic!("{tool_name} required properties"))
            .iter()
            .map(|property| property.as_str().expect("required property name"))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            required,
            expected_required.iter().copied().collect::<BTreeSet<_>>(),
            "{tool_name} required properties"
        );
        tracedecay::application_surface::parse_application_surface_request(operation, request)
            .unwrap_or_else(|error| panic!("{tool_name} must parse: {error}"));
    }
}

#[test]
fn mcp_feedback_cycle_projection_schemas_require_only_the_canonical_handle() {
    let definitions = get_tool_definitions();
    for operation in [
        ApplicationSurfaceOperation::FeedbackImpact,
        ApplicationSurfaceOperation::AffectedTests,
    ] {
        let tool_name = format!("tracedecay_{}", operation.as_str());
        let definition = definitions
            .iter()
            .find(|definition| definition.name == tool_name)
            .unwrap_or_else(|| panic!("{tool_name} definition"));
        assert_eq!(
            definition.input_schema["properties"]
                .as_object()
                .expect("feedback-cycle properties")
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["request_handle"])
        );
        assert_eq!(
            definition.input_schema["required"],
            serde_json::json!(["request_handle"])
        );
        tracedecay::application_surface::parse_application_surface_request(
            operation,
            serde_json::json!({"request_handle": "rh_feedback-cycle.fixture"}),
        )
        .unwrap_or_else(|error| panic!("{tool_name} must parse: {error}"));
    }
}

#[tokio::test]
async fn feedback_reads_are_callable_and_conceal_unknown_handles() {
    let (environment, project) = common::IsolatedEnv::acquire().await;
    std::fs::create_dir_all(project.join("src")).expect("project source directory");
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn transport_fixture() {}\n",
    )
    .expect("project source");
    let initialization = common::tracedecay_command_with_home(environment.home())
        .arg("init")
        .current_dir(&project)
        .stdin(Stdio::null())
        .output()
        .expect("initialize parity project");
    assert!(
        initialization.status.success(),
        "tracedecay init failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&initialization.stdout),
        String::from_utf8_lossy(&initialization.stderr)
    );
    let _daemon = common::spawn_tracedecay_daemon(environment.home());
    let handshake = DaemonHandshake::for_current_client(Some(project.clone()), None, false, false)
        .expect("daemon handshake");
    let client = DaemonInvocationClient::for_current(handshake).expect("daemon client");
    let project_arg = project.to_string_lossy().into_owned();
    let fixture: serde_json::Value =
        serde_json::from_str(PARITY_FIXTURE).expect("application parity fixture");
    let expected_kind = fixture["authorization_concealment"]["problem_kind"]
        .as_str()
        .expect("concealed problem kind");
    wait_for_feedback_owner(&client).await;

    for operation in [
        ApplicationSurfaceOperation::FeedbackDiagnostics,
        ApplicationSurfaceOperation::FeedbackGet,
        ApplicationSurfaceOperation::FeedbackExpand,
        ApplicationSurfaceOperation::FeedbackList,
    ] {
        let expected = &fixture["operations"][operation.as_str()];
        let handle = expected["request_handle"].as_str().expect("request handle");
        let mcp = resolve_mcp_application_surface(
            operation,
            request_id(operation, "mcp-call"),
            feedback_request(handle),
            RequestedOutputFormat::Json,
            Some(&client),
        )
        .await
        .expect("MCP call");
        let http = resolve_http_application_surface(
            operation,
            request_id(operation, "http-call"),
            feedback_request(handle),
            RequestedOutputFormat::Json,
            Some(&client),
        )
        .await
        .expect("HTTP call");

        let mcp_problem = mcp
            .result
            .as_ref()
            .expect_err("unknown handle must be concealed");
        assert_eq!(
            mcp_problem.problem.kind(),
            ApplicationProblemKind::NotFoundOrNotAuthorized,
            "{mcp_problem:?}"
        );
        let http_problem = http
            .result
            .as_ref()
            .expect_err("unknown handle must be concealed");
        assert_eq!(
            http_problem.problem.kind(),
            ApplicationProblemKind::NotFoundOrNotAuthorized,
            "{http_problem:?}"
        );
        let http_value = serde_json::to_value(
            CanonicalInvocationResult::<serde_json::Value>::new(http.binding_id, http.result)
                .into_http_json(),
        )
        .expect("HTTP JSON");
        assert_eq!(http_value["kind"], "problem");
        assert_eq!(http_value["value"]["problem"]["kind"], expected_kind);
        assert!(
            http_value["value"].get("binding_id").is_none(),
            "concealed HTTP problems must not disclose binding identity"
        );

        let output = common::tracedecay_command_with_home(environment.home())
            .current_dir(&project)
            .args([
                "tool",
                "--project",
                project_arg.as_str(),
                operation.as_str(),
                "--request-handle",
                handle,
                "--json",
            ])
            .output()
            .expect("run CLI");
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("canonical CLI JSON");
        assert_eq!(value["problem"]["kind"], expected_kind);
    }
}

async fn wait_for_feedback_owner(client: &DaemonInvocationClient) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let result = resolve_mcp_application_surface(
            ApplicationSurfaceOperation::FeedbackDiagnostics,
            request_id(
                ApplicationSurfaceOperation::FeedbackDiagnostics,
                "owner-readiness",
            ),
            feedback_request("rh_missing-pr12-owner-readiness"),
            RequestedOutputFormat::Json,
            Some(client),
        )
        .await
        .expect("feedback owner readiness call");
        match result.result {
            Err(problem)
                if problem.problem.kind() == ApplicationProblemKind::NotFoundOrNotAuthorized =>
            {
                return;
            }
            Err(problem)
                if problem.problem.kind() == ApplicationProblemKind::Unavailable
                    && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            other => panic!("feedback owner did not become ready: {other:?}"),
        }
    }
}

#[test]
fn sse_projects_the_same_canonical_feedback_payload() {
    let fixture: serde_json::Value =
        serde_json::from_str(PARITY_FIXTURE).expect("application parity fixture");
    let expected = &fixture["http_sse"];
    let sequence = expected["sequence"].as_u64().expect("SSE sequence");
    let payload = expected["item"].clone();
    let event = HttpSseEvent::from(StreamEvent::item(sequence, payload.clone()).expect("item"));
    let wire = serde_json::to_value(event).expect("serialize SSE event");

    assert_eq!(wire["event"], "item");
    assert_eq!(wire["data"]["sequence"], sequence);
    assert_eq!(wire["data"]["item"], payload);
    assert_eq!(
        expected["result_schema"],
        fixture["operations"]["feedback_list"]["result_schema"]
    );
    assert_eq!(
        expected["http_binding"],
        fixture["operations"]["feedback_list"]["bindings"]["http"]
    );
}

#[test]
fn http_concealment_omits_binding_identity() {
    let fixture: serde_json::Value =
        serde_json::from_str(PARITY_FIXTURE).expect("application parity fixture");
    let concealed = &fixture["authorization_concealment"];
    let result = Err(tracedecay_application::ApplicationProblemEnvelope::new(
        ResultContractRef::new(
            SchemaId::new(
                fixture["operations"]["feedback_get"]["result_schema"]
                    .as_str()
                    .expect("result schema"),
            )
            .expect("schema id"),
            1,
        )
        .expect("result contract"),
        RequestId::new("request.feedback-concealment").expect("request id"),
        tracedecay_application::ApplicationProblem::not_found_or_not_authorized(
            RetryDirective::Never,
        ),
    ));
    let value = serde_json::to_value(
        CanonicalInvocationResult::<serde_json::Value>::new(
            tracedecay_tool_catalog::BindingId::new(
                fixture["operations"]["feedback_get"]["bindings"]["http"]
                    .as_str()
                    .expect("HTTP binding"),
            )
            .expect("binding id"),
            result,
        )
        .into_http_json(),
    )
    .expect("HTTP JSON");

    assert_eq!(value["value"]["problem"]["kind"], concealed["problem_kind"]);
    assert!(value["value"].get("binding_id").is_none());
}

fn application_request(
    operation: ApplicationSurfaceOperation,
    expected: &serde_json::Value,
) -> ApplicationSurfaceRequest {
    match operation {
        ApplicationSurfaceOperation::GitStatus => {
            git_read_request(tracedecay::application::git_reads::GitReadRequestV1::Status)
        }
        ApplicationSurfaceOperation::GitDiff => {
            git_read_request(tracedecay::application::git_reads::GitReadRequestV1::Diff {
                scope: GitDiffScopeV1::WorkingTree,
            })
        }
        ApplicationSurfaceOperation::GitHistory => git_read_request(
            tracedecay::application::git_reads::GitReadRequestV1::History {
                max_count: 10,
                path: None,
                follow: false,
                first_parent: false,
            },
        ),
        ApplicationSurfaceOperation::GitBlame => git_read_request(
            tracedecay::application::git_reads::GitReadRequestV1::Blame {
                path: "src/lib.rs".to_owned(),
                follow_renames: false,
            },
        ),
        ApplicationSurfaceOperation::GitHunks => git_read_request(
            tracedecay::application::git_reads::GitReadRequestV1::Hunks {
                scope: GitDiffScopeV1::WorkingTree,
                preview_id: "preview.transport-parity".to_owned(),
                snapshot_digest: digest('a'),
            },
        ),
        ApplicationSurfaceOperation::GitPreview => git_requests().0,
        ApplicationSurfaceOperation::GitApply => git_requests().1,
        ApplicationSurfaceOperation::FeedbackImpact => {
            ApplicationSurfaceRequest::FeedbackImpact(FeedbackImpactSurfaceRequest {
                request_handle: "rh_missing-pr12-parity".to_owned(),
            })
        }
        ApplicationSurfaceOperation::AffectedTests => {
            ApplicationSurfaceRequest::AffectedTests(AffectedTestsSurfaceRequest {
                request_handle: "rh_missing-pr12-parity".to_owned(),
            })
        }
        ApplicationSurfaceOperation::TestResults => {
            ApplicationSurfaceRequest::TestResults(TestResultsSurfaceRequest::default())
        }
        _ => feedback_request(expected["request_handle"].as_str().expect("request handle")),
    }
}

fn git_read_request(
    request: tracedecay::application::git_reads::GitReadRequestV1,
) -> ApplicationSurfaceRequest {
    ApplicationSurfaceRequest::GitRead(GitReadSurfaceRequest {
        request,
        max_entries: 1_000,
        max_bytes: 4_194_304,
    })
}

fn feedback_request(request_handle: &str) -> ApplicationSurfaceRequest {
    ApplicationSurfaceRequest::Feedback(
        FeedbackSurfaceRequest::new(request_handle.to_owned()).expect("feedback request"),
    )
}

fn request_id(operation: ApplicationSurfaceOperation, surface: &str) -> RequestId {
    RequestId::new(format!("request.{surface}.{}", operation.as_str())).expect("request id")
}

fn git_requests() -> (ApplicationSurfaceRequest, ApplicationSurfaceRequest) {
    let snapshot = RepositoryStateSnapshotV1::new(
        id::<ProjectId>("project.pr12-parity"),
        id::<RepositoryId>("repository.pr12-parity"),
        Some(id::<WorktreeId>("worktree.pr12-parity")),
        1,
        GitObjectFormatV1::Sha1,
        GitHeadStateV1::Attached {
            branch: "refs/heads/main".to_owned(),
            commit: oid('a'),
        },
        RepositoryIndexSnapshotV1 {
            checksum: digest('b'),
            tree_id: Some(oid('c')),
            state: RepositoryIndexStateV1::Clean,
            unmerged_stage_digest: None,
        },
        RepositoryWorkingTreeSnapshotV1 {
            state: RepositoryWorkingTreeStateV1::Clean,
            tracked_digest: digest('d'),
            untracked_name_digest: None,
            ignored_collision_digest: None,
        },
        tracedecay_domain::GitOperationStateV1::None,
        Some(digest('1')),
        Some(digest('2')),
        Some(digest('3')),
        Some(digest('4')),
        UtcMicros(1),
        GitCoverageV1::complete(),
    )
    .expect("repository snapshot")
    .with_native_identity(
        "git version fixture".to_owned(),
        "tracedecay.git-index-adapter.v1".to_owned(),
        digest('5'),
    )
    .expect("native repository snapshot");
    let identity = GitCommitIdentityV1 {
        name: "PR12 Fixture".to_owned(),
        email: "pr12-fixture@example.com".to_owned(),
        at: UtcMicros(1_000_000),
    };
    let commit_intent = GitIndexCommitIntentV1::new(
        "PR12 transport parity\n".to_owned(),
        identity.clone(),
        identity,
        GitIndexSigningPolicyV1::UnsignedPermitted,
    )
    .expect("commit intent");
    let preview_id = GitIndexPreviewId::new("preview.pr12-parity").expect("preview id");
    let snapshot_digest =
        GitIndexPreviewV1::repository_snapshot_digest(&snapshot).expect("snapshot digest");
    let preview = GitIndexPreviewV1::new_with_commit_intent(
        preview_id.clone(),
        GitIndexTransactionOperationV1::CommitIndex,
        snapshot.clone(),
        snapshot_digest,
        Vec::new(),
        snapshot.index.tree_id.clone(),
        Some(&commit_intent),
        GitIndexPreviewDispositionV1::Applicable,
        UtcMicros(10),
        UtcMicros(100),
    )
    .expect("immutable preview");

    (
        ApplicationSurfaceRequest::GitPreview(GitPreviewSurfaceRequest {
            operation: GitIndexTransactionOperationV1::CommitIndex,
            preview_id,
            repository_snapshot: snapshot,
            selected_hunks: Vec::new(),
            commit_intent: Some(commit_intent),
        }),
        ApplicationSurfaceRequest::GitApply(GitApplySurfaceRequest {
            preview,
            idempotency_key: IdempotencyKey::new("idempotency.pr12-parity")
                .expect("idempotency key"),
        }),
    )
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("fixture identity")
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("fixture digest")
}

fn oid(byte: char) -> GitOidV1 {
    GitOidV1::new(byte.to_string().repeat(40)).expect("fixture object id")
}
