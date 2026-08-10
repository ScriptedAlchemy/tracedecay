use std::collections::BTreeSet;

use axum::body::Body;
use axum::extract::Extension;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use tracedecay::application_surface::{
    APPLICATION_SURFACE_OPERATIONS, AffectedTestsSurfaceRequest, ApplicationSurfaceOperation,
    ApplicationSurfaceRequest, FeedbackImpactSurfaceRequest, FeedbackSurfaceRequest,
    GitApplySurfaceRequest, GitPreviewSurfaceRequest, GitReadSurfaceRequest,
    TestResultsSurfaceRequest, parse_application_surface_request,
    resolve_application_surface_dispatch, resolve_http_application_surface_dispatch,
};
use tracedecay::daemon_client::{
    BindingResolution, BindingResolver, CatalogBindingResolver, RequestedOutputFormat,
};
use tracedecay::mcp::tools::dispatch::resolve_mcp_application_surface_dispatch;
use tracedecay::mcp::tools::get_tool_definitions;
use tracedecay_api::{
    CanonicalInvocationResult, HttpApplicationControls, HttpApplicationOperation,
    HttpApplicationRequest, HttpSseEvent, application_router,
};
use tracedecay_application::{
    APPLICATION_DEFAULT_PROFILE_ID, ApplicationContractError, CancellationSignal, Deadline,
    IdempotencyKey, RequestId, ResultContractRef, RetryDirective, SafeDiagnostic, StreamEvent,
};
use tracedecay_domain::{
    GitCommitIdentityV1, GitCoverageV1, GitDiffScopeV1, GitHeadStateV1, GitIndexCommitIntentV1,
    GitIndexPreviewDispositionV1, GitIndexPreviewId, GitIndexPreviewV1, GitIndexSigningPolicyV1,
    GitIndexTransactionOperationV1, GitObjectFormatV1, GitOidV1, ManifestDigest, ProjectId,
    RepositoryId, RepositoryIndexSnapshotV1, RepositoryIndexStateV1, RepositoryStateSnapshotV1,
    RepositoryWorkingTreeSnapshotV1, RepositoryWorkingTreeStateV1, UtcMicros, WorktreeId,
};
use tracedecay_tool_catalog::{BindingSurface, ProfileId, SchemaId, SurfaceOperationName};

const PARITY_FIXTURE: &str =
    include_str!("../benchmarks/transport-boundary/goldens/application-surface-parity.json");

/// The operations whose decoded surface request carries a
/// `CallableCodeSurfaceMeta`, and therefore a `cursor` continuation that every
/// transport must accept identically. Mirrors
/// `operation_carries_callable_code_meta` in `src/application_surface.rs`; the
/// drift guard below fails as soon as one of them stops being pinned, stops
/// binding a surface, or stops advertising its cursor over MCP.
const CURSOR_CARRYING_CODE_OPERATIONS: [ApplicationSurfaceOperation; 14] = [
    ApplicationSurfaceOperation::CodeExactOccurrence,
    ApplicationSurfaceOperation::CodePhraseSearch,
    ApplicationSurfaceOperation::CodeSymbolSearch,
    ApplicationSurfaceOperation::CodeSignatureSearch,
    ApplicationSurfaceOperation::CodeImplementations,
    ApplicationSurfaceOperation::CodeTypeHierarchy,
    ApplicationSurfaceOperation::CodeCallers,
    ApplicationSurfaceOperation::CodeCallees,
    ApplicationSurfaceOperation::CodeFacets,
    ApplicationSurfaceOperation::CodeTimeline,
    ApplicationSurfaceOperation::CodeDeclaration,
    ApplicationSurfaceOperation::CodeDefinition,
    ApplicationSurfaceOperation::CodeTypeDefinition,
    ApplicationSurfaceOperation::CodeReferences,
];

fn parity_fixture() -> serde_json::Value {
    serde_json::from_str(PARITY_FIXTURE).expect("application parity fixture")
}

