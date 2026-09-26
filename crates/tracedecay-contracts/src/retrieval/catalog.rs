use schemars::JsonSchema;
use tracedecay_tool_catalog::{
    ApplicationSurfaceOperation, AvailabilityContract, BindingId, BindingSurface,
    CancellationContract, CancellationPoint, CapabilityId, CatalogContributionInputV1,
    CatalogContributionV1, ContributionContractRef, ContributionId, CoverageContractRef,
    DeadlineBehavior, DeadlineContract, DeniedDisclosurePolicy, EffectClass,
    ExecutableSchemaAuthority, LifecycleClass, OmissionContractRef, PaginationContract,
    PrivacyClass, ProfileId, ProtocolRevisionRange, RetrievalFamily,
    RetrievalPrimitiveManifestInputV1, RetrievalPrimitiveManifestV1, RetrieverId,
    RevalidationContract, RevalidationPoint, RoutingContractV1, SchemaId, SchemaRef,
    ScopeDimension, ScopeRequirement, ScoringContractRef, SortContract, SortContractId,
    StreamingContract, SurfaceBindingInputV1, SurfaceBindingV1, SurfaceOperationName, TemporalMode,
    TerminalState, TerminalStateContract,
};

use crate::capability_manifest::{
    ApplicationCapabilityManifestInput, application_capability_manifest,
};
use crate::error::ApplicationContractError;
use crate::handlers::{ApplicationHandlerDescriptor, ApplicationOperation};
use crate::result::ResultContractRef;
use crate::retrieval::DependencyDepthResultV1;
use crate::retrieval::callable_code_catalog::CALLABLE_CODE_DEFAULT_PAGE_SIZE;
use crate::retrieval::graph_report_surface::{
    DependencyDepthSurfaceRequestV1, DiagnoseResultV1, DiagnoseSurfaceRequestV1, DsmResultV1,
    DsmSurfaceRequestV1, GiniResultV1, GiniSurfaceRequestV1, HealthResultV1,
    HealthSurfaceRequestV1, TestMapResultV1, TestMapSurfaceRequestV1, TestRiskResultV1,
    TestRiskSurfaceRequestV1,
};
use crate::retrieval::primitive_surface::{
    ContextResultV1, ContextSurfaceRequestV1, ImpactResultV1, NodeDepthSurfaceRequestV1,
    NodeResultV1, NodeSurfaceRequestV1, PortOrderResultV1, PortOrderSurfaceRequestV1,
    PortStatusResultV1, PortStatusSurfaceRequestV1, RedundancyResultV1, RedundancySurfaceRequestV1,
    RenamePreviewPrimitiveOutcomeV1, RenamePreviewPrimitiveRequestV1, SimilarResultV1,
    SimilarSurfaceRequestV1, TodosResultV1, TodosSurfaceRequestV1,
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
    ImplementationRecord, SymbolGraphPage, SymbolPrimitiveRecord, SymbolRelationRecord,
    TypeHierarchyRecord,
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
/// Operations outside this primitive family, the callable-code queries
/// included, retain the inert page envelope's established value of
/// [`CALLABLE_CODE_DEFAULT_PAGE_SIZE`].
pub fn application_operation_default_page_size(operation: ApplicationSurfaceOperation) -> u32 {
    PRIMITIVE_READ_SPECS
        .iter()
        .find(|spec| spec.operation == operation.as_str())
        .map_or(CALLABLE_CODE_DEFAULT_PAGE_SIZE, |spec| {
            spec.default_page_size
        })
}

struct PrimitiveReadSpec {
    operation: &'static str,
    capability: &'static str,
    use_case: &'static str,
    default_page_size: u32,
    /// Bounded reads continue through `meta.cursor`; whole-project reports
    /// answer in one response and advertise no pagination.
    paginated: bool,
    deadline_millis: u64,
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
    // MCP and CLI callers cannot choose a page size, so these navigation reads
    // default to a page that holds a typical answer; `meta.cursor` continues.
    primitive_spec_with_default_page_size("code_signature_search", 50),
    primitive_spec_with_default_page_size("code_implementations", 20),
    primitive_spec_with_default_page_size("code_type_hierarchy", 100),
    primitive_spec_with_default_page_size("code_callers", 100),
    primitive_spec("context"),
    primitive_spec("node"),
    primitive_spec("impact"),
    primitive_spec("similar"),
    primitive_spec("redundancy"),
    primitive_spec("rename_preview"),
    primitive_spec("port_status"),
    primitive_spec("port_order"),
    primitive_spec("todos"),
    graph_report_spec("test_map"),
    graph_report_spec("test_risk"),
    graph_report_spec("gini"),
    graph_report_spec("dependency_depth"),
    graph_report_spec("health"),
    graph_report_spec("dsm"),
    graph_report_spec("diagnose"),
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
        // The project's graph-tool owner answers these for the tool surfaces
        // only; their typed results render as the established tool output.
        "context" | "node" | "impact" | "similar" | "redundancy" | "rename_preview"
        | "port_status" | "port_order" | "todos" | "test_map" | "test_risk" | "gini"
        | "dependency_depth" | "health" | "dsm" | "diagnose" => &CLI_MCP_PRIMITIVE_SURFACES,
        "health_read" | "storage_status" | "diagnostics_read" => &DASHBOARD_PRIMITIVE_SURFACES,
        _ => &PRE_DASHBOARD_PRIMITIVE_SURFACES,
    }
}

