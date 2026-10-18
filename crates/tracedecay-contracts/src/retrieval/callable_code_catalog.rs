use schemars::JsonSchema;
use tracedecay_tool_catalog::{
    ApplicationSurfaceOperation, AvailabilityContract, BindingId, BindingSurface,
    CancellationContract, CancellationPoint, CapabilityId, CapabilityManifestV1,
    CatalogContributionInputV1, CatalogContributionV1, ContributionId, DeadlineBehavior,
    DeadlineContract, DeniedDisclosurePolicy, EffectClass, ExecutableSchemaAuthority,
    LifecycleClass, PaginationContract, PrivacyClass, ProfileId, ProtocolRevisionRange,
    RevalidationContract, RevalidationPoint, RoutingContractV1, SchemaId, SchemaRef,
    ScopeDimension, ScopeRequirement, StreamingContract, SurfaceBindingInputV1, SurfaceBindingV1,
    SurfaceOperationName, TerminalState, TerminalStateContract, UseCaseId,
};

use crate::capability_manifest::{
    ApplicationCapabilityManifestInput, application_capability_manifest,
};
use crate::current_application_bindings;
use crate::error::ApplicationContractError;
use crate::handlers::{
    ApplicationHandlerDescriptor, ApplicationOperation, capability_executable_schema,
};
use crate::result::ResultContractRef;
use crate::schema_bodies::{SchemaBodyMaterialization, attach_schema_bodies};
use crate::surface_contracts::{
    CodeCalleesSurfaceRequest, CodeExactOccurrenceSurfaceRequest, CodeFacetSurfaceRequest,
    CodeNavigationSurfaceRequest, CodePhraseSearchSurfaceRequest, CodeTimelineSurfaceRequest,
};

use super::callable_code::{
    CALLABLE_CODE_OPERATION_COUNT, CallableCodeOperationKind, CallableCodeOperations,
    CodeFacetRecord, CodeQueryPage, CodeTimelineRecord, ExactOccurrenceRecord,
    LexicalOccurrenceRecord,
};
use super::catalog::APPLICATION_DEFAULT_PROFILE_ID;
use super::symbol_graph::{SymbolPrimitiveRecord, SymbolRelationRecord};

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
        .filter(|kind| !is_internal_only(*kind) && canonical_surface_equivalent(*kind).is_none())
        .map(|kind| {
            let surface_operation = reachable_surface_operation(kind).ok_or(
                ApplicationContractError::Inconsistent {
                    field: "callable code surface handler",
                },
            )?;
            ApplicationHandlerDescriptor::for_catalog_operation(
                surface_operation,
                "service.application.callable-code",
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
    callable_code_catalog_contribution_with(SchemaBodyMaterialization::Materialize)
}

pub(crate) fn callable_code_catalog_contribution_with(
    materialize: SchemaBodyMaterialization,
) -> Result<CatalogContributionV1, ApplicationContractError> {
    let mut capabilities = Vec::with_capacity(CALLABLE_CODE_OPERATION_COUNT);
    let mut bindings = Vec::with_capacity(27);
    for kind in CallableCodeOperationKind::ALL
        .into_iter()
        .filter(|kind| !is_internal_only(*kind) && canonical_surface_equivalent(*kind).is_none())
    {
        let operation =
            reachable_surface_operation(kind).ok_or(ApplicationContractError::Inconsistent {
                field: "callable code surface operation binding",
            })?;
        let surface_operation = ApplicationSurfaceOperation::from_catalog_name(operation).ok_or(
            ApplicationContractError::Inconsistent {
                field: "callable code surface operation identity",
            },
        )?;
        let (surface_bindings, mut binding_ids) = current_application_bindings(
            &code_query_capability_id(kind)?,
            surface_operation,
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
            })?);
            binding_ids.push(binding_id);
        }
        capabilities.push(code_query_capability(kind, binding_ids)?);
    }
    let contribution = CatalogContributionV1::new(CatalogContributionInputV1::new(
        ContributionId::new("contribution.application.callable-code-query")?,
        Vec::new(),
        capabilities,
        bindings,
    ))?;
    attach_schema_bodies(contribution, materialize, callable_code_executable_schemas)
}

