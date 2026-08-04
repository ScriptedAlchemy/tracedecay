use tracedecay_sdk::operations::{TypedOperation, WorkAttemptFinish};
use tracedecay_sdk::{
    CancellationContext, CancellationSignal, CancellationState, CancellationTokenId, api,
    application, domain, operation, operations, work,
};

#[test]
fn canonical_contracts_are_available_without_sdk_copies() {
    fn accepts_application_envelope<T>(_: Option<application::ApplicationEnvelope<T>>) {}
    fn accepts_api_envelope<T>(_: Option<api::HttpJsonEnvelope<T>>) {}
    fn accepts_domain_identity(_: Option<domain::RepositoryId>) {}
    fn accepts_operation_metadata(_: Option<operation::CapabilityManifestV1>) {}

    accepts_application_envelope::<serde_json::Value>(None);
    accepts_api_envelope::<serde_json::Value>(None);
    accepts_domain_identity(None);
    accepts_operation_metadata(None);

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
fn work_inventory_exposes_every_mounted_route() {
    let registry = work::executable_binding_registry().expect("canonical Work registry");
    let attempt = registry
        .get(&operation::OperationId::new("operation.work.attempt_start").unwrap())
        .expect("attempt availability");

    assert!(matches!(
        attempt,
        operation::ExecutableBindingAvailabilityV1::Available { .. }
    ));

    let entries: Vec<_> = registry.iter().collect();
    assert!(!entries.is_empty());
    assert!(
        entries.iter().all(|availability| matches!(
            availability,
            operation::ExecutableBindingAvailabilityV1::Available { .. }
        )),
        "every mounted Work route must be an available executable binding"
    );
    let unique_ids: std::collections::BTreeSet<_> = entries
        .iter()
        .filter_map(|availability| match availability {
            operation::ExecutableBindingAvailabilityV1::Available { binding } => {
                Some(binding.operation_id().as_str())
            }
            operation::ExecutableBindingAvailabilityV1::Unavailable { .. } => None,
        })
        .collect();
    assert_eq!(unique_ids.len(), entries.len());
}

#[test]
fn work_attempt_finish_descriptor_matches_the_canonical_binding() {
    let registry = work::executable_binding_registry().expect("canonical Work registry");
    let availability = registry
        .get(&operation::OperationId::new(WorkAttemptFinish::OPERATION_ID).unwrap())
        .expect("attempt_finish availability");
    let binding = match availability {
        operation::ExecutableBindingAvailabilityV1::Available { binding } => binding,
        operation::ExecutableBindingAvailabilityV1::Unavailable { .. } => {
            panic!("attempt_finish must be an available binding")
        }
    };

    assert_eq!(
        binding.operation_id().as_str(),
        WorkAttemptFinish::OPERATION_ID
    );

    match binding.exposure() {
        operation::RouteExposureV1::Public {
            binding_id,
            route_path,
        } => {
            assert_eq!(binding_id.as_str(), WorkAttemptFinish::BINDING_ID);
            assert_eq!(route_path, WorkAttemptFinish::ROUTE);
        }
        operation::RouteExposureV1::Internal => panic!("attempt_finish must be publicly exposed"),
    }

    assert_eq!(
        binding.request_schema().schema_ref().schema_id().as_str(),
        "schema.work.attempt_finish.request"
    );
    assert_eq!(binding.request_schema().schema_ref().revision(), 1);
    assert_eq!(
        binding.result_schema().schema_ref().schema_id().as_str(),
        WorkAttemptFinish::RESULT_SCHEMA_ID
    );
    assert_eq!(
        binding.result_schema().schema_ref().revision(),
        WorkAttemptFinish::RESULT_SCHEMA_REVISION
    );
}

#[test]
fn generated_production_inventory_excludes_quarantined_multi_root_operations() {
    let production_inventory: Vec<_> = operations::base_operation_capabilities()
        .map(|capability| capability.operation.as_str())
        .collect();

    for operation in [
        "multi_root_scope_set_read",
        "multi_root_scope_set_compare_and_swap",
        "multi_root_execute",
    ] {
        assert!(
            !production_inventory.contains(&operation),
            "{operation} must remain absent from the generated production inventory"
        );
    }
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
    );

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
