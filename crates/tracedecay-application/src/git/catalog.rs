use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, CancellationContract, CancellationPoint,
    CapabilityId, CapabilityManifestInputV1, CapabilityManifestV1, CatalogContributionInputV1,
    CatalogContributionV1, ContributionId, DeadlineBehavior, DeadlineContract,
    DeniedDisclosurePolicy, IdempotencyContract, LifecycleClass, PaginationContract, PrivacyClass,
    ReceiptContract, ReconciliationContract, RevalidationContract, RevalidationPoint,
    RoutingContractV1, SchemaId, SchemaRef, ScopeDimension, ScopeRequirement, StreamResumeContract,
    StreamingContract, TerminalState, TerminalStateContract, UnavailabilityReason, UseCaseId,
};

use crate::error::ApplicationContractError;
use crate::handlers::{ApplicationHandlerDescriptor, ApplicationOperation};
use crate::result::ResultContractRef;

use super::transactions::{git_index_effect_class, git_index_operation_ids};

struct GitIndexCatalogSpec {
    operation: tracedecay_domain::GitIndexTransactionOperationV1,
    request_schema: &'static str,
    result_schema: &'static str,
    summary: &'static str,
    description: &'static str,
    example: &'static str,
}

const GIT_INDEX_SPECS: [GitIndexCatalogSpec; 3] = [
    GitIndexCatalogSpec {
        operation: tracedecay_domain::GitIndexTransactionOperationV1::StageHunks,
        request_schema: "schema.application.git.stage-hunks.request",
        result_schema: "schema.application.git.stage-hunks.result",
        summary: "Stage selected hunks",
        description: "Stage only exact preview-bound Git index hunks.",
        example: "Stage these selected hunks",
    },
    GitIndexCatalogSpec {
        operation: tracedecay_domain::GitIndexTransactionOperationV1::UnstageHunks,
        request_schema: "schema.application.git.unstage-hunks.request",
        result_schema: "schema.application.git.unstage-hunks.result",
        summary: "Unstage selected hunks",
        description: "Unstage only exact preview-bound Git index hunks.",
        example: "Unstage these selected hunks",
    },
    GitIndexCatalogSpec {
        operation: tracedecay_domain::GitIndexTransactionOperationV1::CommitIndex,
        request_schema: "schema.application.git.commit-index.request",
        result_schema: "schema.application.git.commit-index.result",
        summary: "Commit the index",
        description: "Commit the exact previewed index tree with fixed safeguards.",
        example: "Commit the previewed index",
    },
];

pub fn git_index_catalog_contribution() -> Result<CatalogContributionV1, ApplicationContractError> {
    let capabilities = GIT_INDEX_SPECS
        .iter()
        .map(capability)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.application.git-index-transactions")?,
        depends_on: Vec::new(),
        capabilities,
        retrieval_primitives: Vec::new(),
        bindings: Vec::new(),
    })?)
}

pub fn git_index_handler_descriptors()
-> Result<Vec<ApplicationHandlerDescriptor>, ApplicationContractError> {
    GIT_INDEX_SPECS.iter().map(handler_descriptor).collect()
}

fn capability(
    spec: &GitIndexCatalogSpec,
) -> Result<CapabilityManifestV1, ApplicationContractError> {
    let request_schema = request_schema(spec)?;
    let result_schema = result_schema(spec)?;
    let (capability, use_case) = git_index_operation_ids(spec.operation);
    Ok(CapabilityManifestV1::new(CapabilityManifestInputV1 {
        capability_id: CapabilityId::new(capability)?,
        use_case_id: UseCaseId::new(use_case)?,
        routing: RoutingContractV1::new(
            1,
            spec.summary,
            spec.description,
            vec![spec.example.to_owned()],
        )?,
        request_schema,
        result_schema,
        effect: git_index_effect_class(spec.operation),
        scope: git_index_scope()?,
        authority: AuthorityRequirement::CapabilityGrantWithRevalidation,
        denied_disclosure: DeniedDisclosurePolicy::Explicit,
        privacy: PrivacyClass::ScopedMetadata,
        lifecycle: LifecycleClass::Resumable,
        streaming: StreamingContract::bounded(8, 16_384, StreamResumeContract::Resumable)?,
        cancellation: CancellationContract::cooperative(vec![
            CancellationPoint::BeforeAdmission,
            CancellationPoint::BeforeEffect,
            CancellationPoint::EffectInFlight,
            CancellationPoint::Reconciling,
            CancellationPoint::AfterCommit,
        ])?,
        deadline: DeadlineContract::new(30_000, DeadlineBehavior::ReturnEffectReceipt)?,
        pagination: None::<PaginationContract>,
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
        ])?,
        reconciliation: ReconciliationContract::Required,
        receipt: ReceiptContract::DurableEffect,
        terminal_states: TerminalStateContract::new(vec![
            TerminalState::Completed,
            TerminalState::Cancelled,
            TerminalState::TimedOut,
            TerminalState::Failed,
            TerminalState::EffectUnknown,
            TerminalState::Partial,
        ])?,
        // Stage, unstage, and commit are fully shipped: the daemon's native Git
        // index transactions implement all three, and callers reach them by
        // naming the operation on `git_preview`/`git_apply`, which own the
        // transport surface. Labelling them `NotImplemented` was false. They
        // stay non-callable as direct catalog routes -- and stay registered so
        // a direct route resolves to a typed unavailable decision rather than
        // an unknown capability -- but the reason now says why.
        availability: AvailabilityContract::Unavailable {
            reason: UnavailabilityReason::ReachedThroughAnotherCapability,
        },
        binding_ids: Vec::new(),
        profile_eligibility: Vec::new(),
        required_features: Vec::new(),
    })?)
}

fn handler_descriptor(
    spec: &GitIndexCatalogSpec,
) -> Result<ApplicationHandlerDescriptor, ApplicationContractError> {
    let result_schema = result_schema(spec)?;
    let (capability, use_case) = git_index_operation_ids(spec.operation);
    ApplicationHandlerDescriptor::new(
        ApplicationOperation::new(
            CapabilityId::new(capability)?,
            UseCaseId::new(use_case)?,
            ResultContractRef::from_schema(&result_schema),
            true,
        ),
        request_schema(spec)?,
        result_schema,
    )
}

fn request_schema(spec: &GitIndexCatalogSpec) -> Result<SchemaRef, ApplicationContractError> {
    Ok(SchemaRef::new(SchemaId::new(spec.request_schema)?, 1)?)
}

fn result_schema(spec: &GitIndexCatalogSpec) -> Result<SchemaRef, ApplicationContractError> {
    Ok(SchemaRef::new(SchemaId::new(spec.result_schema)?, 1)?)
}

fn git_index_scope() -> Result<ScopeRequirement, ApplicationContractError> {
    Ok(ScopeRequirement::new(vec![
        ScopeDimension::Project,
        ScopeDimension::Repository,
        ScopeDimension::Worktree,
    ])?)
}
