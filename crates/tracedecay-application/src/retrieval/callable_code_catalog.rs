use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, BindingId, BindingStatus, BindingSurface,
    CancellationContract, CancellationPoint, CapabilityId, CapabilityManifestInputV1,
    CapabilityManifestV1, CatalogContributionInputV1, CatalogContributionV1, ContributionId,
    DeadlineBehavior, DeadlineContract, DeniedDisclosurePolicy, EffectClass, IdempotencyContract,
    LifecycleClass, PaginationContract, PrivacyClass, ProfileId, ProtocolRevisionRange,
    ReceiptContract, ReconciliationContract, RevalidationContract, RevalidationPoint,
    RoutingContractV1, SchemaId, SchemaRef, ScopeDimension, ScopeRequirement, StreamingContract,
    SurfaceBindingInputV1, SurfaceBindingV1, SurfaceOperationName, TerminalState,
    TerminalStateContract, UseCaseId,
};

use crate::current_bindings;
use crate::error::ApplicationContractError;
use crate::handlers::{ApplicationHandlerDescriptor, ApplicationOperation};
use crate::result::ResultContractRef;

use super::callable_code::{
    CALLABLE_CODE_OPERATION_COUNT, CallableCodeOperationKind, CallableCodeOperations,
};
use super::catalog::APPLICATION_DEFAULT_PROFILE_ID;

pub fn callable_code_request_schema(
    kind: CallableCodeOperationKind,
) -> Result<SchemaRef, ApplicationContractError> {
    code_query_schema(kind, "request")
}

pub fn callable_code_result_schema(
    kind: CallableCodeOperationKind,
) -> Result<SchemaRef, ApplicationContractError> {
    code_query_schema(kind, "result")
}

fn code_query_schema(
    kind: CallableCodeOperationKind,
    suffix: &str,
) -> Result<SchemaRef, ApplicationContractError> {
    Ok(SchemaRef::new(
        SchemaId::new(format!(
            "schema.application.code-query.{}.{}",
            kind.as_str().replace('_', "-"),
            suffix
        ))?,
        1,
    )?)
}

pub fn callable_code_operation(
    kind: CallableCodeOperationKind,
) -> Result<ApplicationOperation, ApplicationContractError> {
    let operation = kind.as_str().replace('_', "-");
    let result_schema = callable_code_result_schema(kind)?;
    Ok(ApplicationOperation::new(
        CapabilityId::new(format!("capability.application.code-query.{operation}"))?,
        UseCaseId::new(format!("use-case.application.code-query.{operation}"))?,
        ResultContractRef::from_schema(&result_schema),
        true,
    ))
}

pub fn callable_code_operations() -> Result<CallableCodeOperations, ApplicationContractError> {
    CallableCodeOperations::new(
        CallableCodeOperationKind::ALL
            .into_iter()
            .map(|kind| callable_code_operation(kind).map(|operation| (kind, operation)))
            .collect::<Result<Vec<_>, _>>()?,
    )
}

pub fn callable_code_handler_descriptors()
-> Result<Vec<ApplicationHandlerDescriptor>, ApplicationContractError> {
    CallableCodeOperationKind::ALL
        .into_iter()
        .filter(|kind| canonical_surface_equivalent(*kind).is_none())
        .map(|kind| {
            ApplicationHandlerDescriptor::new(
                callable_code_operation(kind)?,
                callable_code_request_schema(kind)?,
                callable_code_result_schema(kind)?,
            )
        })
        .collect()
}

