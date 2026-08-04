use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, BindingId, BindingStatus, BindingSurface,
    CancellationContract, CancellationPoint, CapabilityId, CapabilityManifestInputV1,
    CapabilityManifestV1, CatalogContributionInputV1, CatalogContributionV1,
    ContributionContractRef, ContributionId, CoverageContractRef, DeadlineBehavior,
    DeadlineContract, DeniedDisclosurePolicy, EffectClass, IdempotencyContract, LifecycleClass,
    OmissionContractRef, PaginationContract, PrivacyClass, ProfileId, ProtocolRevisionRange,
    ReceiptContract, ReconciliationContract, RetrievalFamily, RetrievalPrimitiveManifestInputV1,
    RetrievalPrimitiveManifestV1, RetrieverId, RevalidationContract, RevalidationPoint,
    RoutingContractV1, SchemaId, SchemaRef, ScopeDimension, ScopeRequirement, ScoringContractRef,
    SortContract, SortContractId, StreamingContract, SurfaceBindingInputV1, SurfaceBindingV1,
    SurfaceOperationName, TemporalMode, TerminalState, TerminalStateContract,
};

use crate::current_bindings;
use crate::error::ApplicationContractError;
use crate::handlers::{ApplicationHandlerDescriptor, ApplicationOperation};
use crate::result::ResultContractRef;

const SYMBOL_SEARCH_CAPABILITY: &str = "capability.retrieval.symbol-search";
const SYMBOL_SEARCH_USE_CASE: &str = "use-case.retrieval.symbol-search";
pub const APPLICATION_DEFAULT_PROFILE_ID: &str = "profile.default";
pub const APPLICATION_COMPACT_PROFILE_ID: &str = "profile.compact";
pub const APPLICATION_ADMINISTRATIVE_PROFILE_ID: &str = "profile.administrative";
pub const APPLICATION_HOST_LIMITED_PROFILE_ID: &str = "profile.host-limited";

pub(crate) fn application_profile_ids(
    profile_ids: &[&str],
) -> Result<Vec<ProfileId>, ApplicationContractError> {
    profile_ids
        .iter()
        .map(|profile_id| ProfileId::new(*profile_id).map_err(Into::into))
        .collect()
}

/// Closed set of catalog contributions for declared application use cases.
/// Adding metadata here requires adding its typed handler descriptor to
/// [`crate::application_handler_descriptors`].
pub fn application_catalog_contributions()
-> Result<Vec<CatalogContributionV1>, ApplicationContractError> {
    Ok(vec![
        symbol_search_contribution()?,
        primitive_read_contribution()?,
        super::callable_code_catalog_contribution()?,
        crate::git::git_index_catalog_contribution()?,
        crate::git::git_surface_catalog_contribution()?,
        crate::configuration::configuration_surface_catalog_contribution()?,
        crate::context_scout::context_scout_surface_catalog_contribution()?,
        crate::feedback::feedback_surface_catalog_contribution()?,
        crate::lsp_context_catalog::lsp_context_catalog_contribution()?,
        crate::retained_surfaces::retained_surface_catalog_contribution()?,
        crate::source_edit::source_edit_catalog_contribution()?,
        crate::api_migration::api_migration_catalog_contribution()?,
    ])
}

struct PrimitiveReadSpec {
    operation: &'static str,
    capability: &'static str,
    use_case: &'static str,
}

fn primitive_profile_ids(operation: &str) -> &'static [&'static str] {
    match operation {
        "source_lines" => &[
            APPLICATION_DEFAULT_PROFILE_ID,
            APPLICATION_COMPACT_PROFILE_ID,
            APPLICATION_HOST_LIMITED_PROFILE_ID,
        ],
        "source_outline" | "diagnostics_read" => &[
            APPLICATION_DEFAULT_PROFILE_ID,
            APPLICATION_COMPACT_PROFILE_ID,
            APPLICATION_ADMINISTRATIVE_PROFILE_ID,
            APPLICATION_HOST_LIMITED_PROFILE_ID,
        ],
        "health_read" | "health_delta" | "storage_status" => &[
            APPLICATION_DEFAULT_PROFILE_ID,
            APPLICATION_ADMINISTRATIVE_PROFILE_ID,
        ],
        _ => &[APPLICATION_DEFAULT_PROFILE_ID],
    }
}

fn primitive_lsp_methods(operation: &str) -> &'static [&'static str] {
    match operation {
        "code_signature_search" => &["textDocument/signatureHelp"],
        "code_implementations" => &["textDocument/implementation"],
        "code_type_hierarchy" => &[
            "textDocument/typeDefinition",
            "textDocument/prepareTypeHierarchy",
            "typeHierarchy/supertypes",
            "typeHierarchy/subtypes",
        ],
        "code_callers" => &[
            "textDocument/prepareCallHierarchy",
            "callHierarchy/incomingCalls",
        ],
        "qualified_name" => &["textDocument/declaration"],
        "source_body" => &["textDocument/hover"],
        "source_outline" => &["textDocument/documentSymbol"],
        "diagnostics_read" => &["textDocument/diagnostic"],
        _ => &[],
    }
}

