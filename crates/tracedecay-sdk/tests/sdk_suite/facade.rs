use tracedecay_sdk::operations::UNAVAILABLE_OPERATIONS;
use tracedecay_sdk::{
    CancellationContext, CancellationSignal, CancellationState, CancellationTokenId, contracts,
    domain, operation,
};

#[test]
fn cancellation_types_are_the_canonical_application_types() {
    let signal = CancellationSignal::active("cancel.sdk.facade").expect("cancellation signal");
    let canonical_signal: contracts::CancellationSignal = signal.clone();
    let _: CancellationSignal = canonical_signal;

    assert!(signal.cancel(domain::UtcMicros(41)));
    let context: CancellationContext = signal.context();
    let token: CancellationTokenId = context.token_id.clone();
    let _: contracts::CancellationTokenId = token;
    assert!(matches!(
        context.state,
        CancellationState::Cancelled {
            requested_at: domain::UtcMicros(41)
        }
    ));
}
#[test]
fn generated_unavailable_operations_match_the_canonical_sdk_registry() {
    let registry = contracts::sdk_executable_binding_registry().expect("canonical SDK registry");
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
    let envelope = contracts::ApplicationProblemEnvelope::new(
        contracts::ResultContractRef::new(
            operation::SchemaId::new("schema.sdk.problem").expect("schema id"),
            1,
        )
        .expect("result contract"),
        contracts::RequestId::new("request.sdk.problem").expect("request id"),
        contracts::ApplicationProblem::unavailable(
            contracts::SafeDiagnostic::new(
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
    let receipt = contracts::OperationReceipt::completed(
        domain::UtcMicros(10),
        domain::UtcMicros(20),
        contracts::Deadline::new(domain::UtcMicros(30)).expect("deadline"),
        contracts::OperationBudgetUsage {
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

    let decoded: contracts::OperationReceipt =
        serde_json::from_value(value).expect("deserialize receipt");
    assert_eq!(decoded, receipt);

    let canonical: tracedecay_contracts::OperationReceipt = decoded;
    let _: contracts::OperationReceipt = canonical;
}