/// Application contribution for the generation-bound query callable query
/// family. Only operations with production-owned application dispatch are
/// advertised on transport surfaces.
pub fn callable_code_catalog_contribution()
-> Result<CatalogContributionV1, ApplicationContractError> {
    let mut capabilities = Vec::with_capacity(CALLABLE_CODE_OPERATION_COUNT);
    let mut bindings = Vec::with_capacity(27);
    for kind in CallableCodeOperationKind::ALL
        .into_iter()
        .filter(|kind| canonical_surface_equivalent(*kind).is_none())
    {
        let operation = reachable_surface_operation(kind)
            .expect("non-equivalent callable operations have production bindings");
        let (surface_bindings, mut binding_ids) = current_bindings(
            &code_query_capability_id(kind)?,
            operation,
            [
                BindingSurface::Cli,
                BindingSurface::Mcp,
                BindingSurface::Http,
            ],
        )?;
        bindings.extend(surface_bindings);
        for method in lsp_methods(kind) {
            let method_id = method.to_ascii_lowercase().replace('/', "-");
            let binding_id = BindingId::new(format!("binding.lsp.{operation}.{method_id}.v1"))?;
            bindings.push(SurfaceBindingV1::new(SurfaceBindingInputV1 {
                binding_id: binding_id.clone(),
                capability_id: code_query_capability_id(kind)?,
                surface: BindingSurface::Lsp,
                operation: SurfaceOperationName::new(*method)?,
                protocol_revisions: ProtocolRevisionRange::new(1, 1)?,
                required_features: Vec::new(),
                status: BindingStatus::Current,
                alias_of: None,
            })?);
            binding_ids.push(binding_id);
        }
        capabilities.push(code_query_capability(kind, binding_ids)?);
    }
    debug_assert_eq!(
        capabilities.len() + CANONICAL_SURFACE_EQUIVALENT_COUNT,
        CALLABLE_CODE_OPERATION_COUNT
    );
    Ok(CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.application.callable-code-query")?,
        depends_on: Vec::new(),
        capabilities,
        retrieval_primitives: Vec::new(),
        bindings,
    })?)
}

const CANONICAL_SURFACE_EQUIVALENT_COUNT: usize = 9;

/// Existing canonical application surfaces own these semantics. Keeping the
/// mapping here prevents the callable-code catalog from advertising a second
/// capability, kernel, or transport operation for the same query.
fn canonical_surface_equivalent(kind: CallableCodeOperationKind) -> Option<&'static str> {
    match kind {
        CallableCodeOperationKind::SymbolSearch => Some("code_symbol_search"),
        CallableCodeOperationKind::QualifiedName => Some("qualified_name"),
        CallableCodeOperationKind::SignatureSearch => Some("code_signature_search"),
        CallableCodeOperationKind::Implementations => Some("code_implementations"),
        CallableCodeOperationKind::TypeHierarchy => Some("code_type_hierarchy"),
        CallableCodeOperationKind::Callers => Some("code_callers"),
        CallableCodeOperationKind::Impact => Some("feedback_impact"),
        CallableCodeOperationKind::ModuleApi => Some("module_api"),
        CallableCodeOperationKind::SourceMetadata => Some("file_metadata"),
        CallableCodeOperationKind::ExactOccurrence
        | CallableCodeOperationKind::PhraseSearch
        | CallableCodeOperationKind::Callees
        | CallableCodeOperationKind::Facets
        | CallableCodeOperationKind::Timeline
        | CallableCodeOperationKind::Declaration
        | CallableCodeOperationKind::Definition
        | CallableCodeOperationKind::TypeDefinition
        | CallableCodeOperationKind::References => None,
    }
}

fn reachable_surface_operation(kind: CallableCodeOperationKind) -> Option<&'static str> {
    match kind {
        CallableCodeOperationKind::ExactOccurrence => Some("code_exact_occurrence"),
        CallableCodeOperationKind::PhraseSearch => Some("code_phrase_search"),
        CallableCodeOperationKind::Callees => Some("code_callees"),
        CallableCodeOperationKind::Facets => Some("code_facets"),
        CallableCodeOperationKind::Timeline => Some("code_timeline"),
        CallableCodeOperationKind::Declaration => Some("code_declaration"),
        CallableCodeOperationKind::Definition => Some("code_definition"),
        CallableCodeOperationKind::TypeDefinition => Some("code_type_definition"),
        CallableCodeOperationKind::References => Some("code_references"),
        CallableCodeOperationKind::SymbolSearch
        | CallableCodeOperationKind::QualifiedName
        | CallableCodeOperationKind::SignatureSearch
        | CallableCodeOperationKind::Implementations
        | CallableCodeOperationKind::TypeHierarchy
        | CallableCodeOperationKind::Callers
        | CallableCodeOperationKind::Impact
        | CallableCodeOperationKind::ModuleApi
        | CallableCodeOperationKind::SourceMetadata => None,
    }
}