/// Rust-owned request/result schema bodies for every advertised callable-code
/// query.
///
/// Requests use the DTOs consumed by the transport adapter. The application
/// adds temporal and page controls after parsing; advertising its internal
/// request instead makes valid catalog-generated calls fail deserialization.
fn callable_code_executable_schemas(
    contribution: &CatalogContributionV1,
) -> Result<Vec<ExecutableSchemaAuthority>, ApplicationContractError> {
    let mut schemas = Vec::new();
    macro_rules! add {
        ($kind:ident, $request:ty, $item:ty) => {
            schemas.push(callable_code_executable_schema::<
                $request,
                CodeQueryPage<$item>,
            >(
                contribution,
                CallableCodeOperationKind::$kind,
                concat!("tracedecay_contracts::surface_contracts::", stringify!($request)),
                concat!(
                    "tracedecay_contracts::retrieval::CodeQueryPage<tracedecay_contracts::retrieval::",
                    stringify!($item),
                    ">"
                ),
            )?)
        };
    }
    add!(
        ExactOccurrence,
        CodeExactOccurrenceSurfaceRequest,
        ExactOccurrenceRecord
    );
    add!(
        PhraseSearch,
        CodePhraseSearchSurfaceRequest,
        LexicalOccurrenceRecord
    );
    add!(Callees, CodeCalleesSurfaceRequest, SymbolRelationRecord);
    add!(Facets, CodeFacetSurfaceRequest, CodeFacetRecord);
    add!(Timeline, CodeTimelineSurfaceRequest, CodeTimelineRecord);
    add!(
        Declaration,
        CodeNavigationSurfaceRequest,
        SymbolPrimitiveRecord
    );
    add!(
        TypeDefinition,
        CodeNavigationSurfaceRequest,
        SymbolPrimitiveRecord
    );
    add!(
        References,
        CodeNavigationSurfaceRequest,
        SymbolRelationRecord
    );
    Ok(schemas)
}

fn callable_code_executable_schema<Request, Response>(
    contribution: &CatalogContributionV1,
    kind: CallableCodeOperationKind,
    request_rust_type_path: &'static str,
    result_rust_type_path: &'static str,
) -> Result<ExecutableSchemaAuthority, ApplicationContractError>
where
    Request: JsonSchema,
    Response: JsonSchema,
{
    capability_executable_schema::<Request, Response>(
        contribution,
        &code_query_capability_id(kind)?,
        "callable code schema capability",
        request_rust_type_path,
        result_rust_type_path,
    )
}

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
        CallableCodeOperationKind::ExactOccurrence
        | CallableCodeOperationKind::PhraseSearch
        | CallableCodeOperationKind::Callees
        | CallableCodeOperationKind::Facets
        | CallableCodeOperationKind::Timeline
        | CallableCodeOperationKind::Declaration
        | CallableCodeOperationKind::TypeDefinition
        | CallableCodeOperationKind::References
        | CallableCodeOperationKind::SourceMetadata => None,
    }
}

fn is_internal_only(kind: CallableCodeOperationKind) -> bool {
    kind == CallableCodeOperationKind::SourceMetadata
}

fn reachable_surface_operation(kind: CallableCodeOperationKind) -> Option<&'static str> {
    match kind {
        CallableCodeOperationKind::ExactOccurrence => Some("code_exact_occurrence"),
        CallableCodeOperationKind::PhraseSearch => Some("code_phrase_search"),
        CallableCodeOperationKind::Callees => Some("code_callees"),
        CallableCodeOperationKind::Facets => Some("code_facets"),
        CallableCodeOperationKind::Timeline => Some("code_timeline"),
        CallableCodeOperationKind::Declaration => Some("code_declaration"),
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
        CallableCodeOperationKind::ExactOccurrence => &["textDocument/definition"],
        CallableCodeOperationKind::References => &["textDocument/references"],
        CallableCodeOperationKind::Callees => &["callHierarchy/outgoingCalls"],
        _ => &[],
    }
}

/// The page every callable-code query serves when the caller omits one;
/// `meta.cursor` continues past it through the whole result set.
pub(crate) const CALLABLE_CODE_DEFAULT_PAGE_SIZE: u32 = 10;

fn code_query_capability_id(
    kind: CallableCodeOperationKind,
) -> Result<CapabilityId, ApplicationContractError> {
    Ok(CapabilityId::new(format!(
        "capability.application.code-query.{}",
        kind.as_str().replace('_', "-")
    ))?)
}