/// Similar and redundancy expose one current family schema. Callers already
/// use that schema, so the binding is the current protocol revision only,
/// not a revision window for a retired request shape. Do not mint a second
/// binding for the same spelling; `index_bindings` rejects duplicate
/// surface-operation keys.
fn clone_family_surface_bindings(
    capability_id: &CapabilityId,
    operation: &str,
    surfaces: &[BindingSurface],
) -> Result<(Vec<SurfaceBindingV1>, Vec<BindingId>), ApplicationContractError> {
    use crate::surface_binding::surface_name;

    let mut bindings = Vec::with_capacity(surfaces.len());
    let mut binding_ids = Vec::with_capacity(surfaces.len());
    for surface in surfaces.iter().copied() {
        let binding_id = BindingId::new(format!(
            "binding.{}.{}.v1",
            surface_name(surface),
            operation
        ))?;
        bindings.push(SurfaceBindingV1::new(SurfaceBindingInputV1 {
            binding_id: binding_id.clone(),
            capability_id: capability_id.clone(),
            surface,
            operation: SurfaceOperationName::new(operation)?,
            protocol_revisions: ProtocolRevisionRange::new(1, 1)?,
            required_features: Vec::new(),
        })?);
        binding_ids.push(binding_id);
    }
    Ok((bindings, binding_ids))
}