fn lsp_methods(kind: CallableCodeOperationKind) -> &'static [&'static str] {
    match kind {
        CallableCodeOperationKind::ExactOccurrence => {
            &["textDocument/definition", "textDocument/references"]
        }
        CallableCodeOperationKind::Callees => &["callHierarchy/outgoingCalls"],
        _ => &[],
    }
}

fn code_query_capability_id(
    kind: CallableCodeOperationKind,
) -> Result<CapabilityId, ApplicationContractError> {
    Ok(CapabilityId::new(format!(
        "capability.application.code-query.{}",
        kind.as_str().replace('_', "-")
    ))?)
}

fn code_query_capability(
    kind: CallableCodeOperationKind,
    binding_ids: Vec<BindingId>,
) -> Result<CapabilityManifestV1, ApplicationContractError> {
    let operation = kind.as_str();
    let readable_name = operation.replace('_', " ");
    Ok(CapabilityManifestV1::new(CapabilityManifestInputV1 {
        capability_id: code_query_capability_id(kind)?,
        use_case_id: UseCaseId::new(format!(
            "use-case.application.code-query.{}",
            operation.replace('_', "-")
        ))?,
        routing: RoutingContractV1::new(
            1,
            format!("Query {readable_name}"),
            format!(
                "Invoke the generation-bound query {readable_name} query without replacing its owning kernel."
            ),
            // Keep examples distinct from primitive-read fixtures ("Read …").
            vec![format!("Query indexed {readable_name}")],
        )?,
        request_schema: callable_code_request_schema(kind)?,
        result_schema: callable_code_result_schema(kind)?,
        effect: EffectClass::Read,
        scope: code_query_scope()?,
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
        pagination: Some(PaginationContract::new(10, 1_000, 15 * 60 * 1_000)?),
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
        profile_eligibility: vec![ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID)?],
        required_features: Vec::new(),
    })?)
}

fn code_query_scope() -> Result<ScopeRequirement, ApplicationContractError> {
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
    fn canonical_surface_equivalents_are_explicit_and_unique() {
        let equivalents: Vec<_> = CallableCodeOperationKind::ALL
            .into_iter()
            .filter_map(|kind| {
                canonical_surface_equivalent(kind).map(|operation| (kind, operation))
            })
            .collect();

        assert_eq!(
            equivalents,
            vec![
                (
                    CallableCodeOperationKind::SymbolSearch,
                    "code_symbol_search",
                ),
                (CallableCodeOperationKind::QualifiedName, "qualified_name"),
                (
                    CallableCodeOperationKind::SignatureSearch,
                    "code_signature_search",
                ),
                (
                    CallableCodeOperationKind::Implementations,
                    "code_implementations",
                ),
                (
                    CallableCodeOperationKind::TypeHierarchy,
                    "code_type_hierarchy",
                ),
                (CallableCodeOperationKind::Callers, "code_callers"),
                (CallableCodeOperationKind::Impact, "feedback_impact"),
                (CallableCodeOperationKind::ModuleApi, "module_api"),
                (CallableCodeOperationKind::SourceMetadata, "file_metadata"),
            ]
        );
        let mut operation_names: Vec<_> = equivalents
            .iter()
            .map(|(_, operation)| *operation)
            .collect();
        operation_names.sort_unstable();
        operation_names.dedup();
        assert_eq!(operation_names.len(), CANONICAL_SURFACE_EQUIVALENT_COUNT);
    }
}
