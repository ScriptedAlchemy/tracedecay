use schemars::JsonSchema;
use tracedecay_tool_catalog::{
    ApplicationSurfaceOperation, AuthorityRequirement, AvailabilityContract, BindingId,
    BindingStatus, BindingSurface, CancellationContract, CancellationPoint, CapabilityId,
    CapabilityManifestInputV1, CapabilityManifestV1, CatalogContributionInputV1,
    CatalogContributionV1, ContributionContractRef, ContributionId, CoverageContractRef,
    DeadlineBehavior, DeadlineContract, DeniedDisclosurePolicy, EffectClass,
    ExecutableSchemaAuthority, IdempotencyContract, LifecycleClass, OmissionContractRef,
    PaginationContract, PrivacyClass, ProfileId, ProtocolRevisionRange, ReceiptContract,
    ReconciliationContract, RetrievalFamily, RetrievalPrimitiveManifestInputV1,
    RetrievalPrimitiveManifestV1, RetrieverId, RevalidationContract, RevalidationPoint,
    RoutingContractV1, SchemaId, SchemaRef, ScopeDimension, ScopeRequirement, ScoringContractRef,
    SortContract, SortContractId, StreamingContract, SurfaceBindingInputV1, SurfaceBindingV1,
    SurfaceOperationName, TemporalMode, TerminalState, TerminalStateContract,
};

use crate::error::ApplicationContractError;
use crate::handlers::{ApplicationHandlerDescriptor, ApplicationOperation};
use crate::result::ResultContractRef;
use crate::retrieval::grep_analysis::RedundancyResultV1;
use crate::retrieval::primitive_surface::{
    CalleesResultV1, CalleesSurfaceRequestV1, ContextResultV1, ContextSurfaceRequestV1,
    ImpactResultV1, ImpactSurfaceRequestV1, NodeResultV1, NodeSurfaceRequestV1, PortOrderResultV1,
    PortOrderSurfaceRequestV1, PortStatusResultV1, PortStatusSurfaceRequestV1,
    RedundancySurfaceRequestV1, RenamePreviewPrimitiveOutcomeV1, RenamePreviewPrimitiveRequestV1,
    SimilarResultV1, SimilarSurfaceRequestV1, TodosResultV1, TodosSurfaceRequestV1,
};
use crate::retrieval::requests::{
    CallChainPrimitiveRequest, CallChainPrimitiveResult, DiagnosticsPrimitiveRequest,
    DiagnosticsPrimitiveResult, FileDependentsPrimitiveRequest, FileDependentsPrimitiveResult,
    HealthDeltaRequest, HealthDeltaResult, HealthReadRequest, HealthReadResult,
    ModuleApiPrimitiveRequest, ModuleApiPrimitiveResult, QualifiedNamePrimitiveRequest,
    QualifiedNamePrimitiveResult, SessionLookupRequest, SessionLookupResult,
    SourceBodyPrimitiveRequest, SourceBodyPrimitiveResult, SourceLinesRequest, SourceLinesResult,
    SourceOutlinePrimitiveRequest, SourceOutlinePrimitiveResult, StorageStatusPrimitiveRequest,
    StorageStatusPrimitiveResult,
};
use crate::retrieval::symbol_graph::{
    SymbolGraphPage, SymbolPrimitiveRecord, SymbolRelationRecord, TypeHierarchyRecord,
};
use crate::surface_contracts::{
    CodeCallersSurfaceRequest, CodeImplementationsSurfaceRequest,
    CodeSignatureSearchSurfaceRequest, CodeSymbolSearchSurfaceRequest,
    CodeTypeHierarchySurfaceRequest,
};
use crate::{current_application_bindings, current_bindings};

const SYMBOL_SEARCH_CAPABILITY: &str = "capability.application.symbol-search";
const SYMBOL_SEARCH_USE_CASE: &str = "use-case.application.symbol-search";
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
        crate::git::native_integration_surface_catalog_contribution()?,
        crate::configuration::configuration_surface_catalog_contribution()?,
        crate::context_scout::context_scout_surface_catalog_contribution()?,
        crate::feedback::feedback_surface_catalog_contribution()?,
        crate::lsp_context_catalog::lsp_context_catalog_contribution()?,
        crate::observatory_surface::observatory_read_catalog_contribution()?,
        crate::retained_surfaces::retained_surface_catalog_contribution()?,
        crate::source_edit::source_edit_catalog_contribution()?,
    ])
}