const PRIMITIVE_READ_SPECS: [PrimitiveReadSpec; 17] = [
    primitive_spec("code_signature_search"),
    primitive_spec("code_implementations"),
    primitive_spec("code_type_hierarchy"),
    primitive_spec("code_callers"),
    primitive_spec("session_lookup"),
    primitive_spec("qualified_name"),
    primitive_spec("call_chain"),
    primitive_spec("file_dependents"),
    primitive_spec("source_lines"),
    primitive_spec("source_body"),
    primitive_spec("source_outline"),
    primitive_spec("module_api"),
    primitive_spec("file_metadata"),
    primitive_spec("health_read"),
    primitive_spec("health_delta"),
    primitive_spec("storage_status"),
    primitive_spec("diagnostics_read"),
];

const PRE_DASHBOARD_PRIMITIVE_SURFACES: [BindingSurface; 3] = [
    BindingSurface::Cli,
    BindingSurface::Mcp,
    BindingSurface::Http,
];

const PR14_DASHBOARD_PRIMITIVE_SURFACES: [BindingSurface; 4] = [
    BindingSurface::Cli,
    BindingSurface::Mcp,
    BindingSurface::Http,
    BindingSurface::Dashboard,
];

fn primitive_read_surfaces(spec: &PrimitiveReadSpec) -> &'static [BindingSurface] {
    match spec.operation {
        "health_read" | "storage_status" | "diagnostics_read" => &PR14_DASHBOARD_PRIMITIVE_SURFACES,
        _ => &PRE_DASHBOARD_PRIMITIVE_SURFACES,
    }
}

const fn primitive_spec(operation: &'static str) -> PrimitiveReadSpec {
    PrimitiveReadSpec {
        operation,
        capability: operation,
        use_case: operation,
    }
}

fn primitive_schema(operation: &str, suffix: &str) -> Result<SchemaRef, ApplicationContractError> {
    let operation = operation.replace('_', "-");
    Ok(SchemaRef::new(
        SchemaId::new(format!("schema.application.primitive.{operation}.{suffix}"))?,
        1,
    )?)
}

fn primitive_operation(
    spec: &PrimitiveReadSpec,
) -> Result<ApplicationOperation, ApplicationContractError> {
    Ok(ApplicationOperation::new(
        CapabilityId::new(format!(
            "capability.application.primitive.{}",
            spec.capability.replace('_', "-")
        ))?,
        tracedecay_tool_catalog::UseCaseId::new(format!(
            "use-case.application.primitive.{}",
            spec.use_case.replace('_', "-")
        ))?,
        ResultContractRef::from_schema(&primitive_schema(spec.operation, "result")?),
        true,
    ))
}

pub fn primitive_read_operation(
    operation: &str,
) -> Result<Option<ApplicationOperation>, ApplicationContractError> {
    if operation == "code_symbol_search" {
        return symbol_search_operation().map(Some);
    }
    PRIMITIVE_READ_SPECS
        .iter()
        .find(|spec| spec.operation == operation)
        .map(primitive_operation)
        .transpose()
}

pub fn primitive_read_handler_descriptors()
-> Result<Vec<ApplicationHandlerDescriptor>, ApplicationContractError> {
    PRIMITIVE_READ_SPECS
        .iter()
        .map(|spec| {
            ApplicationHandlerDescriptor::new(
                primitive_operation(spec)?,
                primitive_schema(spec.operation, "request")?,
                primitive_schema(spec.operation, "result")?,
            )
        })
        .collect()
}

pub fn primitive_read_contribution() -> Result<CatalogContributionV1, ApplicationContractError> {
    let mut capabilities = Vec::with_capacity(PRIMITIVE_READ_SPECS.len());
    let mut bindings = Vec::with_capacity(
        PRIMITIVE_READ_SPECS
            .iter()
            .map(|spec| {
                primitive_read_surfaces(spec).len() + primitive_lsp_methods(spec.operation).len()
            })
            .sum(),
    );
    for spec in &PRIMITIVE_READ_SPECS {
        let capability_id = CapabilityId::new(format!(
            "capability.application.primitive.{}",
            spec.capability.replace('_', "-")
        ))?;
        let surfaces = primitive_read_surfaces(spec);
        let (surface_bindings, mut binding_ids) =
            current_bindings(&capability_id, spec.operation, surfaces.iter().copied())?;
        bindings.extend(surface_bindings);
        binding_ids.reserve(primitive_lsp_methods(spec.operation).len());
        for method in primitive_lsp_methods(spec.operation) {
            let method_id = method.to_ascii_lowercase().replace('/', "-");
            let binding_id =
                BindingId::new(format!("binding.lsp.{}.{}.v1", spec.operation, method_id))?;
            bindings.push(SurfaceBindingV1::new(SurfaceBindingInputV1 {
                binding_id: binding_id.clone(),
                capability_id: capability_id.clone(),
                surface: BindingSurface::Lsp,
                operation: SurfaceOperationName::new(*method)?,
                protocol_revisions: ProtocolRevisionRange::new(1, 1)?,
                required_features: Vec::new(),
                status: BindingStatus::Current,
                alias_of: None,
            })?);
            binding_ids.push(binding_id);
        }
        capabilities.push(CapabilityManifestV1::new(CapabilityManifestInputV1 {
            capability_id,
            use_case_id: tracedecay_tool_catalog::UseCaseId::new(format!(
                "use-case.application.primitive.{}",
                spec.use_case.replace('_', "-")
            ))?,
            routing: RoutingContractV1::new(
                1,
                format!("Read {}", spec.operation.replace('_', " ")),
                "Invoke the daemon-retained typed primitive owner.",
                vec![format!("Read {}", spec.operation.replace('_', " "))],
            )?,
            request_schema: primitive_schema(spec.operation, "request")?,
            result_schema: primitive_schema(spec.operation, "result")?,
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
            pagination: Some(PaginationContract::new(10, 1_000, 60_000)?),
            idempotency: IdempotencyContract::NotRequired,
            inverse: tracedecay_tool_catalog::InverseContract::NotApplicable,
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
                TerminalState::Unavailable,
                TerminalState::Partial,
            ])?,
            availability: AvailabilityContract::Available,
            binding_ids,
            profile_eligibility: application_profile_ids(primitive_profile_ids(spec.operation))?,
            required_features: Vec::new(),
        })?);
    }
    Ok(CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.application.primitive-reads")?,
        depends_on: Vec::new(),
        capabilities,
        retrieval_primitives: Vec::new(),
        bindings,
    })?)
}

