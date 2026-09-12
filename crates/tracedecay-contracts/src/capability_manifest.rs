use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, BindingId, CancellationContract, CapabilityId,
    CapabilityManifestInputV1, CapabilityManifestV1, CatalogValidationError, DeadlineContract,
    DeniedDisclosurePolicy, EffectClass, FeatureId, IdempotencyContract, InverseContract,
    InverseUnavailableReason, LifecycleClass, PaginationContract, PrivacyClass, ProfileId,
    ReceiptContract, ReconciliationContract, RevalidationContract, RoutingContractV1, SchemaRef,
    ScopeRequirement, StreamingContract, TerminalStateContract, UseCaseId,
};

pub(crate) struct ApplicationCapabilityManifestInput {
    pub capability_id: CapabilityId,
    pub use_case_id: UseCaseId,
    pub routing: RoutingContractV1,
    pub request_schema: SchemaRef,
    pub result_schema: SchemaRef,
    pub effect: EffectClass,
    pub scope: ScopeRequirement,
    pub denied_disclosure: DeniedDisclosurePolicy,
    pub privacy: PrivacyClass,
    pub lifecycle: LifecycleClass,
    pub streaming: StreamingContract,
    pub cancellation: CancellationContract,
    pub deadline: DeadlineContract,
    pub pagination: Option<PaginationContract>,
    pub inverse: Option<InverseContract>,
    pub authority_revalidation: RevalidationContract,
    pub terminal_states: TerminalStateContract,
    pub availability: AvailabilityContract,
    pub binding_ids: Vec<BindingId>,
    pub profile_eligibility: Vec<ProfileId>,
    pub required_features: Vec<FeatureId>,
}

pub(crate) fn application_capability_manifest(
    input: ApplicationCapabilityManifestInput,
) -> Result<CapabilityManifestV1, CatalogValidationError> {
    let is_effect = input.effect.is_effect();
    CapabilityManifestV1::new(CapabilityManifestInputV1 {
        capability_id: input.capability_id,
        use_case_id: input.use_case_id,
        routing: input.routing,
        request_schema: input.request_schema,
        result_schema: input.result_schema,
        effect: input.effect,
        scope: input.scope,
        authority: AuthorityRequirement::CapabilityGrantWithRevalidation,
        denied_disclosure: input.denied_disclosure,
        privacy: input.privacy,
        lifecycle: input.lifecycle,
        streaming: input.streaming,
        cancellation: input.cancellation,
        deadline: input.deadline,
        pagination: input.pagination,
        idempotency: if is_effect {
            IdempotencyContract::Required
        } else {
            IdempotencyContract::NotRequired
        },
        inverse: input.inverse.unwrap_or({
            if is_effect {
                InverseContract::Unavailable {
                    reason: InverseUnavailableReason::NoShippedInverse,
                }
            } else {
                InverseContract::NotApplicable
            }
        }),
        authority_revalidation: input.authority_revalidation,
        reconciliation: if is_effect {
            ReconciliationContract::Required
        } else {
            ReconciliationContract::NotRequired
        },
        receipt: if is_effect {
            ReceiptContract::DurableEffect
        } else {
            ReceiptContract::Operation
        },
        terminal_states: input.terminal_states,
        availability: input.availability,
        binding_ids: input.binding_ids,
        profile_eligibility: input.profile_eligibility,
        required_features: input.required_features,
    })
}
