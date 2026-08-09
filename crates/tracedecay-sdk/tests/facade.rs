use tracedecay_sdk::operations::{
    ApplicationGitStatus, OperationTransport, TypedOperation, UNAVAILABLE_OPERATIONS, WorkCreate,
    WorkflowRegisterDefinition,
};
use tracedecay_sdk::{
    CancellationContext, CancellationSignal, CancellationState, CancellationTokenId, api,
    application, domain, operation, remote, work, workflow,
};

#[test]
fn canonical_contracts_are_available_without_sdk_copies() {
    fn accepts_application_envelope<T>(_: Option<application::ApplicationEnvelope<T>>) {}
    fn accepts_api_envelope<T>(_: Option<api::HttpJsonEnvelope<T>>) {}
    fn accepts_domain_identity(_: Option<domain::RepositoryId>) {}
    fn accepts_operation_metadata(_: Option<operation::CapabilityManifestV1>) {}
    fn accepts_remote_authority(_: Option<domain::CurrentRemoteAuthorityStateV1>) {}
    fn accepts_remote_response<T>(_: Option<remote::protocol::RemoteProtocolResponseV1<T>>) {}

    accepts_application_envelope::<serde_json::Value>(None);
    accepts_api_envelope::<serde_json::Value>(None);
    accepts_domain_identity(None);
    accepts_operation_metadata(None);
    accepts_remote_authority(None);
    accepts_remote_response::<serde_json::Value>(None);

    let _: operation::ReceiptContract = operation::ReceiptContract::Operation;
    let _: Option<application::ApplicationOperation> = None;
}

#[test]
fn cancellation_types_are_the_canonical_application_types() {
    let signal = CancellationSignal::active("cancel.sdk.facade").expect("cancellation signal");
    let canonical_signal: application::CancellationSignal = signal.clone();
    let _: CancellationSignal = canonical_signal;

    assert!(signal.cancel(domain::UtcMicros(41)));
    let context: CancellationContext = signal.context();
    let token: CancellationTokenId = context.token_id.clone();
    let _: application::CancellationTokenId = token;
    assert!(matches!(
        context.state,
        CancellationState::Cancelled {
            requested_at: domain::UtcMicros(41)
        }
    ));
}

#[test]
fn remote_outcomes_are_the_canonical_application_types() {
    let outcome: Option<remote::replay::RemoteReplayOutcomeV1> = None;
    let canonical: Option<application::remote::replay::RemoteReplayOutcomeV1> = outcome;
    let _: Option<remote::replay::RemoteReplayOutcomeV1> = canonical;
}

#[test]
fn work_create_descriptor_matches_the_mounted_binding() {
    let registry = work::executable_binding_registry().expect("canonical Work registry");
    let binding = registry
        .get(&operation::OperationId::new(WorkCreate::OPERATION_ID).unwrap())
        .and_then(|availability| availability.binding())
        .expect("mounted Work create binding");

    assert_eq!(
        WorkCreate::TRANSPORT,
        OperationTransport::Http {
            route: "/application/work/create"
        }
    );
    assert_eq!(WorkCreate::BINDING_ID, "binding.http.work.create");
    assert_eq!(WorkCreate::EFFECT, binding.effect());
    assert_eq!(WorkCreate::IDEMPOTENCY, binding.idempotency());
    assert_eq!(
        WorkCreate::MAXIMUM_DEADLINE_MILLIS,
        binding.deadline().maximum_millis()
    );
    assert_eq!(WorkCreate::DEADLINE_BEHAVIOR, binding.deadline().behavior());
}

#[test]
fn workflow_register_definition_descriptor_matches_the_mounted_binding() {
    let registry = workflow::executable_binding_registry().expect("canonical Workflow registry");
    let binding = registry
        .get(&operation::OperationId::new(WorkflowRegisterDefinition::OPERATION_ID).unwrap())
        .and_then(|availability| availability.binding())
        .expect("mounted workflow register-definition binding");

    assert_eq!(
        WorkflowRegisterDefinition::TRANSPORT,
        OperationTransport::Http {
            route: "/application/workflow/register-definition"
        }
    );
    assert_eq!(
        WorkflowRegisterDefinition::BINDING_ID,
        "binding.http.workflow.register_definition"
    );
    assert_eq!(WorkflowRegisterDefinition::EFFECT, binding.effect());
    assert_eq!(
        WorkflowRegisterDefinition::IDEMPOTENCY,
        binding.idempotency()
    );
    assert_eq!(
        WorkflowRegisterDefinition::MAXIMUM_DEADLINE_MILLIS,
        binding.deadline().maximum_millis()
    );
    assert_eq!(
        WorkflowRegisterDefinition::DEADLINE_BEHAVIOR,
        binding.deadline().behavior()
    );
    assert_eq!(
        binding.result_schema().schema_ref().schema_id().as_str(),
        WorkflowRegisterDefinition::RESULT_SCHEMA_ID
    );
    assert_eq!(
        binding.result_schema().schema_ref().revision(),
        WorkflowRegisterDefinition::RESULT_SCHEMA_REVISION
    );
}