pub fn symbol_search_request_schema() -> Result<SchemaRef, ApplicationContractError> {
    Ok(SchemaRef::new(
        SchemaId::new("schema.application.symbol-search.request")?,
        1,
    )?)
}

pub fn symbol_search_result_schema() -> Result<SchemaRef, ApplicationContractError> {
    Ok(SchemaRef::new(
        SchemaId::new("schema.application.symbol-search.result")?,
        1,
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

/// Catalog contribution for the declared symbol-search use case.
///
/// Root composition remains outside this crate; the contribution declares
/// transport bindings but has no dispatch, storage, or transport side effect.
pub fn symbol_search_contribution() -> Result<CatalogContributionV1, ApplicationContractError> {
    let capability_id = CapabilityId::new(SYMBOL_SEARCH_CAPABILITY)?;
    let request_schema = symbol_search_request_schema()?;
    let result_schema = symbol_search_result_schema()?;
    let (mut bindings, mut binding_ids) = current_bindings(
        &capability_id,
        "code_symbol_search",
        [
            BindingSurface::Cli,
            BindingSurface::Mcp,
            BindingSurface::Http,
        ],
    )?;
    let lsp_binding_id = BindingId::new("binding.lsp.symbol-search.workspace-symbol.v1")?;
    bindings.push(SurfaceBindingV1::new(SurfaceBindingInputV1 {
        binding_id: lsp_binding_id.clone(),
        capability_id: capability_id.clone(),
        surface: BindingSurface::Lsp,
        operation: SurfaceOperationName::new("workspace/symbol")?,
        protocol_revisions: ProtocolRevisionRange::new(1, 1)?,
        required_features: Vec::new(),
        status: BindingStatus::Current,
        alias_of: None,
    })?);
    binding_ids.push(lsp_binding_id);
    let capability = CapabilityManifestV1::new(CapabilityManifestInputV1 {
        capability_id: capability_id.clone(),
        use_case_id: tracedecay_tool_catalog::UseCaseId::new(SYMBOL_SEARCH_USE_CASE)?,
        routing: RoutingContractV1::new(
            1,
            "Search symbols",
            "Search the admitted single-root query symbol evidence.",
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
        inverse: tracedecay_tool_catalog::InverseContract::NotApplicable,
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
            TerminalState::Unavailable,
            TerminalState::Partial,
        ])?,
        availability: AvailabilityContract::Available,
        binding_ids,
        profile_eligibility: application_profile_ids(&[
            APPLICATION_DEFAULT_PROFILE_ID,
            APPLICATION_COMPACT_PROFILE_ID,
            APPLICATION_HOST_LIMITED_PROFILE_ID,
        ])?,
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
        )?),
        omission_contract: OmissionContractRef::new(SchemaRef::new(
            SchemaId::new("schema.application.evidence-omission")?,
            1,
        )?),
        scoring_contract: ScoringContractRef::new(SchemaRef::new(
            SchemaId::new("schema.application.evidence-score")?,
            1,
        )?),
        contribution_contract: ContributionContractRef::new(SchemaRef::new(
            SchemaId::new("schema.application.retriever-contribution")?,
            1,
        )?),
        deterministic_order: SortContract::new(
            SortContractId::new("sort.application.symbol-search.v1")?,
            1,
        )?,
        default_page_size: 10,
        maximum_page_size: 100,
        temporal_modes: vec![TemporalMode::Current],
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
        bindings,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_search_advertises_only_supported_temporal_modes() {
        let contribution = symbol_search_contribution().expect("symbol-search contribution");
        let primitive = contribution
            .retrieval_primitives()
            .first()
            .expect("symbol-search retrieval primitive");

        assert_eq!(primitive.temporal_modes(), &[TemporalMode::Current]);
    }
}
