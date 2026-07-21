use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, CancellationContract, CancellationPoint,
    CapabilityId, CapabilityManifestInputV1, CapabilityManifestV1, CatalogContributionInputV1,
    CatalogContributionV1, ContributionContractRef, ContributionId, CoverageContractRef,
    DeadlineBehavior, DeadlineContract, DeniedDisclosurePolicy, EffectClass, IdempotencyContract,
    LifecycleClass, OmissionContractRef, PaginationContract, PrivacyClass, ProfileId,
    ReceiptContract, ReconciliationContract, RetrievalFamily, RetrievalPrimitiveManifestInputV1,
    RetrievalPrimitiveManifestV1, RetrieverId, RevalidationContract, RevalidationPoint,
    RoutingContractV1, SchemaId, SchemaRef, ScopeDimension, ScopeRequirement, ScoringContractRef,
    SortContract, SortContractId, StreamingContract, TemporalMode, TerminalState,
    TerminalStateContract, UnavailabilityReason,
};

use crate::error::ApplicationContractError;
use crate::handlers::{ApplicationHandlerDescriptor, ApplicationOperation};
use crate::result::ResultContractRef;

const SYMBOL_SEARCH_CAPABILITY: &str = "capability.retrieval.symbol-search";
const SYMBOL_SEARCH_USE_CASE: &str = "use-case.retrieval.symbol-search";
pub const APPLICATION_DEFAULT_PROFILE_ID: &str = "profile.default";

/// Closed set of inert catalog contributions for declared application use
/// cases. Adding metadata here requires adding its validation-only typed
/// handler descriptor to [`crate::application_handler_descriptors`].
pub fn application_catalog_contributions()
-> Result<Vec<CatalogContributionV1>, ApplicationContractError> {
    Ok(vec![
        symbol_search_contribution()?,
        crate::git::git_index_catalog_contribution()?,
    ])
}

pub fn symbol_search_request_schema() -> Result<SchemaRef, ApplicationContractError> {
    Ok(SchemaRef::new(
        SchemaId::new("schema.application.symbol-search.request")?,
        1,
        384,
    )?)
}

pub fn symbol_search_result_schema() -> Result<SchemaRef, ApplicationContractError> {
    Ok(SchemaRef::new(
        SchemaId::new("schema.application.symbol-search.result")?,
        1,
        1_024,
    )?)
}

pub fn symbol_search_operation() -> Result<ApplicationOperation, ApplicationContractError> {
    let result_schema = symbol_search_result_schema()?;
    Ok(ApplicationOperation::new(
        CapabilityId::new(SYMBOL_SEARCH_CAPABILITY)?,
        tracedecay_tool_catalog::UseCaseId::new(SYMBOL_SEARCH_USE_CASE)?,
        ResultContractRef::from_schema(&result_schema),
        true,
    ))
}

pub fn symbol_search_handler_descriptor()
-> Result<ApplicationHandlerDescriptor, ApplicationContractError> {
    ApplicationHandlerDescriptor::new(
        symbol_search_operation()?,
        symbol_search_request_schema()?,
        symbol_search_result_schema()?,
    )
}