/// Operations the golden deliberately does not pin yet. Every entry must still
/// be a real catalog operation, and an operation that appears in neither roster
/// fails the parity test instead of being skipped.
fn unpinned_operations(fixture: &serde_json::Value) -> BTreeSet<String> {
    fixture["unpinned_operations"]
        .as_array()
        .expect("unpinned operation roster")
        .iter()
        .map(|operation| operation.as_str().expect("operation name").to_owned())
        .collect()
}

#[tokio::test]
async fn catalog_advertised_specialized_http_routes_invoke_the_application_owner() {
    let owner = |request: HttpApplicationRequest| async move {
        Ok::<_, ApplicationContractError>(CanonicalInvocationResult::<serde_json::Value>::new(
            tracedecay_tool_catalog::BindingId::new(format!(
                "binding.http.{}.v1",
                request.operation.as_str()
            ))
            .expect("binding id"),
            Err(tracedecay_application::ApplicationProblemEnvelope::new(
                ResultContractRef::new(
                    SchemaId::new("schema.test.feedback.result").expect("schema id"),
                    1,
                )
                .expect("result contract"),
                request.request_id,
                tracedecay_application::ApplicationProblem::unavailable(
                    SafeDiagnostic::new("feedback.test_unavailable", "Feedback is unavailable")
                        .expect("diagnostic"),
                ),
            )
            .expect("canonical feedback fixture problem")),
        ))
    };
    let router = application_router(owner)
        .layer(Extension(
            RequestId::new("request.feedback-http-parity").expect("request id"),
        ))
        .layer(Extension(HttpApplicationControls {
            deadline: Deadline::new(UtcMicros(10_000)).expect("deadline"),
            cancellation: CancellationSignal::active("cancel.feedback-http-parity")
                .expect("cancellation"),
        }));
    let catalog =
        tracedecay::application_surface::application_surface_catalog().expect("catalog snapshot");
    let resolver = CatalogBindingResolver::new(&catalog);

    for (route, operation) in [
        (
            "/feedback/diagnostics",
            HttpApplicationOperation::FeedbackDiagnostics,
        ),
        ("/feedback/get", HttpApplicationOperation::FeedbackGet),
        ("/feedback/expand", HttpApplicationOperation::FeedbackExpand),
        ("/feedback/list", HttpApplicationOperation::FeedbackList),
        ("/feedback/impact", HttpApplicationOperation::FeedbackImpact),
        (
            "/feedback/advisory_cycle",
            HttpApplicationOperation::FeedbackAdvisoryCycle,
        ),
    ] {
        let resolution = BindingResolution {
            profile_id: ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).expect("profile"),
            operation: SurfaceOperationName::new(operation.as_str()).expect("operation"),
            protocol_revision: 1,
            negotiated_features: BTreeSet::new(),
        };
        assert!(
            resolver
                .resolve_binding(BindingSurface::Http, &resolution)
                .is_some(),
            "{route} must remain catalog-advertised"
        );
        let request = Request::builder()
            .method("POST")
            .uri(route)
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .expect("HTTP request");
        let response = router
            .clone()
            .oneshot(request)
            .await
            .expect("HTTP response");
        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "{route} must preserve the application problem"
        );
    }

    for (route, operation) in [
        (
            "/github-stack/signal-expand",
            HttpApplicationOperation::GitHubStackSignalExpand,
        ),
        (
            "/native-integration/worktree_inventory",
            HttpApplicationOperation::NativeIntegrationWorktreeInventory,
        ),
        (
            "/native-integration/worktree_cleanup_inspect",
            HttpApplicationOperation::NativeIntegrationWorktreeInspect,
        ),
        (
            "/native-integration/worktree_cleanup_confirm",
            HttpApplicationOperation::NativeIntegrationWorktreeConfirm,
        ),
        (
            "/native-integration/worktree_cleanup_remove",
            HttpApplicationOperation::NativeIntegrationWorktreeRemove,
        ),
        (
            "/native-integration/worktree_cleanup_reconcile",
            HttpApplicationOperation::NativeIntegrationWorktreeReconcile,
        ),
    ] {
        let resolution = BindingResolution {
            profile_id: ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).expect("profile"),
            operation: SurfaceOperationName::new(operation.as_str()).expect("operation"),
            protocol_revision: 1,
            negotiated_features: BTreeSet::new(),
        };
        assert!(
            resolver
                .resolve_binding(BindingSurface::Http, &resolution)
                .is_some(),
            "{route} must remain catalog-advertised"
        );
        let request = Request::builder()
            .method("POST")
            .uri(route)
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .expect("HTTP request");
        let response = router
            .clone()
            .oneshot(request)
            .await
            .expect("HTTP response");
        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "{route} must preserve the application problem"
        );
    }
}

