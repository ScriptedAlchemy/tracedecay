mod common;

use schemars::JsonSchema;
use tracedecay_tool_catalog::{
    BindingId, CodecBindingKey, ExecutableBindingAvailabilityV1, ExecutableBindingRegistryV1,
    ExecutableBindingV1, ExecutableUnavailableDispositionV1, ExecutionOwnerV1, OperationId,
    RouteExposureV1, SchemaBodyAuthorityV1, ServiceId,
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
        RouteExposureV1::Public { binding_id },
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
fn executable_binding_wire_is_deterministic_and_keeps_stable_names() {
    let binding = binding();
    let first = serde_json::to_vec(&binding).unwrap();
    let second = serde_json::to_vec(&binding).unwrap();

    assert_eq!(first, second);
    assert_eq!(
        String::from_utf8(first.clone()).unwrap(),
        concat!(
            r#"{"capability_id":"capability.source.read","operation_id":"operation.source.read","#,
            r#""owner":{"mode":"direct","service_id":"service.source-read"},"#,
            r#""request_schema":{"schema_ref":{"schema_id":"schema.source.read.request","revision":1},"#,
            r#""body":{"$schema":"https://json-schema.org/draft/2020-12/schema","properties":{"path":{"type":"string"}},"required":["path"],"title":"ReadRequest","type":"object"},"#,
            r#""digest":"sha256:514659187a933c544e2d5572ea3bcb0298d2a087ccfc395d11ef30ac3876c03a"},"#,
            r#""result_schema":{"schema_ref":{"schema_id":"schema.source.read.result","revision":1},"#,
            r#""body":{"$schema":"https://json-schema.org/draft/2020-12/schema","properties":{"contents":{"type":"string"}},"required":["contents"],"title":"ReadResult","type":"object"},"#,
            r#""digest":"sha256:db8f100ea56f9e38862213e6a50d28e8747ee74877db4664aa40050e3bb50fc0"},"#,
            r#""codec":{"codec":"json","binding_key":"codec.source-read.json.v1"},"#,
            r#""exposure":{"visibility":"public","binding_id":"binding.http.source-read"},"#,
            r#""cancellation":{"mode":"cooperative","points":["before_admission","before_read","during_read"]}}"#
        )
    );
    let value: serde_json::Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(value["operation_id"], "operation.source.read");
    assert_eq!(value["owner"]["mode"], "direct");
    assert_eq!(value["owner"]["service_id"], "service.source-read");
    assert_eq!(value["codec"]["codec"], "json");
    assert_eq!(value["codec"]["binding_key"], "codec.source-read.json.v1");
    assert_eq!(value["exposure"]["visibility"], "public");
    assert_eq!(value["exposure"]["binding_id"], "binding.http.source-read");
    assert_eq!(value["cancellation"]["mode"], "cooperative");
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
    let first = ExecutableBindingAvailabilityV1::Available {
        binding: binding.clone(),
    };
    let duplicate = ExecutableBindingAvailabilityV1::Available { binding };

    assert!(ExecutableBindingRegistryV1::new(vec![first, duplicate]).is_err());

    let registry =
        ExecutableBindingRegistryV1::new(vec![ExecutableBindingAvailabilityV1::Unavailable {
            operation_id: operation_id.clone(),
            disposition: ExecutableUnavailableDispositionV1::RouteUnavailable,
        }])
        .unwrap();
    assert!(registry.get(&operation_id).is_some());
}