/// Inert catalog contribution for the declared symbol-search use case.
///
/// Root composition remains outside this crate; this function has no dispatch,
/// binding, profile, storage, or transport side effect.
pub fn symbol_search_contribution() -> Result<CatalogContributionV1, ApplicationContractError> {
    let capability_id = CapabilityId::new(SYMBOL_SEARCH_CAPABILITY)?;
    let request_schema = symbol_search_request_schema()?;
    let result_schema = symbol_search_result_schema()?;
    let capability = CapabilityManifestV1::new(CapabilityManifestInputV1 {
        capability_id: capability_id.clone(),
        use_case_id: tracedecay_tool_catalog::UseCaseId::new(SYMBOL_SEARCH_USE_CASE)?,
        routing: RoutingContractV1::new(
            1,
            "Search symbols",
            "Search the admitted single-root PR9 symbol evidence.",
            vec!["Find this symbol".to_owned()],
        )?,
        request_schema: request_schema.clone(),
        result_schema: result_schema.clone(),
        effect: EffectClass::Read,
        scope: symbol_search_scope()?,
        authority: AuthorityRequirement::CapabilityGrantWithRevalidation,
        denied_disclosure: DeniedDisclosurePolicy::Indistinguishable,
        privacy: PrivacyClass::ScopedMetadata,
        lifecycle: LifecycleClass::Resumable,
        streaming: StreamingContract::Unsupported,
        cancellation: CancellationContract::cooperative(vec![
            CancellationPoint::BeforeAdmission,
            CancellationPoint::BeforeRead,
            CancellationPoint::DuringRead,
        ])?,
        deadline: DeadlineContract::new(10_000, DeadlineBehavior::ReturnOperationReceipt)?,
        pagination: Some(PaginationContract::new(10, 100, 60_000)?),
        idempotency: IdempotencyContract::NotRequired,
        authority_revalidation: RevalidationContract::required(vec![
            RevalidationPoint::Authority,
            RevalidationPoint::Scope,
            RevalidationPoint::Policy,
            RevalidationPoint::Configuration,
        ])?,
        reconciliation: ReconciliationContract::NotRequired,
        receipt: ReceiptContract::Operation,
        terminal_states: TerminalStateContract::new(vec![
            TerminalState::Completed,
            TerminalState::Cancelled,
            TerminalState::TimedOut,
            TerminalState::Failed,
            TerminalState::Partial,
        ])?,
        availability: AvailabilityContract::Unavailable {
            reason: UnavailabilityReason::NotImplemented,
        },
        binding_ids: Vec::new(),
        profile_eligibility: vec![ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID)?],
        required_features: Vec::new(),
    })?;
    let primitive = RetrievalPrimitiveManifestV1::new(RetrievalPrimitiveManifestInputV1 {
        capability_id,
        family: RetrievalFamily::Symbol,
        retriever_id: RetrieverId::new("retriever.application.symbol-search")?,
        request_schema,
        evidence_packet_schema: result_schema,
        coverage_contract: CoverageContractRef::new(SchemaRef::new(
            SchemaId::new("schema.application.evidence-coverage")?,
            1,
            512,
        )?),
        omission_contract: OmissionContractRef::new(SchemaRef::new(
            SchemaId::new("schema.application.evidence-omission")?,
            1,
            256,
        )?),
        scoring_contract: ScoringContractRef::new(SchemaRef::new(
            SchemaId::new("schema.application.evidence-score")?,
            1,
            384,
        )?),
        contribution_contract: ContributionContractRef::new(SchemaRef::new(
            SchemaId::new("schema.application.retriever-contribution")?,
            1,
            640,
        )?),
        deterministic_order: SortContract::new(
            SortContractId::new("sort.application.symbol-search.v1")?,
            1,
        )?,
        default_page_size: 10,
        maximum_page_size: 100,
        temporal_modes: vec![TemporalMode::Current, TemporalMode::AsOf],
        cancellation_points: vec![
            CancellationPoint::BeforeAdmission,
            CancellationPoint::BeforeRead,
            CancellationPoint::DuringRead,
        ],
        deadline_behavior: DeadlineBehavior::ReturnOperationReceipt,
    })?;
    Ok(CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.application.symbol-search")?,
        depends_on: Vec::new(),
        capabilities: vec![capability],
        retrieval_primitives: vec![primitive],
        bindings: Vec::new(),
    })?)
}

fn symbol_search_scope() -> Result<ScopeRequirement, ApplicationContractError> {
    Ok(ScopeRequirement::new(vec![
        ScopeDimension::Project,
        ScopeDimension::Repository,
        ScopeDimension::Worktree,
        ScopeDimension::Resource,
    ])?)
}
