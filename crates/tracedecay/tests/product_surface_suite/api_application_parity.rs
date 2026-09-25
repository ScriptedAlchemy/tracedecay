use std::collections::BTreeSet;

use axum::body::Body;
use axum::extract::Extension;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use tracedecay_api::{
    CanonicalInvocationResult, HttpApplicationControls, HttpApplicationRequest, HttpSseEvent,
    application_router,
};
use tracedecay_contracts::{
    APPLICATION_DEFAULT_PROFILE_ID, ApplicationContractError, CancellationSignal, Deadline,
    RequestId, ResultContractRef, RetryDirective, SafeDiagnostic, StreamEvent,
};
use tracedecay_daemon_protocol::{
    ApplicationSurfaceRequest, BindingResolution, BindingResolver, CatalogBindingResolver,
    FeedbackSurfaceRequest, RequestedOutputFormat,
};
use tracedecay_daemon_service::application_surface::{
    resolve_application_surface_dispatch, resolve_http_application_surface_dispatch,
};
use tracedecay_domain::UtcMicros;
use tracedecay_mcp::get_tool_definitions;
use tracedecay_mcp::tools::dispatch::resolve_mcp_application_surface_dispatch;
use tracedecay_tool_catalog::{
    ApplicationSurfaceOperation, BindingSurface, OperationId, ProfileId, SchemaId,
    SurfaceOperationName,
};