/// Resolves the page size an omitted transport control receives from the
/// canonical primitive descriptor.
///
/// Operations outside this primitive family retain the inert page envelope's
/// established value of 10.
pub fn application_operation_default_page_size(operation: ApplicationSurfaceOperation) -> u32 {
    PRIMITIVE_READ_SPECS
        .iter()
        .find(|spec| spec.operation == operation.as_str())
        .map_or(10, |spec| spec.default_page_size)
}

struct PrimitiveReadSpec {
    operation: &'static str,
    capability: &'static str,
    use_case: &'static str,
    default_page_size: u32,
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

const PRIMITIVE_READ_SPECS: &[PrimitiveReadSpec] = &[
    primitive_spec("code_signature_search"),
    primitive_spec("code_implementations"),
    primitive_spec("code_type_hierarchy"),
    primitive_spec("code_callers"),
    primitive_spec("context"),
    primitive_spec("redundancy"),
    primitive_spec("node"),
    primitive_spec("callees"),
    primitive_spec("impact"),
    primitive_spec("similar"),
    primitive_spec("rename_preview"),
    primitive_spec("port_status"),
    primitive_spec("port_order"),
    primitive_spec("todos"),
    primitive_spec("session_lookup"),
    primitive_spec("qualified_name"),
    primitive_spec("call_chain"),
    primitive_spec("file_dependents"),
    primitive_spec("source_lines"),
    primitive_spec("source_body"),
    primitive_spec("source_outline"),
    primitive_spec("module_api"),
    primitive_spec("health_read"),
    primitive_spec("health_delta"),
    primitive_spec("storage_status"),
    primitive_spec_with_default_page_size("diagnostics_read", 1_000),
];

const PRE_DASHBOARD_PRIMITIVE_SURFACES: [BindingSurface; 3] = [
    BindingSurface::Cli,
    BindingSurface::Mcp,
    BindingSurface::Http,
];

const CLI_MCP_PRIMITIVE_SURFACES: [BindingSurface; 2] = [BindingSurface::Cli, BindingSurface::Mcp];

const DASHBOARD_PRIMITIVE_SURFACES: [BindingSurface; 4] = [
    BindingSurface::Cli,
    BindingSurface::Mcp,
    BindingSurface::Http,
    BindingSurface::Dashboard,
];

fn primitive_read_surfaces(spec: &PrimitiveReadSpec) -> &'static [BindingSurface] {
    match spec.operation {
        // These established tool handlers retain their current wire schemas
        // and rendering across the generic CLI fallback and MCP, while using
        // this operation identity for canonical code-graph read admission.
        "context" | "redundancy" | "node" | "callees" | "impact" | "similar" | "rename_preview"
        | "port_status" | "port_order" | "todos" => &CLI_MCP_PRIMITIVE_SURFACES,
        "health_read" | "storage_status" | "diagnostics_read" => &DASHBOARD_PRIMITIVE_SURFACES,
        _ => &PRE_DASHBOARD_PRIMITIVE_SURFACES,
    }
}

const fn primitive_spec(operation: &'static str) -> PrimitiveReadSpec {
    primitive_spec_with_default_page_size(operation, 10)
}