/// The routing description of each advertised code-query kind. Kinds served
/// by a canonical surface carry that surface's description instead.
fn code_query_description(kind: CallableCodeOperationKind) -> Option<&'static str> {
    match kind {
        CallableCodeOperationKind::ExactOccurrence => Some(
            "Find where an exact name is defined, or where a literal technical term (path, CLI flag, error code, configuration key, quoted phrase) appears. A bare identifier matches definition names only, never its uses, so the page reports partial coverage; use grep or code_references to find every use.",
        ),
        CallableCodeOperationKind::Callees => Some(
            "What does this call: outgoing calls of a known symbol node ID up to `maximum_depth` (default 3). A callee that is a trait method also returns the concrete impl methods reachable through the trait, tagged `dispatch_via_trait` with `dispatch_from`; set `resolve_trait_dispatch: false` for direct call edges only.",
        ),
        CallableCodeOperationKind::PhraseSearch => Some(
            "Rank indexed code text against `query` plus one or more boosting `phrases`, returning scored occurrences with the terms and phrases each matched, optionally limited to chosen fields. Use grep for every literal or regex match in the working tree, or code_exact_occurrence for where a name is defined.",
        ),
        CallableCodeOperationKind::Facets => Some(
            "Count indexed symbols by kind, or files by language or path, in the latest code-index generation under an optional `scope.path_prefix`, one row per value. Use code_timeline for the generation's total file and symbol counts.",
        ),
        CallableCodeOperationKind::Timeline => Some(
            "Report the latest sealed code-index generation as one row with its generation ID, seal time, and file and symbol counts under an optional `scope.path_prefix`. It is not a history of generations; use code_facets for counts by kind, language, or path.",
        ),
        CallableCodeOperationKind::Declaration => Some(
            "Return the symbol record (name, kind, file, lines, signature) for a known symbol node ID, or nothing when it falls outside `scope.path_prefix`; it does not resolve a use site to its declaration. Use qualified_name or find_exact_symbol when you have a name rather than a node ID.",
        ),
        CallableCodeOperationKind::TypeDefinition => Some(
            "For a symbol node ID, return the node itself when its kind names a type (struct, enum, class, interface, trait, type), otherwise the types its outgoing TypeOf edges point to. Use type_hierarchy to walk the implementors and extenders of a type.",
        ),
        CallableCodeOperationKind::References => Some(
            "List the symbols with a direct (one-hop) Calls, Uses, TypeOf, or Annotates edge into a known symbol node ID, marked partial when call targets or imports are unresolved. Use callers for inbound callers beyond one hop, or grep for textual mentions.",
        ),
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

fn code_query_capability(
    kind: CallableCodeOperationKind,
    binding_ids: Vec<BindingId>,
) -> Result<CapabilityManifestV1, ApplicationContractError> {
    let operation = kind.as_str();
    let readable_name = operation.replace('_', " ");
    Ok(application_capability_manifest(
        ApplicationCapabilityManifestInput {
            capability_id: code_query_capability_id(kind)?,
            use_case_id: UseCaseId::new(format!(
                "use-case.application.code-query.{}",
                operation.replace('_', "-")
            ))?,
            routing: RoutingContractV1::new(
                1,
                format!("Query {readable_name}"),
                code_query_description(kind).ok_or(ApplicationContractError::Inconsistent {
                    field: "callable code routing description",
                })?,
                // Keep examples distinct from primitive-read fixtures ("Read …").
                vec![format!("Query indexed {readable_name}")],
            )?,
            request_schema: callable_code_request_schema(kind)?,
            result_schema: callable_code_result_schema(kind)?,
            effect: EffectClass::Read,
            scope: code_query_scope()?,
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
            pagination: Some(PaginationContract::new(
                CALLABLE_CODE_DEFAULT_PAGE_SIZE,
                1_000,
                15 * 60 * 1_000,
            )?),
            inverse: None,
            authority_revalidation: RevalidationContract::required(vec![
                RevalidationPoint::Authority,
                RevalidationPoint::Scope,
                RevalidationPoint::Policy,
                RevalidationPoint::Configuration,
            ])?,
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
        },
    )?)
}

fn code_query_scope() -> Result<ScopeRequirement, ApplicationContractError> {
    Ok(ScopeRequirement::new(vec![
        ScopeDimension::Project,
        ScopeDimension::Repository,
        ScopeDimension::Worktree,
        ScopeDimension::Resource,
    ])?)
}
