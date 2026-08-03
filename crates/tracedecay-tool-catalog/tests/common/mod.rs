#![allow(dead_code)]

use tracedecay_tool_catalog::{
    ApplicationHandlerDescriptorV1, AuthorityRequirement, AvailabilityContract, BindingId,
    CancellationContract, CancellationPoint, CapabilityId, CapabilityManifestInputV1,
    CapabilityManifestV1, DeadlineBehavior, DeadlineContract, DeniedDisclosurePolicy, EffectClass,
    FeatureId, IdempotencyContract, LifecycleClass, PaginationContract, PrivacyClass,
    ProfileBudget, ProfileDefinition, ProfileDefinitionInputV1, ProfileId, ProfileKind,
    ReceiptContract, ReconciliationContract, RevalidationContract, RevalidationPoint,
    RoutingContractV1, RoutingFixtureExpectation, RoutingFixtureV1, SchemaId, SchemaRef,
    ScopeDimension, ScopeRequirement, StreamResumeContract, StreamingContract, TerminalState,
    TerminalStateContract, UseCaseId,
};

pub fn capability_id(value: &str) -> CapabilityId {
    CapabilityId::new(value).unwrap()
}

pub fn use_case_id(value: &str) -> UseCaseId {
    UseCaseId::new(value).unwrap()
}

pub fn profile_id(value: &str) -> ProfileId {
    ProfileId::new(value).unwrap()
}

/// A ceiling generous enough that tests which are not exercising the budget
/// never trip it.
pub fn ample_budget() -> ProfileBudget {
    ProfileBudget::new(64, 12_000).unwrap()
}

pub fn schema(name: &str) -> SchemaRef {
    SchemaRef::new(SchemaId::new(name).unwrap(), 1).unwrap()
}

pub fn read_manifest(
    capability_id: CapabilityId,
    use_case_id: UseCaseId,
    request_schema: SchemaRef,
    result_schema: SchemaRef,
    binding_ids: Vec<BindingId>,
    profile_eligibility: Vec<ProfileId>,
) -> CapabilityManifestV1 {
    CapabilityManifestV1::new(CapabilityManifestInputV1 {
        capability_id,
        use_case_id,
        routing: RoutingContractV1::new(
            1,
            "Read source",
            "Read bounded source evidence without applying an effect.",
            vec!["Show a bounded source excerpt".to_owned()],
        )
        .unwrap(),
        request_schema,
        result_schema,
        effect: EffectClass::Read,
        scope: ScopeRequirement::new(vec![ScopeDimension::Project, ScopeDimension::Resource])
            .unwrap(),
        authority: AuthorityRequirement::CapabilityGrantWithRevalidation,
        denied_disclosure: DeniedDisclosurePolicy::Indistinguishable,
        privacy: PrivacyClass::ScopedMetadata,
        lifecycle: LifecycleClass::Stateless,
        streaming: StreamingContract::Unsupported,
        cancellation: CancellationContract::cooperative(vec![
            CancellationPoint::BeforeAdmission,
            CancellationPoint::BeforeRead,
            CancellationPoint::DuringRead,
        ])
        .unwrap(),
        deadline: DeadlineContract::new(1_000, DeadlineBehavior::ReturnOperationReceipt).unwrap(),
        pagination: Some(PaginationContract::new(10, 100, 60_000).unwrap()),
        idempotency: IdempotencyContract::NotRequired,
        inverse: tracedecay_tool_catalog::InverseContract::NotApplicable,
        authority_revalidation: RevalidationContract::required(vec![
            RevalidationPoint::Authority,
            RevalidationPoint::Scope,
        ])
        .unwrap(),
        reconciliation: ReconciliationContract::NotRequired,
        receipt: ReceiptContract::Operation,
        terminal_states: TerminalStateContract::new(vec![
            TerminalState::Completed,
            TerminalState::Cancelled,
            TerminalState::TimedOut,
            TerminalState::Failed,
            TerminalState::Partial,
        ])
        .unwrap(),
        availability: AvailabilityContract::Available,
        binding_ids,
        profile_eligibility,
        required_features: Vec::<FeatureId>::new(),
    })
    .unwrap()
}

pub fn handler_for(manifest: &CapabilityManifestV1) -> ApplicationHandlerDescriptorV1 {
    ApplicationHandlerDescriptorV1::new(
        manifest.capability_id().clone(),
        manifest.use_case_id().clone(),
        manifest.request_schema().clone(),
        manifest.result_schema().clone(),
    )
}

pub fn profile(
    profile_id: ProfileId,
    capability_ids: Vec<CapabilityId>,
    budget: ProfileBudget,
) -> ProfileDefinition {
    let mut routing_fixtures = capability_ids
        .iter()
        .cloned()
        .map(|capability_id| {
            RoutingFixtureV1::new(
                format!("select {capability_id}"),
                RoutingFixtureExpectation::Select { capability_id },
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    routing_fixtures
        .push(RoutingFixtureV1::new("do nothing", RoutingFixtureExpectation::Reject).unwrap());
    if capability_ids.len() > 1 {
        routing_fixtures.push(
            RoutingFixtureV1::new(
                "this is intentionally ambiguous",
                RoutingFixtureExpectation::ambiguous(capability_ids.clone()).unwrap(),
            )
            .unwrap(),
        );
    }

    ProfileDefinition::new(ProfileDefinitionInputV1 {
        profile_id,
        kind: ProfileKind::Default,
        capability_ids,
        enabled_surfaces: Vec::new(),
        requires_cli_mcp_pairing: false,
        budget,
        routing_fixtures,
    })
    .unwrap()
}

pub fn bounded_streaming_contract() -> StreamingContract {
    StreamingContract::bounded(16, 8_192, StreamResumeContract::Resumable).unwrap()
}
