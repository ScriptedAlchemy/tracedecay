use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, BindingId, BindingStatus, BindingSurface,
    CancellationContract, CancellationPoint, CapabilityId, CapabilityManifestInputV1,
    CapabilityManifestV1, CatalogContributionInputV1, CatalogContributionV1, ContributionId,
    DeadlineBehavior, DeadlineContract, DeniedDisclosurePolicy, EffectClass, FeatureId,
    IdempotencyContract, LifecycleClass, PaginationContract, PrivacyClass, ProtocolRevisionRange,
    ReceiptContract, ReconciliationContract, RevalidationContract, RevalidationPoint,
    RoutingContractV1, SchemaId, SchemaRef, ScopeDimension, ScopeRequirement, StreamingContract,
    SurfaceBindingInputV1, SurfaceBindingV1, SurfaceOperationName, TerminalState,
    TerminalStateContract, UseCaseId,
};

use crate::error::ApplicationContractError;
use crate::handlers::{ApplicationHandlerDescriptor, ApplicationOperation};
use crate::result::ResultContractRef;
use crate::retrieval::catalog::{
    APPLICATION_COMPACT_PROFILE_ID, APPLICATION_HOST_LIMITED_PROFILE_ID, application_profile_ids,
};

const CONTEXT_FEATURE: &str = "feature.lsp.tracedecay-context.v1";

struct LspContextSpec {
    suffix: &'static str,
    method: &'static str,
    summary: &'static str,
    description: &'static str,
    profiles: &'static [&'static str],
    paginated: bool,
}

const LSP_CONTEXT_SPECS: [LspContextSpec; 2] = [
    LspContextSpec {
        suffix: "context",
        method: "tracedecay/context",
        summary: "Read TraceDecay LSP context",
        description: "Read the negotiated bounded diagnostics, impact, affected-test, and test-result projection.",
        profiles: &[
            APPLICATION_COMPACT_PROFILE_ID,
            APPLICATION_HOST_LIMITED_PROFILE_ID,
        ],
        paginated: false,
    },
    LspContextSpec {
        suffix: "context-expand",
        method: "tracedecay/context/expand",
        summary: "Expand TraceDecay LSP context",
        description: "Reauthorize and expand one opaque omission handle from the negotiated TraceDecay context projection.",
        profiles: &[APPLICATION_COMPACT_PROFILE_ID],
        paginated: true,
    },
];

pub fn lsp_context_catalog_contribution() -> Result<CatalogContributionV1, ApplicationContractError>
{
    let mut capabilities = Vec::with_capacity(LSP_CONTEXT_SPECS.len());
    let mut bindings = Vec::with_capacity(LSP_CONTEXT_SPECS.len());
    for spec in &LSP_CONTEXT_SPECS {
        let capability_id = capability_id(spec)?;
        let binding_id = BindingId::new(format!("binding.lsp.{}.v1", spec.suffix))?;
        let feature = FeatureId::new(CONTEXT_FEATURE)?;
        bindings.push(SurfaceBindingV1::new(SurfaceBindingInputV1 {
            binding_id: binding_id.clone(),
            capability_id: capability_id.clone(),
            surface: BindingSurface::Lsp,
            operation: SurfaceOperationName::new(spec.method)?,
            protocol_revisions: ProtocolRevisionRange::new(1, 1)?,
            required_features: vec![feature.clone()],
            status: BindingStatus::Current,
            alias_of: None,
        })?);
        capabilities.push(CapabilityManifestV1::new(CapabilityManifestInputV1 {
            capability_id,
            use_case_id: use_case_id(spec)?,
            routing: RoutingContractV1::new(
                1,
                spec.summary,
                spec.description,
                vec![spec.summary.to_owned()],
            )?,
            request_schema: schema(spec, "request")?,
            result_schema: schema(spec, "result")?,
            effect: EffectClass::Read,
            scope: ScopeRequirement::new(vec![
                ScopeDimension::Project,
                ScopeDimension::Repository,
                ScopeDimension::Worktree,
                ScopeDimension::Resource,
            ])?,
            authority: AuthorityRequirement::CapabilityGrantWithRevalidation,
            denied_disclosure: DeniedDisclosurePolicy::Indistinguishable,
            privacy: PrivacyClass::ScopedMetadata,
            lifecycle: LifecycleClass::SessionStateful,
            streaming: StreamingContract::Unsupported,
            cancellation: CancellationContract::cooperative(vec![
                CancellationPoint::BeforeAdmission,
                CancellationPoint::BeforeRead,
                CancellationPoint::DuringRead,
            ])?,
            deadline: DeadlineContract::new(10_000, DeadlineBehavior::ReturnOperationReceipt)?,
            pagination: spec
                .paginated
                .then(|| PaginationContract::new(10, 100, 60_000))
                .transpose()?,
            idempotency: IdempotencyContract::NotRequired,
            inverse: tracedecay_tool_catalog::InverseContract::NotApplicable,
            authority_revalidation: RevalidationContract::required(vec![
                RevalidationPoint::Authority,
                RevalidationPoint::Scope,
                RevalidationPoint::Policy,
                RevalidationPoint::Configuration,
                RevalidationPoint::ExpectedState,
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
            availability: AvailabilityContract::Available,
            binding_ids: vec![binding_id],
            profile_eligibility: application_profile_ids(spec.profiles)?,
            required_features: vec![feature],
        })?);
    }
    Ok(CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.application.lsp-context")?,
        depends_on: Vec::new(),
        capabilities,
        retrieval_primitives: Vec::new(),
        bindings,
    })?)
}

pub fn lsp_context_handler_descriptors()
-> Result<Vec<ApplicationHandlerDescriptor>, ApplicationContractError> {
    LSP_CONTEXT_SPECS
        .iter()
        .map(|spec| {
            let result = schema(spec, "result")?;
            ApplicationHandlerDescriptor::new(
                ApplicationOperation::new(
                    capability_id(spec)?,
                    use_case_id(spec)?,
                    ResultContractRef::from_schema(&result),
                    true,
                ),
                schema(spec, "request")?,
                result,
            )
        })
        .collect()
}

fn capability_id(spec: &LspContextSpec) -> Result<CapabilityId, ApplicationContractError> {
    Ok(CapabilityId::new(format!(
        "capability.application.lsp.{}",
        spec.suffix
    ))?)
}

fn use_case_id(spec: &LspContextSpec) -> Result<UseCaseId, ApplicationContractError> {
    Ok(UseCaseId::new(format!(
        "use-case.application.lsp.{}",
        spec.suffix
    ))?)
}

fn schema(spec: &LspContextSpec, direction: &str) -> Result<SchemaRef, ApplicationContractError> {
    Ok(SchemaRef::new(
        SchemaId::new(format!(
            "schema.application.lsp.{}.{}",
            spec.suffix, direction
        ))?,
        1,
    )?)
}
