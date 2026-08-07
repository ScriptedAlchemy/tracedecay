mod common;

use schemars::JsonSchema;
use tracedecay_tool_catalog::{
    BindingId, CatalogContributionInputV1, CatalogContributionV1, CodecBindingKey, ContributionId,
    ExecutableBindingAvailabilityV1, ExecutableBindingRegistryV1, ExecutableBindingV1,
    ExecutableSchemaAuthority, ExecutableUnavailableDispositionV1, ExecutionOwnerV1, OperationId,
    RouteExposureV1, SchemaBodyAuthorityV1, SdkExecutableBindingV1, SdkTransportBindingV1,
    ServiceId, SurfaceOperationName,
};

use common::{capability_id, profile_id, read_manifest, schema, use_case_id};

#[derive(JsonSchema)]
#[allow(dead_code)]
struct ReadRequest {
    path: String,
}

#[derive(JsonSchema)]
#[allow(dead_code)]
struct ReadResult {
    contents: String,
}

fn binding() -> ExecutableBindingV1 {
    let binding_id = BindingId::new("binding.http.source-read").unwrap();
    let manifest = read_manifest(
        capability_id("capability.source.read"),
        use_case_id("use-case.source.read"),
        schema("schema.source.read.request"),
        schema("schema.source.read.result"),
        vec![binding_id.clone()],
        vec![profile_id("profile.default")],
    );
    let request_schema =
        SchemaBodyAuthorityV1::for_type::<ReadRequest>(manifest.request_schema().clone()).unwrap();
    let result_schema =
        SchemaBodyAuthorityV1::for_type::<ReadResult>(manifest.result_schema().clone()).unwrap();

    ExecutableBindingV1::direct(
        &manifest,
        OperationId::new("operation.source.read").unwrap(),
        ServiceId::new("service.source-read").unwrap(),
        request_schema,
        result_schema,
        CodecBindingKey::new("codec.source-read.json.v1").unwrap(),
        RouteExposureV1::Public {
            binding_id,
            route_path: "/application/source/read".to_owned(),
        },
    )
    .unwrap()
}

#[test]
fn schema_bodies_are_derived_from_rust_type_authority() {
    let binding = binding();

    assert_eq!(
        binding.request_schema().schema_ref().schema_id().as_str(),
        "schema.source.read.request"
    );
    assert_eq!(
        binding.request_schema().body()["properties"]["path"]["type"],
        "string"
    );
    assert_eq!(
        binding.result_schema().body()["properties"]["contents"]["type"],
        "string"
    );
    assert_eq!(
        binding.request_schema().digest(),
        SchemaBodyAuthorityV1::for_type::<ReadRequest>(
            binding.request_schema().schema_ref().clone()
        )
        .unwrap()
        .digest()
    );
}

#[test]
fn contribution_retains_schema_bodies_beside_the_owning_manifest() {
    let manifest = read_manifest(
        capability_id("capability.source.read"),
        use_case_id("use-case.source.read"),
        schema("schema.source.read.request"),
        schema("schema.source.read.result"),
        Vec::new(),
        vec![profile_id("profile.default")],
    );
    let authority = ExecutableSchemaAuthority::for_types::<ReadRequest, ReadResult>(&manifest)
        .expect("schema authority");
    let contribution = CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.source").unwrap(),
        depends_on: Vec::new(),
        capabilities: vec![manifest.clone()],
        retrieval_primitives: Vec::new(),
        bindings: Vec::new(),
    })
    .unwrap()
    .with_executable_schemas(vec![authority])
    .expect("schema-backed contribution");

    let retained = contribution
        .executable_schema(manifest.capability_id())
        .expect("retained schema authority");
    assert_eq!(
        retained.request_schema().schema_ref(),
        manifest.request_schema()
    );
    assert_eq!(
        retained.result_schema().schema_ref(),
        manifest.result_schema()
    );
    assert_eq!(
        retained.request_schema().body()["properties"]["path"]["type"],
        "string"
    );
}