fn primitive_read_description(operation: &str) -> &'static str {
    match operation {
        "code_signature_search" => {
            "Find functions and methods by signature shape: `returns` (return-type substring), `params` (substrings that must all appear in the parameter list), or `is_async`; narrow with `scope.path_prefix`. At least one filter is required. Use symbol search for name or concept searches."
        }
        "code_implementations" => {
            "Find every type implementing a trait (`selector: {\"selector\": \"trait\", \"name\": ...}`) or every function or method with a name (`selector: {\"selector\": \"method\", \"name\": ...}`). Each match carries its exact source body. Use type_hierarchy to traverse extends and implements relationships from a known node ID."
        }
        "code_type_hierarchy" => {
            "Use for trait, interface, or class hierarchy questions before grepping `impl X for` or `extends X`: traverses the implementors and extenders of a type node ID up to `maximum_depth` (default 5). Use implementations when starting from a trait or method name."
        }
        "code_callers" => {
            "Who calls this: find references, usages, and call sites of a known symbol node ID up to `maximum_depth` (default 3). Coverage is partial when a call target cannot be resolved exactly. Use call_chain for the shortest call path between two known node IDs."
        }
        "redundancy" => {
            "Report bounded, token-verified exact and rename-normalized implementation families in the admitted repository. Results rank review candidates by repeated source bytes."
        }
        "session_lookup" => {
            "Return retrieval anchors for one exact session ID from the mounted session store. Use message_search when you need to find session content rather than look up a known session."
        }
        "qualified_name" => {
            "Resolve an exact qualified symbol name to matching symbol records and node IDs, including ambiguous matches. Use code_symbol_search for partial names or concepts."
        }
        "call_chain" => {
            "Find the shortest Calls path between two symbol node IDs, bounded by maximum depth. Obtain node IDs from code_symbol_search, qualified_name, or another graph read; use code_callers for an inbound neighborhood."
        }
        "file_dependents" => {
            "Find files containing callers of symbols in one project-relative file. Use code_callers when you need the dependent symbols and call edges instead of file paths."
        }
        "source_lines" => {
            "Read an exact byte span from the current indexed file and return a stable source anchor. Supply the file occurrence ID and span from code_exact_occurrence; use source_body when you already have a symbol node ID."
        }
        "source_body" => {
            "Read the current source body and line range for a symbol node ID returned by code_symbol_search, qualified_name, or another graph read. Use source_lines for an exact occurrence span."
        }
        "source_outline" => {
            "List indexed symbols in one project-relative file without reading their bodies. Use source_body with a returned node ID to inspect one symbol, or module_api for public symbols across a path."
        }
        "module_api" => {
            "List public indexed symbols in a project-relative file or directory tree. Use source_outline when you need every indexed symbol from one file, including non-public symbols."
        }
        "health_read" => {
            "Read the admitted project's serving status: ok, read_only, or degraded. Use storage_status for database size and page telemetry, or health_delta for generation-bound code-health changes."
        }
        "health_delta" => {
            "Compare the current generation's code-health score and dimensions with a prior after_cursor, optionally within a path prefix. Save the returned after_cursor for a later comparison; omit before_cursor to establish a baseline."
        }
        "storage_status" => {
            "Inspect the admitted project's graph-store status, read-only state, database size, page telemetry, and bounded size history. Use health_read when only serving availability matters."
        }
        "diagnostics_read" => {
            "Read retained diagnostics for the current indexed generation, scoped to the workspace or one file. This does not run a compiler or refresh diagnostics; use the project's build or typecheck when fresh post-edit results are required."
        }
        "test_map" => {
            "Map a source file's or symbol's callables to the tests that reach them within three call-graph hops. Coverage is static attribution, not executed coverage."
        }
        "test_risk" => {
            "Rank source symbols with weak or no static test attribution by complexity, fan-in, and churn."
        }
        "gini" => {
            "Measure how unevenly a metric (complexity, lines, fan-in, fan-out, or members) is distributed across files or symbols, with the top outliers."
        }
        "dependency_depth" => {
            "Report the longest file-level dependency chains and how far the deepest exceeds the ideal depth."
        }
        "health" => {
            "Score code health (0-10000) as the geometric mean of acyclicity, depth, equality, redundancy, modularity, and coverage discipline."
        }
        "dsm" => {
            "Summarize the file dependency design-structure matrix: density, directory clusters, and optionally the matrix itself."
        }
        "diagnose" => {
            "Map raw cargo, clippy, or rustc diagnostics to the smallest containing graph symbol and its callers, and publish them to the managed diagnostics store."
        }
        _ => "Read bounded data from the admitted project's current retained state.",
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
        paginated: true,
        deadline_millis: 10_000,
    }
}