#[tokio::test]
async fn catalog_advertised_specialized_http_routes_invoke_the_application_owner() {
    let owner = |request: HttpApplicationRequest| async move {
        Ok::<_, ApplicationContractError>(CanonicalInvocationResult::<serde_json::Value>::new(
            tracedecay_tool_catalog::BindingId::new(format!(
                "binding.http.{}.v1",
                request.operation.as_str()
            ))
            .expect("binding id"),
            Err(tracedecay_contracts::ApplicationProblemEnvelope::new(
                ResultContractRef::new(
                    SchemaId::new("schema.test.feedback.result").expect("schema id"),
                    1,
                )
                .expect("result contract"),
                request.request_id,
                tracedecay_contracts::ApplicationProblem::unavailable(
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
    let catalog = tracedecay_daemon_service::application_surface::application_surface_catalog()
        .expect("catalog snapshot");
    let resolver = CatalogBindingResolver::new(&catalog);

    for (route, operation) in [
        (
            "/feedback/diagnostics",
            ApplicationSurfaceOperation::FeedbackDiagnostics,
        ),
        ("/feedback/get", ApplicationSurfaceOperation::FeedbackGet),
        (
            "/feedback/expand",
            ApplicationSurfaceOperation::FeedbackExpand,
        ),
        ("/feedback/list", ApplicationSurfaceOperation::FeedbackList),
        (
            "/feedback/impact",
            ApplicationSurfaceOperation::FeedbackImpact,
        ),
        (
            "/feedback/advisory_cycle",
            ApplicationSurfaceOperation::FeedbackAdvisoryCycle,
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
            ApplicationSurfaceOperation::GitHubStackSignalExpand,
        ),
        (
            "/native-integration/worktree_inventory",
            ApplicationSurfaceOperation::NativeIntegrationWorktreeInventory,
        ),
        (
            "/native-integration/worktree_cleanup_inspect",
            ApplicationSurfaceOperation::NativeIntegrationWorktreeInspect,
        ),
        (
            "/native-integration/worktree_cleanup_confirm",
            ApplicationSurfaceOperation::NativeIntegrationWorktreeConfirm,
        ),
        (
            "/native-integration/worktree_cleanup_remove",
            ApplicationSurfaceOperation::NativeIntegrationWorktreeRemove,
        ),
        (
            "/native-integration/worktree_cleanup_reconcile",
            ApplicationSurfaceOperation::NativeIntegrationWorktreeReconcile,
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
fn cli_mcp_and_http_dispatch_feedback_list_to_one_callable_contract() {
    let operation = ApplicationSurfaceOperation::FeedbackList;
    let request = || {
        ApplicationSurfaceRequest::Feedback(
            FeedbackSurfaceRequest::new("rh_missing-application-parity".to_owned())
                .expect("feedback request"),
        )
    };
    let request_id = |surface: &str| {
        RequestId::new(format!("request.{surface}.feedback_list")).expect("request id")
    };
    let dispatched = [
        (
            "binding.cli.feedback_list.v1",
            resolve_application_surface_dispatch(
                BindingSurface::Cli,
                operation,
                request_id("cli"),
                request(),
                RequestedOutputFormat::Json,
            )
            .expect("CLI dispatch"),
        ),
        (
            "binding.mcp.feedback_list.v1",
            resolve_mcp_application_surface_dispatch(
                operation,
                request_id("mcp"),
                request(),
                RequestedOutputFormat::Json,
            )
            .expect("MCP dispatch"),
        ),
        (
            "binding.http.feedback_list.v1",
            resolve_http_application_surface_dispatch(
                operation,
                request_id("http"),
                request(),
                RequestedOutputFormat::Json,
            )
            .expect("HTTP dispatch"),
        ),
    ];
    for (binding_id, dispatched) in dispatched {
        assert_eq!(dispatched.invocation.binding_id.as_str(), binding_id);
        assert_eq!(
            dispatched.invocation.request_schema.schema_id().as_str(),
            "schema.application.feedback.list.request"
        );
        assert_eq!(
            dispatched.invocation.result_schema.schema_id().as_str(),
            "schema.application.feedback.list.result"
        );
    }
}

#[test]
fn extended_primitive_reads_bind_cli_mcp_and_http() {
    let catalog = tracedecay_daemon_service::application_surface::application_surface_catalog()
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
        ApplicationSurfaceOperation::HealthRead,
        ApplicationSurfaceOperation::StorageStatus,
        ApplicationSurfaceOperation::DiagnosticsRead,
    ] {
        for surface in [
            BindingSurface::Cli,
            BindingSurface::Mcp,
            BindingSurface::Http,
        ] {
            let resolution = BindingResolution {
                profile_id: ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).expect("profile"),
                operation: SurfaceOperationName::new(operation.name_for_surface(surface))
                    .expect("operation"),
                protocol_revision: 1,
                negotiated_features: BTreeSet::new(),
            };
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
    let contributions = tracedecay_contracts::application_catalog_contributions()
        .expect("application catalog contributions");
    let registry = tracedecay_contracts::mcp_executable_binding_registry().expect("MCP registry");
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
        let tool_name = operation.mcp_tool_name();
        let definition = definitions
            .iter()
            .find(|definition| definition.name == tool_name)
            .unwrap_or_else(|| panic!("{tool_name} definition"));
        let operation_id =
            OperationId::new(format!("operation.application.{}", operation.as_str()))
                .expect("operation ID");
        let capability_id = registry
            .get(&operation_id)
            .and_then(|availability| availability.binding())
            .expect("MCP executable")
            .capability_id();
        let canonical_description = contributions
            .iter()
            .flat_map(|contribution| contribution.capabilities())
            .find(|capability| capability.capability_id() == capability_id)
            .unwrap_or_else(|| panic!("{tool_name} application capability"))
            .routing()
            .description();
        assert_eq!(
            definition.description, canonical_description,
            "{tool_name} must advertise its canonical application description"
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
        let required = definition
            .input_schema
            .get("required")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .map(|property| property.as_str().expect("required property name"))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            required,
            expected_required.iter().copied().collect::<BTreeSet<_>>(),
            "{tool_name} required properties"
        );
        tracedecay_daemon_protocol::parse_application_surface_request(operation, request)
            .unwrap_or_else(|error| panic!("{tool_name} must parse: {error}"));
    }
}

#[test]
fn sse_projects_the_same_canonical_feedback_payload() {
    let payload = serde_json::json!({
        "finding_id": "feedback-finding.application-parity.fixture",
        "summary": "Canonical feedback evidence remains transport-neutral"
    });
    let event = HttpSseEvent::from(StreamEvent::item(7, payload.clone()).expect("item"));
    let wire = serde_json::to_value(event).expect("serialize SSE event");

    assert_eq!(wire["event"], "item");
    assert_eq!(wire["data"]["sequence"], 7);
    assert_eq!(wire["data"]["item"], payload);
}

#[test]
fn http_concealment_omits_binding_identity() {
    let result = Err(tracedecay_contracts::ApplicationProblemEnvelope::new(
        ResultContractRef::new(
            SchemaId::new("schema.application.feedback.get.result").expect("schema id"),
            1,
        )
        .expect("result contract"),
        RequestId::new("request.feedback-concealment").expect("request id"),
        tracedecay_contracts::ApplicationProblem::not_found_or_not_authorized(
            RetryDirective::Never,
        ),
    )
    .expect("canonical concealment fixture problem"));
    let value = serde_json::to_value(
        CanonicalInvocationResult::<serde_json::Value>::new(
            tracedecay_tool_catalog::BindingId::new("binding.http.feedback_get.v1")
                .expect("binding id"),
            result,
        )
        .into_http_json(),
    )
    .expect("HTTP JSON");

    assert_eq!(
        value["value"]["problem"]["kind"],
        "not_found_or_not_authorized"
    );
    assert!(value["value"].get("binding_id").is_none());
}