const fn primitive_spec_with_default_page_size(
    operation: &'static str,
    default_page_size: u32,
) -> PrimitiveReadSpec {
    PrimitiveReadSpec {
        operation,
        capability: operation,
        use_case: operation,
        default_page_size,
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
            ApplicationHandlerDescriptor::for_catalog_operation(
                spec.operation,
                "service.application.primitive",
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
    for spec in PRIMITIVE_READ_SPECS {
        let capability_id = CapabilityId::new(format!(
            "capability.application.primitive.{}",
            spec.capability.replace('_', "-")
        ))?;
        let surfaces = primitive_read_surfaces(spec);
        let (surface_bindings, mut binding_ids) =
            match ApplicationSurfaceOperation::from_catalog_name(spec.operation) {
                Some(operation) => current_application_bindings(
                    &capability_id,
                    operation,
                    surfaces.iter().copied(),
                )?,
                None => current_bindings(&capability_id, spec.operation, surfaces.iter().copied())?,
            };
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
            pagination: Some(PaginationContract::new(
                spec.default_page_size,
                1_000,
                60_000,
            )?),
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
    let contribution = CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.application.primitive-reads")?,
        depends_on: Vec::new(),
        capabilities,
        retrieval_primitives: Vec::new(),
        bindings,
    })?;
    let schemas = primitive_executable_schemas(&contribution)?;
    Ok(contribution.with_executable_schemas(schemas)?)
}

/// Rust-owned request/result schema bodies for the primitive reads whose wire
/// types live in this crate.
///
/// The registered pairs are exactly the types the daemon parses and returns
/// for these operations: the retrieval reads bind their
/// `crate::retrieval::requests` pairs, and the symbol-graph reads bind the
/// transport DTOs against the [`SymbolGraphPage`] payloads they return, so the
/// generated SDKs cannot describe a shape the surface does not speak.
fn primitive_executable_schemas(
    contribution: &CatalogContributionV1,
) -> Result<Vec<ExecutableSchemaAuthority>, ApplicationContractError> {
    let mut schemas = Vec::new();
    macro_rules! add {
        ($operation:literal, $request:ty, SymbolGraphPage<$item:ident>) => {
            schemas.push(primitive_executable_schema::<$request, SymbolGraphPage<$item>>(
                contribution,
                $operation,
                concat!("tracedecay_contracts::surface_contracts::", stringify!($request)),
                concat!(
                    "tracedecay_contracts::retrieval::SymbolGraphPage<tracedecay_contracts::retrieval::",
                    stringify!($item),
                    ">"
                ),
            )?)
        };
        ($operation:literal, $request:ty, $result:ty) => {
            schemas.push(primitive_executable_schema::<$request, $result>(
                contribution,
                $operation,
                concat!("tracedecay_contracts::retrieval::", stringify!($request)),
                concat!("tracedecay_contracts::retrieval::", stringify!($result)),
            )?)
        };
    }
    add!("session_lookup", SessionLookupRequest, SessionLookupResult);
    add!("source_lines", SourceLinesRequest, SourceLinesResult);
    add!("health_read", HealthReadRequest, HealthReadResult);
    add!("health_delta", HealthDeltaRequest, HealthDeltaResult);
    add!(
        "qualified_name",
        QualifiedNamePrimitiveRequest,
        QualifiedNamePrimitiveResult
    );
    add!(
        "call_chain",
        CallChainPrimitiveRequest,
        CallChainPrimitiveResult
    );
    add!(
        "file_dependents",
        FileDependentsPrimitiveRequest,
        FileDependentsPrimitiveResult
    );
    add!(
        "source_body",
        SourceBodyPrimitiveRequest,
        SourceBodyPrimitiveResult
    );
    add!(
        "source_outline",
        SourceOutlinePrimitiveRequest,
        SourceOutlinePrimitiveResult
    );
    add!(
        "module_api",
        ModuleApiPrimitiveRequest,
        ModuleApiPrimitiveResult
    );
    add!(
        "storage_status",
        StorageStatusPrimitiveRequest,
        StorageStatusPrimitiveResult
    );
    add!(
        "diagnostics_read",
        DiagnosticsPrimitiveRequest,
        DiagnosticsPrimitiveResult
    );
    add!(
        "code_signature_search",
        CodeSignatureSearchSurfaceRequest,
        SymbolGraphPage<SymbolPrimitiveRecord>
    );
    add!(
        "code_implementations",
        CodeImplementationsSurfaceRequest,
        SymbolGraphPage<SymbolRelationRecord>
    );
    add!(
        "code_type_hierarchy",
        CodeTypeHierarchySurfaceRequest,
        SymbolGraphPage<TypeHierarchyRecord>
    );
    add!(
        "code_callers",
        CodeCallersSurfaceRequest,
        SymbolGraphPage<SymbolRelationRecord>
    );
    add!("context", ContextSurfaceRequestV1, ContextResultV1);
    add!("callees", CalleesSurfaceRequestV1, CalleesResultV1);
    add!("impact", ImpactSurfaceRequestV1, ImpactResultV1);
    add!("node", NodeSurfaceRequestV1, NodeResultV1);
    add!("similar", SimilarSurfaceRequestV1, SimilarResultV1);
    add!(
        "rename_preview",
        RenamePreviewPrimitiveRequestV1,
        RenamePreviewPrimitiveOutcomeV1
    );
    add!(
        "port_status",
        PortStatusSurfaceRequestV1,
        PortStatusResultV1
    );
    add!("port_order", PortOrderSurfaceRequestV1, PortOrderResultV1);
    add!("redundancy", RedundancySurfaceRequestV1, RedundancyResultV1);
    add!("todos", TodosSurfaceRequestV1, TodosResultV1);
    Ok(schemas)
}

fn primitive_executable_schema<Request, Response>(
    contribution: &CatalogContributionV1,
    operation: &str,
    request_rust_type_path: &'static str,
    result_rust_type_path: &'static str,
) -> Result<ExecutableSchemaAuthority, ApplicationContractError>
where
    Request: JsonSchema,
    Response: JsonSchema,
{
    let capability_id = CapabilityId::new(format!(
        "capability.application.primitive.{}",
        operation.replace('_', "-")
    ))?;
    let manifest = contribution
        .capabilities()
        .iter()
        .find(|manifest| manifest.capability_id() == &capability_id)
        .ok_or(ApplicationContractError::Inconsistent {
            field: "primitive schema capability",
        })?;
    Ok(ExecutableSchemaAuthority::for_types_at_paths::<
        Request,
        Response,
    >(
        manifest, request_rust_type_path, result_rust_type_path
    )?)
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
    ApplicationHandlerDescriptor::for_catalog_operation(
        "code_symbol_search",
        "service.application.primitive",
        symbol_search_operation()?,
        symbol_search_request_schema()?,
        symbol_search_result_schema()?,
    )
}

/// Catalog contribution for the declared symbol-search use case.
///
/// The contribution declares transport bindings but has no dispatch, storage,
/// or transport side effect; binding them to the canonical dispatcher stays in
/// `tracedecay-daemon-service`.
pub fn symbol_search_contribution() -> Result<CatalogContributionV1, ApplicationContractError> {
    let capability_id = CapabilityId::new(SYMBOL_SEARCH_CAPABILITY)?;
    let request_schema = symbol_search_request_schema()?;
    let result_schema = symbol_search_result_schema()?;
    let (mut bindings, mut binding_ids) = current_application_bindings(
        &capability_id,
        ApplicationSurfaceOperation::CodeSymbolSearch,
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
    let contribution = CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.application.symbol-search")?,
        depends_on: Vec::new(),
        capabilities: vec![capability],
        retrieval_primitives: vec![primitive],
        bindings,
    })?;
    let manifest = contribution.capabilities().first().cloned().ok_or(
        ApplicationContractError::Inconsistent {
            field: "symbol-search capability",
        },
    )?;
    let schemas = vec![ExecutableSchemaAuthority::for_types_at_paths::<
        CodeSymbolSearchSurfaceRequest,
        SymbolGraphPage<SymbolPrimitiveRecord>,
    >(
        &manifest,
        "tracedecay_contracts::surface_contracts::CodeSymbolSearchSurfaceRequest",
        "tracedecay_contracts::retrieval::SymbolGraphPage<tracedecay_contracts::retrieval::SymbolPrimitiveRecord>",
    )?];
    Ok(contribution.with_executable_schemas(schemas)?)
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

    const ESTABLISHED_TOOL_PRIMITIVES: [&str; 10] = [
        "context",
        "redundancy",
        "node",
        "callees",
        "impact",
        "similar",
        "rename_preview",
        "port_status",
        "port_order",
        "todos",
    ];

    #[test]
    fn symbol_search_advertises_only_supported_temporal_modes() {
        let contribution = symbol_search_contribution().expect("symbol-search contribution");
        let primitive = contribution
            .retrieval_primitives()
            .first()
            .expect("symbol-search retrieval primitive");

        assert_eq!(primitive.temporal_modes(), &[TemporalMode::Current]);
    }

    #[test]
    fn established_tool_primitives_pair_cli_and_mcp_bindings() {
        let contribution = primitive_read_contribution().expect("primitive contribution");

        for operation in ESTABLISHED_TOOL_PRIMITIVES {
            let surfaces = contribution
                .bindings()
                .iter()
                .filter(|binding| binding.operation().as_str() == operation)
                .map(|binding| binding.surface())
                .collect::<Vec<_>>();
            assert_eq!(
                surfaces,
                vec![BindingSurface::Cli, BindingSurface::Mcp],
                "{operation} must remain callable from the paired default profile"
            );
        }
    }

    #[test]
    fn diagnostics_catalog_preserves_the_shipped_default_page_size() {
        let operation = primitive_read_operation("diagnostics_read")
            .expect("primitive operation")
            .expect("diagnostics operation");
        let contribution = primitive_read_contribution().expect("primitive contribution");
        let diagnostics = contribution
            .capabilities()
            .iter()
            .find(|capability| capability.capability_id() == operation.capability_id())
            .expect("diagnostics capability");
        let pagination = diagnostics.pagination().expect("diagnostics pagination");

        assert_eq!(pagination.default_page_size(), 1_000);
        assert_eq!(pagination.maximum_page_size(), 1_000);
    }
}