/// A whole-project graph report. It keeps the two-minute interactive ceiling
/// these reports have always dispatched under: a report over every file in a
/// large repository is not a ten-second primitive read.
const fn graph_report_spec(operation: &'static str) -> PrimitiveReadSpec {
    PrimitiveReadSpec {
        operation,
        capability: operation,
        use_case: operation,
        default_page_size: CALLABLE_CODE_DEFAULT_PAGE_SIZE,
        paginated: false,
        deadline_millis: 120_000,
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
            if matches!(spec.operation, "similar" | "redundancy") {
                clone_family_surface_bindings(&capability_id, spec.operation, surfaces)?
            } else {
                match ApplicationSurfaceOperation::from_catalog_name(spec.operation) {
                    Some(operation) => current_application_bindings(
                        &capability_id,
                        operation,
                        surfaces.iter().copied(),
                    )?,
                    None => {
                        current_bindings(&capability_id, spec.operation, surfaces.iter().copied())?
                    }
                }
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
            })?);
            binding_ids.push(binding_id);
        }
        capabilities.push(application_capability_manifest(
            ApplicationCapabilityManifestInput {
                capability_id,
                use_case_id: tracedecay_tool_catalog::UseCaseId::new(format!(
                    "use-case.application.primitive.{}",
                    spec.use_case.replace('_', "-")
                ))?,
                routing: RoutingContractV1::new(
                    1,
                    format!("Read {}", spec.operation.replace('_', " ")),
                    primitive_read_description(spec.operation),
                    vec![format!("Read {}", spec.operation.replace('_', " "))],
                )?,
                request_schema: primitive_schema(spec.operation, "request")?,
                result_schema: primitive_schema(spec.operation, "result")?,
                effect: EffectClass::Read,
                scope: symbol_search_scope()?,
                denied_disclosure: DeniedDisclosurePolicy::Indistinguishable,
                privacy: PrivacyClass::ScopedMetadata,
                lifecycle: LifecycleClass::Resumable,
                streaming: StreamingContract::Unsupported,
                cancellation: CancellationContract::cooperative(vec![
                    CancellationPoint::BeforeAdmission,
                    CancellationPoint::BeforeRead,
                    CancellationPoint::DuringRead,
                ])?,
                deadline: DeadlineContract::new(
                    spec.deadline_millis,
                    DeadlineBehavior::ReturnOperationReceipt,
                )?,
                pagination: if spec.paginated {
                    Some(PaginationContract::new(
                        spec.default_page_size,
                        1_000,
                        60_000,
                    )?)
                } else {
                    None
                },
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
                profile_eligibility: application_profile_ids(primitive_profile_ids(
                    spec.operation,
                ))?,
                required_features: Vec::new(),
            },
        )?);
    }
    let contribution = CatalogContributionV1::new(CatalogContributionInputV1::new(
        ContributionId::new("contribution.application.primitive-reads")?,
        Vec::new(),
        capabilities,
        bindings,
    ))?;
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
        SymbolGraphPage<ImplementationRecord>
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
    add!("impact", NodeDepthSurfaceRequestV1, ImpactResultV1);
    add!("node", NodeSurfaceRequestV1, NodeResultV1);
    add!("similar", SimilarSurfaceRequestV1, SimilarResultV1);
    add!("redundancy", RedundancySurfaceRequestV1, RedundancyResultV1);
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
    add!("todos", TodosSurfaceRequestV1, TodosResultV1);
    add!("test_map", TestMapSurfaceRequestV1, TestMapResultV1);
    add!("test_risk", TestRiskSurfaceRequestV1, TestRiskResultV1);
    add!("gini", GiniSurfaceRequestV1, GiniResultV1);
    add!(
        "dependency_depth",
        DependencyDepthSurfaceRequestV1,
        DependencyDepthResultV1
    );
    add!("health", HealthSurfaceRequestV1, HealthResultV1);
    add!("dsm", DsmSurfaceRequestV1, DsmResultV1);
    add!("diagnose", DiagnoseSurfaceRequestV1, DiagnoseResultV1);
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
    })?);
    binding_ids.push(lsp_binding_id);
    let capability = application_capability_manifest(ApplicationCapabilityManifestInput {
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
    let contribution = CatalogContributionV1::new(
        CatalogContributionInputV1::new(
            ContributionId::new("contribution.application.symbol-search")?,
            Vec::new(),
            vec![capability],
            bindings,
        )
        .with_retrieval_primitives(vec![primitive]),
    )?;
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

    const ESTABLISHED_TOOL_PRIMITIVES: [&str; 9] = [
        "context",
        "node",
        "impact",
        "similar",
        "redundancy",
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
}
