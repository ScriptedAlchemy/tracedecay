use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header::CONTENT_TYPE};
use schemars::JsonSchema;
use serde_json::Value;
use tower::ServiceExt;
use tracedecay_application::{
    ApplicationWireOperation, ApplicationWireSchemaRegistryV1, ApplicationWireSchemaV1,
};
use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, BindingId, BindingStatus, BindingSurface,
    CancellationContract, CancellationPoint, CapabilityId, CapabilityManifestInputV1,
    CapabilityManifestV1, DeadlineBehavior, DeadlineContract, DeniedDisclosurePolicy, EffectClass,
    IdempotencyContract, InverseContract, LifecycleClass, PrivacyClass, ProfileId,
    ProtocolRevisionRange, ReceiptContract, ReconciliationContract, RevalidationContract,
    RoutingContractV1, SchemaBodyAuthorityV1, SchemaId, SchemaRef, ScopeRequirement,
    StreamingContract, SurfaceBindingInputV1, SurfaceBindingV1, SurfaceOperationName,
    TerminalState, TerminalStateContract, UseCaseId,
};

use super::{
    OPENAPI_DOCUMENT_ROUTE_PATH, OpenApiDocumentError, OpenApiRouterBuildError, openapi_router,
};
use crate::HttpRouteDocumentV1;

#[derive(JsonSchema)]
struct OpenApiRequestFixture;

#[derive(JsonSchema)]
struct OpenApiResultFixture;

fn canonical_route_schema(
    operation: ApplicationWireOperation,
    path: &str,
    surface: BindingSurface,
) -> (HttpRouteDocumentV1, ApplicationWireSchemaV1) {
    let operation_name = operation.as_str();
    let capability_id =
        CapabilityId::new(format!("capability.{operation_name}")).expect("fixture capability");
    let binding_id =
        BindingId::new(format!("binding.{operation_name}.http")).expect("fixture binding");
    let request_schema = SchemaRef::new(
        SchemaId::new(format!("schema.{operation_name}.request")).expect("fixture request schema"),
        1,
    )
    .expect("fixture request schema ref");
    let result_schema = SchemaRef::new(
        SchemaId::new(format!("schema.{operation_name}.result")).expect("fixture result schema"),
        1,
    )
    .expect("fixture result schema ref");
    let binding = SurfaceBindingV1::new(SurfaceBindingInputV1 {
        binding_id: binding_id.clone(),
        capability_id: capability_id.clone(),
        surface,
        operation: SurfaceOperationName::new(operation_name).expect("fixture operation"),
        protocol_revisions: ProtocolRevisionRange::new(1, 1).expect("fixture protocol revision"),
        required_features: Vec::new(),
        status: BindingStatus::Current,
        alias_of: None,
    })
    .expect("fixture surface binding");
    let cancellation = CancellationContract::cooperative(vec![CancellationPoint::BeforeAdmission])
        .expect("fixture cancellation contract");
    let deadline = DeadlineContract::new(2_500, DeadlineBehavior::ReturnOperationReceipt)
        .expect("fixture deadline contract");
    let terminal_states = TerminalStateContract::new(vec![
        TerminalState::Completed,
        TerminalState::Cancelled,
        TerminalState::TimedOut,
        TerminalState::Failed,
        TerminalState::Partial,
    ])
    .expect("fixture terminal states");
    let manifest = CapabilityManifestV1::new(CapabilityManifestInputV1 {
        capability_id: capability_id.clone(),
        use_case_id: UseCaseId::new(format!("use-case.{operation_name}"))
            .expect("fixture use case"),
        routing: RoutingContractV1::new(
            1,
            format!("Route {operation_name}"),
            format!("Route {operation_name} fixture."),
            Vec::new(),
        )
        .expect("fixture routing contract"),
        request_schema: request_schema.clone(),
        result_schema: result_schema.clone(),
        effect: EffectClass::Read,
        scope: ScopeRequirement::none(),
        authority: AuthorityRequirement::None,
        denied_disclosure: DeniedDisclosurePolicy::Explicit,
        privacy: PrivacyClass::PublicMetadata,
        lifecycle: LifecycleClass::Stateless,
        streaming: StreamingContract::Unsupported,
        cancellation: cancellation.clone(),
        deadline: deadline.clone(),
        pagination: None,
        idempotency: IdempotencyContract::NotRequired,
        inverse: InverseContract::NotApplicable,
        authority_revalidation: RevalidationContract::NotRequired,
        reconciliation: ReconciliationContract::NotRequired,
        receipt: ReceiptContract::Operation,
        terminal_states: terminal_states.clone(),
        availability: AvailabilityContract::Available,
        binding_ids: vec![binding_id.clone()],
        profile_eligibility: vec![ProfileId::new("profile.default").expect("fixture profile")],
        required_features: Vec::new(),
    })
    .expect("fixture manifest");
    let request = SchemaBodyAuthorityV1::for_type::<OpenApiRequestFixture>(request_schema.clone())
        .expect("fixture request schema body");
    let result = SchemaBodyAuthorityV1::for_type::<OpenApiResultFixture>(result_schema.clone())
        .expect("fixture result schema body");
    let schema =
        ApplicationWireSchemaV1::from_catalog(operation, &manifest, &binding, request, result)
            .expect("fixture wire schema");
    let route = HttpRouteDocumentV1 {
        method: "POST",
        path: path.to_owned(),
        operation: operation_name.to_owned(),
        capability_id: capability_id.as_str().to_owned(),
        binding_id: binding_id.as_str().to_owned(),
        request_schema: request_schema.schema_id().as_str().to_owned(),
        request_schema_revision: request_schema.revision(),
        result_schema: result_schema.schema_id().as_str().to_owned(),
        result_schema_revision: result_schema.revision(),
        cancellation,
        deadline,
        pagination: None,
        receipt: ReceiptContract::Operation,
        terminal_states,
    };
    (route, schema)
}