#[test]
fn contribution_rejects_schema_bodies_without_their_owning_manifest() {
    let owned_manifest = read_manifest(
        capability_id("capability.source.read"),
        use_case_id("use-case.source.read"),
        schema("schema.source.read.request"),
        schema("schema.source.read.result"),
        Vec::new(),
        vec![profile_id("profile.default")],
    );
    let foreign_manifest = read_manifest(
        capability_id("capability.source.foreign"),
        use_case_id("use-case.source.foreign"),
        schema("schema.source.foreign.request"),
        schema("schema.source.foreign.result"),
        Vec::new(),
        vec![profile_id("profile.default")],
    );
    let foreign_authority =
        ExecutableSchemaAuthority::for_types::<ReadRequest, ReadResult>(&foreign_manifest)
            .expect("schema authority");
    let contribution = CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.source").unwrap(),
        depends_on: Vec::new(),
        capabilities: vec![owned_manifest],
        retrieval_primitives: Vec::new(),
        bindings: Vec::new(),
    })
    .unwrap();

    let error = contribution
        .with_executable_schemas(vec![foreign_authority])
        .expect_err("foreign schema authority must be rejected");

    assert!(matches!(
        error,
        tracedecay_tool_catalog::CatalogValidationError::InvalidCapability {
            reason: "executable schema authority has no owning manifest",
            ..
        }
    ));
}

#[test]
fn executable_binding_wire_is_deterministic_and_keeps_stable_names() {
    let binding = binding();
    let first = serde_json::to_vec(&binding).unwrap();
    let second = serde_json::to_vec(&binding).unwrap();

    assert_eq!(first, second);
    let value: serde_json::Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(value["operation_id"], "operation.source.read");
    assert_eq!(value["owner"]["mode"], "direct");
    assert_eq!(value["owner"]["service_id"], "service.source-read");
    assert_eq!(value["codec"]["codec"], "json");
    assert_eq!(value["codec"]["binding_key"], "codec.source-read.json.v1");
    assert_eq!(value["exposure"]["visibility"], "public");
    assert_eq!(value["exposure"]["binding_id"], "binding.http.source-read");
    assert_eq!(value["effect"], "read");
    assert_eq!(value["idempotency"], "not_required");
    assert_eq!(value["cancellation"]["mode"], "cooperative");
    assert_eq!(value["deadline"]["maximum_millis"], 1_000);
    assert_eq!(value["deadline"]["behavior"], "return_operation_receipt");
    assert_eq!(value["reconciliation"], "not_required");
    assert_eq!(value["receipt"], "operation");
    assert_eq!(
        value["terminal_states"]["states"],
        serde_json::json!(["completed", "cancelled", "timed_out", "failed", "partial"])
    );
}

#[test]
fn manifest_schema_or_route_mismatch_is_rejected() {
    let manifest = read_manifest(
        capability_id("capability.source.read"),
        use_case_id("use-case.source.read"),
        schema("schema.source.read.request"),
        schema("schema.source.read.result"),
        Vec::new(),
        vec![profile_id("profile.default")],
    );
    let wrong_request =
        SchemaBodyAuthorityV1::for_type::<ReadRequest>(schema("schema.other.request")).unwrap();
    let result =
        SchemaBodyAuthorityV1::for_type::<ReadResult>(manifest.result_schema().clone()).unwrap();

    assert!(
        ExecutableBindingV1::daemon_owned(
            &manifest,
            OperationId::new("operation.source.read").unwrap(),
            ServiceId::new("service.daemon").unwrap(),
            wrong_request,
            result,
            CodecBindingKey::new("codec.source-read.json.v1").unwrap(),
            RouteExposureV1::Internal,
        )
        .is_err()
    );

    let request =
        SchemaBodyAuthorityV1::for_type::<ReadRequest>(manifest.request_schema().clone()).unwrap();
    let result =
        SchemaBodyAuthorityV1::for_type::<ReadResult>(manifest.result_schema().clone()).unwrap();
    assert!(
        ExecutableBindingV1::direct(
            &manifest,
            OperationId::new("operation.source.read").unwrap(),
            ServiceId::new("service.source-read").unwrap(),
            request,
            result,
            CodecBindingKey::new("codec.source-read.json.v1").unwrap(),
            RouteExposureV1::Public {
                binding_id: BindingId::new("binding.http.undeclared").unwrap(),
                route_path: "/application/source/read".to_owned(),
            },
        )
        .is_err()
    );
}

