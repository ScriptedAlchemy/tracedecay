mod common;

use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, CancellationContract, CancellationPoint,
    CapabilityManifestInputV1, DeadlineBehavior, DeadlineContract, DeniedDisclosurePolicy,
    EffectClass, IdempotencyContract, LifecycleClass, PrivacyClass, ReceiptContract,
    ReconciliationContract, RevalidationContract, RevalidationPoint, RoutingContractV1,
    ScopeDimension, ScopeRequirement, TerminalState, TerminalStateContract,
};

use common::{capability_id, profile_id, read_manifest, schema, use_case_id};

#[test]
fn manifest_serialization_preserves_stable_ids_and_contract_metadata() {
    let profile = profile_id("profile.default");
    let manifest = read_manifest(
        capability_id("capability.source.read"),
        use_case_id("use-case.source.read"),
        schema("schema.source.read.request"),
        schema("schema.source.read.result"),
        Vec::new(),
        vec![profile],
    );

    let serialized = serde_json::to_value(&manifest).unwrap();
    assert_eq!(serialized["capability_id"], "capability.source.read");
    assert_eq!(
        serialized["request_schema"]["schema_id"],
        "schema.source.read.request"
    );
    assert_eq!(
        serialized["result_schema"]["schema_id"],
        "schema.source.read.result"
    );
    assert_eq!(serialized["effect"], "read");
    assert_eq!(serialized["inverse"]["mode"], "not_applicable");
    assert_eq!(serialized["denied_disclosure"], "indistinguishable");
    assert_eq!(serialized["streaming"]["mode"], "unsupported");
    assert_eq!(serialized["cancellation"]["mode"], "cooperative");
    assert_eq!(
        serialized["cancellation"]["points"],
        serde_json::json!(["before_admission", "before_read", "during_read"])
    );
    assert_eq!(serialized["pagination"]["maximum_page_size"], 100);
}

#[test]
fn index_effects_require_effect_receipt_revalidation_and_cancellation_contracts() {
    let input = CapabilityManifestInputV1 {
        capability_id: capability_id("capability.git.stage-hunks"),
        use_case_id: use_case_id("use-case.git.stage-hunks"),
        routing: RoutingContractV1::new(
            1,
            "Stage selected hunks",
            "Stage only the exact previewed Git index hunks.",
            vec!["Stage these selected hunks".to_owned()],
        )
        .unwrap(),
        request_schema: schema("schema.git.stage.request"),
        result_schema: schema("schema.git.stage.result"),
        effect: EffectClass::GitIndexStage,
        scope: ScopeRequirement::new(vec![ScopeDimension::Project]).unwrap(),
        authority: AuthorityRequirement::CapabilityGrantWithRevalidation,
        denied_disclosure: DeniedDisclosurePolicy::Explicit,
        privacy: PrivacyClass::ScopedMetadata,
        lifecycle: LifecycleClass::Resumable,
        streaming: common::bounded_streaming_contract(),
        cancellation: CancellationContract::cooperative(vec![
            CancellationPoint::BeforeAdmission,
            CancellationPoint::BeforeEffect,
            CancellationPoint::EffectInFlight,
            CancellationPoint::Reconciling,
            CancellationPoint::AfterCommit,
        ])
        .unwrap(),
        deadline: DeadlineContract::new(30_000, DeadlineBehavior::ReturnEffectReceipt).unwrap(),
        pagination: None,
        idempotency: IdempotencyContract::Required,
        inverse: tracedecay_tool_catalog::InverseContract::Unavailable {
            reason: tracedecay_tool_catalog::InverseUnavailableReason::NoShippedInverse,
        },
        authority_revalidation: RevalidationContract::required(vec![
            RevalidationPoint::Authority,
            RevalidationPoint::Scope,
            RevalidationPoint::Policy,
            RevalidationPoint::Configuration,
            RevalidationPoint::ExpectedState,
        ])
        .unwrap(),
        reconciliation: ReconciliationContract::Required,
        receipt: ReceiptContract::DurableEffect,
        terminal_states: TerminalStateContract::new(vec![
            TerminalState::Completed,
            TerminalState::Cancelled,
            TerminalState::TimedOut,
            TerminalState::Failed,
            TerminalState::EffectUnknown,
            TerminalState::Partial,
        ])
        .unwrap(),
        availability: AvailabilityContract::Available,
        binding_ids: Vec::new(),
        profile_eligibility: Vec::new(),
        required_features: Vec::new(),
    };

    let manifest = tracedecay_tool_catalog::CapabilityManifestV1::new(input.clone()).unwrap();
    let serialized = serde_json::to_value(manifest).unwrap();
    assert_eq!(serialized["effect"], "git_index_stage");
    assert_eq!(serialized["streaming"]["mode"], "bounded");
    assert_eq!(serialized["streaming"]["resume"], "resumable");
    assert_eq!(
        serialized["cancellation"]["points"],
        serde_json::json!([
            "before_admission",
            "before_effect",
            "effect_in_flight",
            "reconciling",
            "after_commit"
        ])
    );
    assert_eq!(serialized["deadline"]["behavior"], "return_effect_receipt");
    assert_eq!(serialized["idempotency"], "required");
    assert_eq!(serialized["inverse"]["mode"], "unavailable");
    assert_eq!(serialized["inverse"]["reason"], "no_shipped_inverse");
    assert_eq!(serialized["receipt"], "durable_effect");

    let mut invalid = input.clone();
    invalid.receipt = ReceiptContract::Operation;
    assert!(tracedecay_tool_catalog::CapabilityManifestV1::new(invalid).is_err());

    let mut missing_inverse_contract = input;
    missing_inverse_contract.inverse = tracedecay_tool_catalog::InverseContract::NotApplicable;
    assert!(tracedecay_tool_catalog::CapabilityManifestV1::new(missing_inverse_contract).is_err());
}

#[test]
fn availability_is_callable_only_for_available_entries() {
    assert!(AvailabilityContract::Available.is_callable());
    assert!(
        !AvailabilityContract::Unavailable {
            reason: tracedecay_tool_catalog::UnavailabilityReason::NotImplemented,
        }
        .is_callable()
    );
}