#[test]
fn cli_mcp_and_http_dispatch_the_same_callable_contracts() {
    let fixture = parity_fixture();
    let unpinned = unpinned_operations(&fixture);
    let catalog = tracedecay::application_surface::application_surface_catalog()
        .expect("application catalog");
    let resolver = CatalogBindingResolver::new(&catalog);

    for operation in APPLICATION_SURFACE_OPERATIONS {
        let expected = &fixture["operations"][operation.as_str()];
        if !expected.is_object() {
            assert!(
                unpinned.contains(operation.as_str()),
                "{} is advertised by the application catalog but the parity golden \
                 neither pins its contract nor lists it under unpinned_operations; \
                 pin the operation or declare it unpinned instead of leaving it \
                 silently unverified",
                operation.as_str()
            );
            continue;
        }
        let resolution = BindingResolution {
            profile_id: ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).expect("profile"),
            operation: SurfaceOperationName::new(operation.as_str()).expect("operation"),
            protocol_revision: 1,
            negotiated_features: BTreeSet::new(),
        };
        let mut direct = Vec::new();
        for surface in expected["bindings"]
            .as_object()
            .expect("surface bindings")
            .keys()
        {
            let dispatched = match surface.as_str() {
                "cli" => resolve_application_surface_dispatch(
                    BindingSurface::Cli,
                    operation,
                    request_id(operation, "cli"),
                    application_request(operation, expected),
                    RequestedOutputFormat::Json,
                )
                .expect("CLI dispatch"),
                "mcp" => resolve_mcp_application_surface_dispatch(
                    operation,
                    request_id(operation, "mcp"),
                    application_request(operation, expected),
                    RequestedOutputFormat::Json,
                )
                .expect("MCP dispatch"),
                "http" => resolve_http_application_surface_dispatch(
                    operation,
                    request_id(operation, "http"),
                    application_request(operation, expected),
                    RequestedOutputFormat::Json,
                )
                .expect("HTTP dispatch"),
                unexpected => panic!("unsupported fixture surface {unexpected}"),
            };
            direct.push((surface.as_str(), dispatched));
        }
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
            match expected["bindings"][surface_name].as_str() {
                Some(binding_id) => {
                    let resolved = resolver
                        .resolve_binding(surface, &resolution)
                        .expect("binding");
                    assert_eq!(resolved.binding_id.as_str(), binding_id);
                }
                None => assert!(
                    resolver.resolve_binding(surface, &resolution).is_none(),
                    "{} must not bind on {surface:?}",
                    operation.as_str()
                ),
            }
        }
    }
}

#[test]
fn the_parity_golden_accounts_for_every_catalog_operation() {
    let fixture = parity_fixture();
    let catalog: BTreeSet<&str> = APPLICATION_SURFACE_OPERATIONS
        .iter()
        .map(|operation| operation.as_str())
        .collect();

    let pinned: BTreeSet<String> = fixture["operations"]
        .as_object()
        .expect("pinned operation contracts")
        .keys()
        .cloned()
        .collect();
    let unpinned = unpinned_operations(&fixture);

    for operation in pinned.iter().chain(unpinned.iter()) {
        assert!(
            catalog.contains(operation.as_str()),
            "{operation} is named by the parity golden but is not an application \
             catalog operation; remove the stale roster entry"
        );
    }
    let overlap: Vec<&String> = pinned.intersection(&unpinned).collect();
    assert!(
        overlap.is_empty(),
        "operations cannot be both pinned and unpinned: {overlap:?}"
    );

    let accounted: BTreeSet<&str> = pinned
        .iter()
        .chain(unpinned.iter())
        .map(String::as_str)
        .collect();
    let missing: Vec<&&str> = catalog.difference(&accounted).collect();
    assert!(
        missing.is_empty(),
        "the parity golden must account for every application catalog operation, \
         either by pinning its transport contract or by listing it under \
         unpinned_operations; unaccounted: {missing:?}"
    );
}