#[test]
fn configuration_family_is_http_mounted_and_not_generated_as_unavailable() {
    let registry = application::sdk_executable_binding_registry().expect("canonical SDK registry");
    let unavailable = UNAVAILABLE_OPERATIONS
        .iter()
        .map(|operation| operation.operation_id)
        .collect::<std::collections::BTreeSet<_>>();

    for operation in application::configuration::CONFIGURATION_SURFACE_OPERATION_NAMES {
        let operation_id = format!("operation.application.{operation}");
        let binding = registry
            .get(&operation::OperationId::new(operation_id.clone()).expect("operation ID"))
            .and_then(|availability| availability.binding())
            .expect("mounted configuration operation");
        assert_eq!(
            binding.sdk_method().as_str(),
            format!("application_{operation}")
        );
        assert!(matches!(
            binding.transport(),
            operation::SdkTransportBindingV1::Http { route_path }
                if route_path == &format!("/application/configuration/{operation}")
        ));
        assert!(!unavailable.contains(operation_id.as_str()));
    }
}

#[test]
fn generated_unavailable_operations_match_the_canonical_sdk_registry() {
    let registry = application::sdk_executable_binding_registry().expect("canonical SDK registry");
    let expected = registry
        .iter()
        .filter_map(|availability| match availability {
            operation::SdkExecutableBindingAvailabilityV1::Available { .. } => None,
            operation::SdkExecutableBindingAvailabilityV1::Unavailable {
                operation_id,
                disposition,
            } => Some((
                operation_id.as_str(),
                (
                    operation_id
                        .as_str()
                        .strip_prefix("operation.")
                        .unwrap_or(operation_id.as_str())
                        .replace('.', "_"),
                    *disposition,
                ),
            )),
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let generated = UNAVAILABLE_OPERATIONS
        .iter()
        .map(|operation| {
            (
                operation.operation_id,
                (operation.operation.to_owned(), operation.disposition),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();

    assert_eq!(generated, expected);
}

#[test]
fn canonical_problem_envelope_serializes_verbatim() {
    let envelope = application::ApplicationProblemEnvelope::new(
        application::ResultContractRef::new(
            operation::SchemaId::new("schema.sdk.problem").expect("schema id"),
            1,
        )
        .expect("result contract"),
        application::RequestId::new("request.sdk.problem").expect("request id"),
        application::ApplicationProblem::unavailable(
            application::SafeDiagnostic::new(
                "sdk.test_unavailable",
                "The requested operation is unavailable",
            )
            .expect("safe diagnostic"),
        ),
    )
    .expect("construct problem envelope");

    let value = serde_json::to_value(envelope).expect("serialize problem envelope");

    assert_eq!(value["contract"]["schema_id"], "schema.sdk.problem");
    assert_eq!(value["contract"]["schema_revision"], 1);
    assert_eq!(value["request_id"], "request.sdk.problem");
    assert_eq!(value["problem"]["kind"], "unavailable");
    assert_eq!(value["problem"]["code"], "sdk.test_unavailable");
    assert_eq!(
        value["problem"]["diagnostic"]["code"],
        "sdk.test_unavailable"
    );
    assert_eq!(value["problem"]["retry"], "after_delay");
}

#[test]
fn canonical_operation_receipt_round_trips() {
    let receipt = application::OperationReceipt::completed(
        domain::UtcMicros(10),
        domain::UtcMicros(20),
        application::Deadline::new(domain::UtcMicros(30)).expect("deadline"),
        application::OperationBudgetUsage {
            units_consumed: 2,
            bytes_consumed: 64,
            elapsed_micros: 10,
        },
    )
    .expect("completed receipt");

    let value = serde_json::to_value(&receipt).expect("serialize receipt");
    assert_eq!(
        value,
        serde_json::json!({
            "started_at": 10,
            "ended_at": 20,
            "effective_deadline": {"expires_at": 30},
            "cancellation": null,
            "budget": {
                "units_consumed": 2,
                "bytes_consumed": 64,
                "elapsed_micros": 10
            },
            "termination": "completed"
        })
    );

    let decoded: application::OperationReceipt =
        serde_json::from_value(value).expect("deserialize receipt");
    assert_eq!(decoded, receipt);

    let canonical: tracedecay_application::OperationReceipt = decoded;
    let _: application::OperationReceipt = canonical;
}

#[test]
fn application_git_status_descriptor_matches_the_mounted_http_binding() {
    let registry = application::sdk_executable_binding_registry().expect("SDK registry");
    let binding = registry
        .get(&operation::OperationId::new(ApplicationGitStatus::OPERATION_ID).unwrap())
        .and_then(|availability| availability.binding())
        .expect("Git status SDK binding");

    assert_eq!(binding.sdk_method().as_str(), "application_git_status");
    assert_eq!(
        binding.binding_id().as_str(),
        ApplicationGitStatus::BINDING_ID
    );
    assert_eq!(ApplicationGitStatus::EFFECT, binding.effect());
    assert_eq!(ApplicationGitStatus::IDEMPOTENCY, binding.idempotency());
    assert_eq!(
        ApplicationGitStatus::TRANSPORT,
        OperationTransport::Http {
            route: "/application/git/status"
        }
    );
}