#[test]
fn daemon_owned_binding_retains_its_service_owner() {
    let manifest = read_manifest(
        capability_id("capability.source.read"),
        use_case_id("use-case.source.read"),
        schema("schema.source.read.request"),
        schema("schema.source.read.result"),
        Vec::new(),
        vec![profile_id("profile.default")],
    );
    let request =
        SchemaBodyAuthorityV1::for_type::<ReadRequest>(manifest.request_schema().clone()).unwrap();
    let result =
        SchemaBodyAuthorityV1::for_type::<ReadResult>(manifest.result_schema().clone()).unwrap();
    let binding = ExecutableBindingV1::daemon_owned(
        &manifest,
        OperationId::new("operation.source.read").unwrap(),
        ServiceId::new("service.daemon").unwrap(),
        request,
        result,
        CodecBindingKey::new("codec.source-read.json.v1").unwrap(),
        RouteExposureV1::Internal,
    )
    .unwrap();

    assert!(matches!(
        binding.owner(),
        ExecutionOwnerV1::DaemonOwned { service_id }
            if service_id.as_str() == "service.daemon"
    ));
    assert_eq!(binding.owner().service_id().as_str(), "service.daemon");
}

#[test]
fn unavailable_disposition_cannot_carry_an_executable_binding() {
    let unavailable = ExecutableBindingAvailabilityV1::Unavailable {
        operation_id: OperationId::new("operation.source.read").unwrap(),
        disposition: ExecutableUnavailableDispositionV1::ServiceNotRegistered,
    };
    let value = serde_json::to_value(unavailable).unwrap();

    assert_eq!(value["state"], "unavailable");
    assert_eq!(value["operation_id"], "operation.source.read");
    assert_eq!(value["disposition"], "service_not_registered");
    assert!(value.get("binding").is_none());
}

#[test]
fn executable_registry_rejects_duplicate_operation_ids() {
    let binding = binding();
    let operation_id = binding.operation_id().clone();
    let first = ExecutableBindingAvailabilityV1::available(binding.clone());
    let duplicate = ExecutableBindingAvailabilityV1::available(binding);

    assert!(ExecutableBindingRegistryV1::new(vec![first, duplicate]).is_err());

    let registry =
        ExecutableBindingRegistryV1::new(vec![ExecutableBindingAvailabilityV1::Unavailable {
            operation_id: operation_id.clone(),
            disposition: ExecutableUnavailableDispositionV1::RouteUnavailable,
        }])
        .unwrap();
    assert!(registry.get(&operation_id).is_some());
}

#[test]
fn sdk_binding_keeps_the_named_mcp_transport_without_inventing_an_http_route() {
    let manifest = read_manifest(
        capability_id("capability.source.read"),
        use_case_id("use-case.source.read"),
        schema("schema.source.read.request"),
        schema("schema.source.read.result"),
        vec![BindingId::new("binding.mcp.source-read").unwrap()],
        vec![profile_id("profile.default")],
    );
    let executable = ExecutableBindingV1::daemon_owned(
        &manifest,
        OperationId::new("operation.source.read").unwrap(),
        ServiceId::new("service.source-read").unwrap(),
        SchemaBodyAuthorityV1::for_type::<ReadRequest>(manifest.request_schema().clone()).unwrap(),
        SchemaBodyAuthorityV1::for_type::<ReadResult>(manifest.result_schema().clone()).unwrap(),
        CodecBindingKey::new("codec.source-read.json.v1").unwrap(),
        RouteExposureV1::Internal,
    )
    .unwrap();

    let binding = SdkExecutableBindingV1::new(
        executable,
        BindingId::new("binding.mcp.source-read").unwrap(),
        SurfaceOperationName::new("source_read").unwrap(),
        SdkTransportBindingV1::McpTool {
            tool_name: "tracedecay_source_read".to_owned(),
        },
    )
    .unwrap();

    assert_eq!(binding.sdk_method().as_str(), "source_read");
    assert_eq!(binding.binding_id().as_str(), "binding.mcp.source-read");
    assert!(matches!(
        binding.transport(),
        SdkTransportBindingV1::McpTool { tool_name }
            if tool_name == "tracedecay_source_read"
    ));
    assert!(matches!(
        binding.executable().exposure(),
        RouteExposureV1::Internal
    ));
}