#[tokio::test]
async fn router_serves_byte_stable_authorized_document() {
    let (status_route, status_schema) = canonical_route_schema(
        ApplicationWireOperation::GitStatus,
        "/git/status",
        BindingSurface::Http,
    );
    let (diff_route, diff_schema) = canonical_route_schema(
        ApplicationWireOperation::GitDiff,
        "/git/diff",
        BindingSurface::Http,
    );
    let registry = ApplicationWireSchemaRegistryV1::new(vec![status_schema, diff_schema])
        .expect("fixture schema registry");
    let forward = openapi_router(&[status_route.clone(), diff_route.clone()], &registry)
        .expect("forward OpenAPI router")
        .oneshot(
            Request::builder()
                .uri(OPENAPI_DOCUMENT_ROUTE_PATH)
                .body(Body::empty())
                .expect("forward request"),
        )
        .await
        .expect("forward response");
    assert_eq!(forward.status(), StatusCode::OK);
    assert_eq!(forward.headers()[CONTENT_TYPE], "application/json");
    let forward_body = to_bytes(forward.into_body(), usize::MAX)
        .await
        .expect("forward body");
    let document: Value = serde_json::from_slice(&forward_body).expect("served OpenAPI JSON");
    assert_eq!(document["openapi"], "3.1.0");
    assert_eq!(
        document["paths"]["/git/status"]["post"]["operationId"],
        "git_status"
    );
    assert_eq!(
        document["paths"]["/git/diff"]["post"]["operationId"],
        "git_diff"
    );
    assert_eq!(
        document["paths"]["/git/status"]["post"]["responses"]["200"]["content"]["application/json"]
            ["schema"]["properties"]["value"]["properties"]["outcome"]["oneOf"][0]["properties"]["value"]
            ["properties"]["payload"]["oneOf"][0]["$ref"],
        "#/components/schemas/binding.git_status.http.result"
    );

    let reverse = openapi_router(&[diff_route, status_route], &registry)
        .expect("reverse OpenAPI router")
        .oneshot(
            Request::builder()
                .uri(OPENAPI_DOCUMENT_ROUTE_PATH)
                .body(Body::empty())
                .expect("reverse request"),
        )
        .await
        .expect("reverse response");
    let reverse_body = to_bytes(reverse.into_body(), usize::MAX)
        .await
        .expect("reverse body");
    assert_eq!(forward_body, reverse_body);
}

#[test]
fn router_build_rejects_missing_binding_schema() {
    let (route, _) = canonical_route_schema(
        ApplicationWireOperation::GitStatus,
        "/git/status",
        BindingSurface::Http,
    );
    let registry = ApplicationWireSchemaRegistryV1::new(Vec::new()).expect("empty schema registry");

    let error = openapi_router(&[route.clone()], &registry)
        .err()
        .expect("missing schema must fail construction");

    assert!(matches!(
        error,
        OpenApiRouterBuildError::Document(OpenApiDocumentError::MissingWireSchema {
            binding_id
        }) if binding_id == route.binding_id
    ));
}

#[test]
fn router_build_rejects_route_schema_drift() {
    let (mut route, schema) = canonical_route_schema(
        ApplicationWireOperation::GitStatus,
        "/git/status",
        BindingSurface::Http,
    );
    route.request_schema_revision += 1;
    let registry =
        ApplicationWireSchemaRegistryV1::new(vec![schema]).expect("fixture schema registry");

    let error = openapi_router(&[route.clone()], &registry)
        .err()
        .expect("schema drift must fail construction");

    assert!(matches!(
        error,
        OpenApiRouterBuildError::Document(OpenApiDocumentError::MismatchedWireSchema {
            binding_id
        }) if binding_id == route.binding_id
    ));
}

#[test]
fn router_build_rejects_non_http_binding_schema() {
    let (route, schema) = canonical_route_schema(
        ApplicationWireOperation::GitStatus,
        "/git/status",
        BindingSurface::Cli,
    );
    let registry =
        ApplicationWireSchemaRegistryV1::new(vec![schema]).expect("fixture schema registry");

    let error = openapi_router(&[route.clone()], &registry)
        .err()
        .expect("surface drift must fail construction");

    assert!(matches!(
        error,
        OpenApiRouterBuildError::Document(OpenApiDocumentError::MismatchedWireSchema {
            binding_id
        }) if binding_id == route.binding_id
    ));
}