#[test]
fn cursor_carrying_code_operations_are_pinned_on_every_surface() {
    let fixture = parity_fixture();
    let catalog = tracedecay::application_surface::application_surface_catalog()
        .expect("application catalog");
    let resolver = CatalogBindingResolver::new(&catalog);
    let definitions = get_tool_definitions().expect("tool definitions");

    for operation in CURSOR_CARRYING_CODE_OPERATIONS {
        let expected = &fixture["operations"][operation.as_str()];
        assert!(
            expected.is_object(),
            "{} carries a callable-code cursor and must be pinned by the parity \
             golden, not left to the unpinned roster",
            operation.as_str()
        );

        let resolution = BindingResolution {
            profile_id: ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).expect("profile"),
            operation: SurfaceOperationName::new(operation.as_str()).expect("operation"),
            protocol_revision: 1,
            negotiated_features: BTreeSet::new(),
        };
        for (surface, surface_name) in [
            (BindingSurface::Cli, "cli"),
            (BindingSurface::Mcp, "mcp"),
            (BindingSurface::Http, "http"),
        ] {
            let resolved = resolver
                .resolve_binding(surface, &resolution)
                .unwrap_or_else(|| panic!("{} must bind on {surface:?}", operation.as_str()));
            assert_eq!(
                resolved.binding_id.as_str(),
                expected["bindings"][surface_name]
                    .as_str()
                    .unwrap_or_else(|| panic!(
                        "{} must pin its {surface_name} binding",
                        operation.as_str()
                    )),
                "{} {surface_name} binding drifted from the golden",
                operation.as_str()
            );
        }

        let tool_name = format!("tracedecay_{}", operation.as_str());
        let definition = definitions
            .iter()
            .find(|definition| definition.name == tool_name)
            .unwrap_or_else(|| panic!("{tool_name} definition"));
        assert!(
            definition.input_schema["properties"]["meta"]["properties"]["cursor"].is_object(),
            "{tool_name} must advertise its continuation cursor over MCP"
        );
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
    let definitions = get_tool_definitions().expect("tool definitions");
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
fn handle_gated_feedback_tools_are_advertised_over_mcp() {
    let definitions = get_tool_definitions().expect("tool definitions");
    for tool_name in [
        "tracedecay_feedback_diagnostics",
        "tracedecay_feedback_get",
        "tracedecay_feedback_expand",
        "tracedecay_feedback_list",
        "tracedecay_feedback_impact",
        "tracedecay_affected_tests",
    ] {
        assert!(
            definitions
                .iter()
                .any(|definition| definition.name == tool_name),
            "{tool_name} must be advertised"
        );
    }
}

#[test]
fn handle_gated_feedback_capabilities_follow_catalog_availability() {
    let catalog = tracedecay::application_surface::application_surface_catalog()
        .expect("application catalog");
    for capability_id in [
        "capability.application.feedback.affected-tests",
        "capability.application.feedback.ci-failure-localize",
        "capability.application.feedback.diagnostics",
        "capability.application.feedback.expand",
        "capability.application.feedback.get",
        "capability.application.feedback.github-review-ingest",
        "capability.application.feedback.impact",
        "capability.application.feedback.list",
        "capability.application.feedback.proximity",
    ] {
        let capability_id =
            tracedecay_tool_catalog::CapabilityId::new(capability_id).expect("capability id");
        let capability = catalog
            .capability(&capability_id)
            .expect("feedback capability remains documented");
        assert!(capability.availability().is_callable(), "{capability_id}");
    }
}

#[test]
fn sse_projects_the_same_canonical_feedback_payload() {
    let fixture = parity_fixture();
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
    let fixture = parity_fixture();
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
    )
    .expect("canonical concealment fixture problem"));
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
            git_read_request(tracedecay_usecases::git_reads::GitReadRequestV1::Status)
        }
        ApplicationSurfaceOperation::GitDiff => {
            git_read_request(tracedecay_usecases::git_reads::GitReadRequestV1::Diff {
                scope: GitDiffScopeV1::WorkingTree,
            })
        }
        ApplicationSurfaceOperation::GitHistory => {
            git_read_request(tracedecay_usecases::git_reads::GitReadRequestV1::History {
                max_count: 10,
                path: None,
                follow: false,
                first_parent: false,
            })
        }
        ApplicationSurfaceOperation::GitBlame => {
            git_read_request(tracedecay_usecases::git_reads::GitReadRequestV1::Blame {
                path: "src/lib.rs".to_owned(),
                follow_renames: false,
            })
        }
        ApplicationSurfaceOperation::GitHunks => {
            git_read_request(tracedecay_usecases::git_reads::GitReadRequestV1::Hunks {
                scope: GitDiffScopeV1::WorkingTree,
                daemon_binding: None,
            })
        }
        ApplicationSurfaceOperation::GitPreview => git_requests().0,
        ApplicationSurfaceOperation::GitApply => git_requests().1,
        ApplicationSurfaceOperation::FeedbackImpact => {
            ApplicationSurfaceRequest::FeedbackImpact(FeedbackImpactSurfaceRequest {
                request_handle: "rh_missing-application-parity".to_owned(),
            })
        }
        ApplicationSurfaceOperation::AffectedTests => {
            ApplicationSurfaceRequest::AffectedTests(AffectedTestsSurfaceRequest {
                request_handle: "rh_missing-application-parity".to_owned(),
            })
        }
        ApplicationSurfaceOperation::TestResults => {
            ApplicationSurfaceRequest::TestResults(TestResultsSurfaceRequest::default())
        }
        // Cursor-carrying code operations decode straight from the golden's
        // pinned request body, so the fixture proves the reviewed request
        // schema still accepts it rather than restating it in Rust.
        operation if CURSOR_CARRYING_CODE_OPERATIONS.contains(&operation) => {
            parse_application_surface_request(operation, expected["request"].clone())
                .unwrap_or_else(|error| {
                    panic!(
                        "{} golden request body must parse: {error:?}",
                        operation.as_str()
                    )
                })
        }
        _ => feedback_request(expected["request_handle"].as_str().expect("request handle")),
    }
}

fn git_read_request(
    request: tracedecay_usecases::git_reads::GitReadRequestV1,
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
        id::<ProjectId>("project.application-parity"),
        id::<RepositoryId>("repository.application-parity"),
        Some(id::<WorktreeId>("worktree.application-parity")),
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
        Some(digest('0')),
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
        name: "Application Fixture".to_owned(),
        email: "application-fixture@example.com".to_owned(),
        at: UtcMicros(1_000_000),
    };
    let commit_intent = GitIndexCommitIntentV1::new(
        "application transport parity\n".to_owned(),
        identity.clone(),
        identity,
        GitIndexSigningPolicyV1::UnsignedPermitted,
    )
    .expect("commit intent");
    let preview_id = GitIndexPreviewId::new("preview.application-parity").expect("preview id");
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
            preview_input_id: None,
            selected_hunk_digests: Vec::new(),
            commit_intent: Some(commit_intent),
        }),
        ApplicationSurfaceRequest::GitApply(GitApplySurfaceRequest {
            preview_id: preview.preview_id,
            preview_digest: preview.preview_digest,
            idempotency_key: IdempotencyKey::new("idempotency.application-parity")
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
