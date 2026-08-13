//! Catalog contracts for retained memory, session, and workflow operations.
//!
//! These records sit beside the application boundary. Transport adapters keep
//! their public wire schemas, but resolve the operation identity here before
//! invoking the retained owner.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, BindingId, BindingStatus, BindingSurface,
    CancellationContract, CancellationPoint, CapabilityId, CapabilityManifestInputV1,
    CapabilityManifestV1, CatalogContributionInputV1, CatalogContributionV1, CodecBindingKey,
    ContributionId, DeadlineBehavior, DeadlineContract, DeniedDisclosurePolicy, EffectClass,
    ExecutableBindingAvailabilityV1, ExecutableBindingRegistryV1, ExecutableBindingV1,
    ExecutableSchemaAuthority, IdempotencyContract, LifecycleClass, OperationId,
    PaginationContract, PrivacyClass, ProfileId, ProtocolRevisionRange, ReceiptContract,
    ReconciliationContract, RevalidationContract, RevalidationPoint, RouteExposureV1,
    RoutingContractV1, SchemaId, SchemaRef, ScopeDimension, ScopeRequirement, ServiceId,
    StreamingContract, SurfaceBindingInputV1, SurfaceBindingV1, SurfaceOperationName,
    TerminalState, TerminalStateContract, UseCaseId,
};

use crate::error::ApplicationContractError;
use crate::handlers::{ApplicationHandlerDescriptor, ApplicationOperation};
use crate::result::ResultContractRef;
use crate::retrieval::catalog::APPLICATION_DEFAULT_PROFILE_ID;
use crate::surface_name;

mod automation;
mod evidence;
mod memory;
mod sdk;
mod service;
mod session;
mod workflow;

pub use evidence::*;
pub use sdk::*;
pub use service::*;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RetainedSurfaceOperation {
    FactStoreCurate,
    FactStoreAdd,
    FactStoreSearch,
    FactStoreProbe,
    FactStoreRelated,
    FactStoreReason,
    FactStoreContradict,
    FactStoreGet,
    FactStoreUpdate,
    FactStoreRemove,
    FactStoreList,
    FactFeedback,
    MemoryStatus,
    /// Legacy broad MCP translator; never a current catalog capability.
    SessionRefresh,
    SessionRefreshStatus,
    SessionRefreshCancel,
    SessionRefreshBegin,
    MessageSearch,
    SessionsFor,
    Workflows,
    LcmStatus,
    LcmDoctor,
    LcmLoadSession,
    LcmGrep,
    LcmDescribe,
    LcmExpand,
    LcmExpandQuery,
}

/// Whether an SDK caller must supply a stable transport request identity.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SdkRequestIdControlV1 {
    ServerMinted,
    Required,
}

/// Semantic validation applied after structural result decoding.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SdkResultSemanticsV1 {
    SchemaOnly,
    FactStoreCurateTerminal,
}

/// SDK-only transport and terminal controls derived from the application owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetainedSdkOperationContractV1 {
    pub request_id: SdkRequestIdControlV1,
    pub result_semantics: SdkResultSemanticsV1,
}

impl RetainedSdkOperationContractV1 {
    pub const DEFAULT: Self = Self {
        request_id: SdkRequestIdControlV1::ServerMinted,
        result_semantics: SdkResultSemanticsV1::SchemaOnly,
    };
}

impl RetainedSurfaceOperation {
    /// Canonical catalog operations. The broad `session_refresh` translator is
    /// intentionally not a catalog operation.
    pub const ALL: [Self; 26] = [
        Self::FactStoreCurate,
        Self::FactStoreAdd,
        Self::FactStoreSearch,
        Self::FactStoreProbe,
        Self::FactStoreRelated,
        Self::FactStoreReason,
        Self::FactStoreContradict,
        Self::FactStoreGet,
        Self::FactStoreUpdate,
        Self::FactStoreRemove,
        Self::FactStoreList,
        Self::FactFeedback,
        Self::MemoryStatus,
        Self::SessionRefreshStatus,
        Self::SessionRefreshCancel,
        Self::SessionRefreshBegin,
        Self::MessageSearch,
        Self::SessionsFor,
        Self::LcmStatus,
        Self::LcmDoctor,
        Self::LcmLoadSession,
        Self::LcmGrep,
        Self::LcmDescribe,
        Self::LcmExpand,
        Self::LcmExpandQuery,
        Self::Workflows,
    ];

    /// Operations with a current callable transport. Daemon grants, HTTP
    /// routes, and the SDK all derive from this exact mounted set.
    pub const CALLABLE: [Self; 26] = [
        Self::FactStoreCurate,
        Self::FactStoreAdd,
        Self::FactStoreSearch,
        Self::FactStoreProbe,
        Self::FactStoreRelated,
        Self::FactStoreReason,
        Self::FactStoreContradict,
        Self::FactStoreGet,
        Self::FactStoreUpdate,
        Self::FactStoreRemove,
        Self::FactStoreList,
        Self::FactFeedback,
        Self::MemoryStatus,
        Self::SessionRefreshStatus,
        Self::SessionRefreshCancel,
        Self::SessionRefreshBegin,
        Self::MessageSearch,
        Self::SessionsFor,
        Self::LcmStatus,
        Self::LcmDoctor,
        Self::LcmLoadSession,
        Self::LcmGrep,
        Self::LcmDescribe,
        Self::LcmExpand,
        Self::LcmExpandQuery,
        Self::Workflows,
    ];

    /// Every current retained action has an exact project-open production
    /// adapter. SDK clients invoke the operation-selected routes.
    pub const SDK_EXECUTABLE: [Self; 26] = [
        Self::FactStoreCurate,
        Self::FactStoreAdd,
        Self::FactStoreSearch,
        Self::FactStoreProbe,
        Self::FactStoreRelated,
        Self::FactStoreReason,
        Self::FactStoreContradict,
        Self::FactStoreGet,
        Self::FactStoreUpdate,
        Self::FactStoreRemove,
        Self::FactStoreList,
        Self::FactFeedback,
        Self::MemoryStatus,
        Self::SessionRefreshStatus,
        Self::SessionRefreshCancel,
        Self::SessionRefreshBegin,
        Self::MessageSearch,
        Self::SessionsFor,
        Self::LcmStatus,
        Self::LcmDoctor,
        Self::LcmLoadSession,
        Self::LcmGrep,
        Self::LcmDescribe,
        Self::LcmExpand,
        Self::LcmExpandQuery,
        Self::Workflows,
    ];

    pub const fn is_callable(self) -> bool {
        !matches!(self, Self::SessionRefresh)
    }

    /// Additional SDK controls that cannot live in the bounds-only operation body.
    pub const fn sdk_operation_contract(self) -> RetainedSdkOperationContractV1 {
        match self {
            Self::FactStoreCurate => RetainedSdkOperationContractV1 {
                request_id: SdkRequestIdControlV1::Required,
                result_semantics: SdkResultSemanticsV1::FactStoreCurateTerminal,
            },
            _ => RetainedSdkOperationContractV1::DEFAULT,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FactStoreCurate => "fact_store_curate",
            Self::FactStoreAdd => "fact_store_add",
            Self::FactStoreSearch => "fact_store_search",
            Self::FactStoreProbe => "fact_store_probe",
            Self::FactStoreRelated => "fact_store_related",
            Self::FactStoreReason => "fact_store_reason",
            Self::FactStoreContradict => "fact_store_contradict",
            Self::FactStoreGet => "fact_store_get",
            Self::FactStoreUpdate => "fact_store_update",
            Self::FactStoreRemove => "fact_store_remove",
            Self::FactStoreList => "fact_store_list",
            Self::FactFeedback => "fact_feedback",
            Self::MemoryStatus => "memory_status",
            Self::SessionRefresh => "session_refresh",
            Self::SessionRefreshStatus => "session_refresh_status",
            Self::SessionRefreshCancel => "session_refresh_cancel",
            Self::SessionRefreshBegin => "session_refresh_begin",
            Self::MessageSearch => "message_search",
            Self::SessionsFor => "sessions_for",
            Self::Workflows => "workflows",
            Self::LcmStatus => "lcm_status",
            Self::LcmDoctor => "lcm_doctor",
            Self::LcmLoadSession => "lcm_load_session",
            Self::LcmGrep => "lcm_grep",
            Self::LcmDescribe => "lcm_describe",
            Self::LcmExpand => "lcm_expand",
            Self::LcmExpandQuery => "lcm_expand_query",
        }
    }

    /// Parse an exact catalog/HTTP operation segment without a tool prefix.
    pub fn from_operation_name(name: &str) -> Option<Self> {
        match name {
            "session_refresh" => return Some(Self::SessionRefresh),
            _ => {}
        }
        surface_specs()
            .into_iter()
            .find(|spec| !spec.surfaces.is_empty() && spec.operation.as_str() == name)
            .map(|spec| spec.operation)
    }

    /// Parse an exact MCP/CLI tool name without accepting a bare operation.
    pub fn from_tool_name(name: &str) -> Option<Self> {
        Self::from_operation_name(name.strip_prefix("tracedecay_")?)
    }
}

pub(super) struct RetainedSurfaceSpec {
    pub(super) operation: RetainedSurfaceOperation,
    pub(super) summary: &'static str,
    pub(super) description: &'static str,
    pub(super) example: &'static str,
    pub(super) effect: EffectClass,
    pub(super) scope: &'static [ScopeDimension],
    pub(super) paginated: bool,
    pub(super) surfaces: &'static [BindingSurface],
}

fn surface_specs() -> Vec<&'static RetainedSurfaceSpec> {
    automation::SPECS
        .iter()
        .chain(memory::SPECS.iter())
        .chain(session::SPECS.iter())
        .chain(workflow::SPECS.iter())
        .collect()
}

/// Every callable retained operation reaches the same typed application owner
/// from HTTP, MCP, and the dynamic `tracedecay tool` CLI. Broad fact-store and
/// session-refresh tools translate their action to one of these exact bindings
/// before dispatch; the catalog does not fabricate separate public tools.
pub(super) const CURRENT_SURFACES: &[BindingSurface] = &[
    BindingSurface::Http,
    BindingSurface::Cli,
    BindingSurface::Mcp,
];
pub fn retained_surface_catalog_contribution()
-> Result<CatalogContributionV1, ApplicationContractError> {
    let specs = surface_specs();
    let mut capabilities = Vec::with_capacity(specs.len());
    let mut bindings =
        Vec::with_capacity(specs.iter().map(|spec| spec.surfaces.len()).sum::<usize>());
    for spec in specs {
        let capability_id = CapabilityId::new(capability_id(spec.operation))?;
        let mut binding_ids = Vec::with_capacity(spec.surfaces.len());
        for &surface in spec.surfaces {
            let binding_id = BindingId::new(format!(
                "binding.{}.{}.v1",
                surface_name(surface),
                spec.operation.as_str()
            ))?;
            bindings.push(SurfaceBindingV1::new(SurfaceBindingInputV1 {
                binding_id: binding_id.clone(),
                capability_id: capability_id.clone(),
                surface,
                operation: SurfaceOperationName::new(spec.operation.as_str())?,
                protocol_revisions: ProtocolRevisionRange::new(1, 1)?,
                required_features: Vec::new(),
                status: BindingStatus::Current,
                alias_of: None,
            })?);
            binding_ids.push(binding_id);
        }
        capabilities.push(capability(spec, capability_id, binding_ids)?);
    }
    let contribution = CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new(
            "contribution.application.retained-memory-session-workflow",
        )?,
        depends_on: Vec::new(),
        capabilities,
        retrieval_primitives: Vec::new(),
        bindings,
    })?;
    let schemas = retained_surface_executable_schemas(&contribution)?;
    Ok(contribution.with_executable_schemas(schemas)?)
}

/// Daemon-owned public HTTP bindings for retained V2 operations with a
/// project-opened execution port and exact raw-handler proof.
pub fn retained_surface_executable_binding_registry()
-> Result<ExecutableBindingRegistryV1, ApplicationContractError> {
    let contribution = retained_surface_catalog_contribution()?;
    let service_id = ServiceId::new("service.application.retained")?;
    let mut bindings = Vec::with_capacity(RetainedSurfaceOperation::SDK_EXECUTABLE.len());
    for operation in RetainedSurfaceOperation::SDK_EXECUTABLE {
        let capability_id = CapabilityId::new(capability_id(operation))?;
        let manifest = contribution
            .capabilities()
            .iter()
            .find(|manifest| manifest.capability_id() == &capability_id)
            .ok_or(ApplicationContractError::Inconsistent {
                field: "retained executable capability",
            })?;
        let schema = contribution.executable_schema(&capability_id).ok_or(
            ApplicationContractError::Inconsistent {
                field: "retained executable schema",
            },
        )?;
        let http_binding = contribution
            .bindings()
            .iter()
            .find(|binding| {
                binding.capability_id() == &capability_id
                    && binding.surface() == BindingSurface::Http
            })
            .ok_or(ApplicationContractError::Inconsistent {
                field: "retained HTTP binding",
            })?;
        bindings.push(ExecutableBindingAvailabilityV1::available(
            ExecutableBindingV1::daemon_owned(
                manifest,
                OperationId::new(format!("operation.application.{}", operation.as_str()))?,
                service_id.clone(),
                schema.request_schema().clone(),
                schema.result_schema().clone(),
                CodecBindingKey::new(format!(
                    "codec.application.retained.{}.json.v1",
                    operation.as_str()
                ))?,
                RouteExposureV1::Public {
                    binding_id: http_binding.binding_id().clone(),
                    route_path: format!("/application/retained/{}", operation.as_str()),
                },
            )?,
        ));
    }
    Ok(ExecutableBindingRegistryV1::new(bindings)?)
}

fn retained_surface_executable_schemas(
    contribution: &CatalogContributionV1,
) -> Result<Vec<ExecutableSchemaAuthority>, ApplicationContractError> {
    Ok(vec![
        retained_surface_executable_schema::<FactStoreCurateRequestV1, AutomationRunResultV1>(
            contribution,
            RetainedSurfaceOperation::FactStoreCurate,
            "tracedecay_application::retained_surfaces::FactStoreCurateRequestV1",
            "tracedecay_application::retained_surfaces::AutomationRunResultV1",
        )?,
        retained_surface_executable_schema::<FactStoreAddRequestV1, FactStoreAddResultV1>(
            contribution,
            RetainedSurfaceOperation::FactStoreAdd,
            "tracedecay_application::retained_surfaces::FactStoreAddRequestV1",
            "tracedecay_application::retained_surfaces::FactStoreAddResultV1",
        )?,
        retained_surface_executable_schema::<FactStoreSearchRequestV1, FactStoreSearchResultV1>(
            contribution,
            RetainedSurfaceOperation::FactStoreSearch,
            "tracedecay_application::retained_surfaces::FactStoreSearchRequestV1",
            "tracedecay_application::retained_surfaces::FactStoreSearchResultV1",
        )?,
        retained_surface_executable_schema::<FactStoreProbeRequestV1, FactStoreProbeResultV1>(
            contribution,
            RetainedSurfaceOperation::FactStoreProbe,
            "tracedecay_application::retained_surfaces::FactStoreProbeRequestV1",
            "tracedecay_application::retained_surfaces::FactStoreProbeResultV1",
        )?,
        retained_surface_executable_schema::<FactStoreRelatedRequestV1, FactStoreRelatedResultV1>(
            contribution,
            RetainedSurfaceOperation::FactStoreRelated,
            "tracedecay_application::retained_surfaces::FactStoreRelatedRequestV1",
            "tracedecay_application::retained_surfaces::FactStoreRelatedResultV1",
        )?,
        retained_surface_executable_schema::<FactStoreReasonRequestV1, FactStoreReasonResultV1>(
            contribution,
            RetainedSurfaceOperation::FactStoreReason,
            "tracedecay_application::retained_surfaces::FactStoreReasonRequestV1",
            "tracedecay_application::retained_surfaces::FactStoreReasonResultV1",
        )?,
        retained_surface_executable_schema::<
            FactStoreContradictRequestV1,
            FactStoreContradictResultV1,
        >(
            contribution,
            RetainedSurfaceOperation::FactStoreContradict,
            "tracedecay_application::retained_surfaces::FactStoreContradictRequestV1",
            "tracedecay_application::retained_surfaces::FactStoreContradictResultV1",
        )?,
        retained_surface_executable_schema::<FactStoreGetRequestV1, FactStoreGetResultV1>(
            contribution,
            RetainedSurfaceOperation::FactStoreGet,
            "tracedecay_application::retained_surfaces::FactStoreGetRequestV1",
            "tracedecay_application::retained_surfaces::FactStoreGetResultV1",
        )?,
        retained_surface_executable_schema::<FactStoreUpdateRequestV1, FactStoreUpdateResultV1>(
            contribution,
            RetainedSurfaceOperation::FactStoreUpdate,
            "tracedecay_application::retained_surfaces::FactStoreUpdateRequestV1",
            "tracedecay_application::retained_surfaces::FactStoreUpdateResultV1",
        )?,
        retained_surface_executable_schema::<FactStoreRemoveRequestV1, FactStoreRemoveResultV1>(
            contribution,
            RetainedSurfaceOperation::FactStoreRemove,
            "tracedecay_application::retained_surfaces::FactStoreRemoveRequestV1",
            "tracedecay_application::retained_surfaces::FactStoreRemoveResultV1",
        )?,
        retained_surface_executable_schema::<FactStoreListRequestV1, FactStoreListResultV1>(
            contribution,
            RetainedSurfaceOperation::FactStoreList,
            "tracedecay_application::retained_surfaces::FactStoreListRequestV1",
            "tracedecay_application::retained_surfaces::FactStoreListResultV1",
        )?,
        retained_surface_executable_schema::<FactFeedbackRequestV1, FactFeedbackResultV1>(
            contribution,
            RetainedSurfaceOperation::FactFeedback,
            "tracedecay_application::retained_surfaces::FactFeedbackRequestV1",
            "tracedecay_application::retained_surfaces::FactFeedbackResultV1",
        )?,
        retained_surface_executable_schema::<MemoryStatusRequestV1, MemoryStatusResultV1>(
            contribution,
            RetainedSurfaceOperation::MemoryStatus,
            "tracedecay_application::retained_surfaces::MemoryStatusRequestV1",
            "tracedecay_application::retained_surfaces::MemoryStatusResultV1",
        )?,
        retained_surface_executable_schema::<
            SessionRefreshActionRequestV1,
            SessionRefreshStatusResultV1,
        >(
            contribution,
            RetainedSurfaceOperation::SessionRefreshStatus,
            "tracedecay_application::retained_surfaces::SessionRefreshActionRequestV1",
            "tracedecay_application::retained_surfaces::SessionRefreshStatusResultV1",
        )?,
        retained_surface_executable_schema::<
            SessionRefreshActionRequestV1,
            SessionRefreshCancelResultV1,
        >(
            contribution,
            RetainedSurfaceOperation::SessionRefreshCancel,
            "tracedecay_application::retained_surfaces::SessionRefreshActionRequestV1",
            "tracedecay_application::retained_surfaces::SessionRefreshCancelResultV1",
        )?,
        retained_surface_executable_schema::<
            SessionRefreshActionRequestV1,
            SessionRefreshBeginResultV1,
        >(
            contribution,
            RetainedSurfaceOperation::SessionRefreshBegin,
            "tracedecay_application::retained_surfaces::SessionRefreshActionRequestV1",
            "tracedecay_application::retained_surfaces::SessionRefreshBeginResultV1",
        )?,
        retained_surface_executable_schema::<MessageSearchRequestV1, MessageSearchResultV1>(
            contribution,
            RetainedSurfaceOperation::MessageSearch,
            "tracedecay_application::retained_surfaces::MessageSearchRequestV1",
            "tracedecay_application::retained_surfaces::MessageSearchResultV1",
        )?,
        retained_surface_executable_schema::<SessionsForRequestV1, SessionsForResultV1>(
            contribution,
            RetainedSurfaceOperation::SessionsFor,
            "tracedecay_application::retained_surfaces::SessionsForRequestV1",
            "tracedecay_application::retained_surfaces::SessionsForResultV1",
        )?,
        retained_surface_executable_schema::<LcmStatusRequestV1, LcmStatusResultV1>(
            contribution,
            RetainedSurfaceOperation::LcmStatus,
            "tracedecay_application::retained_surfaces::LcmStatusRequestV1",
            "tracedecay_application::retained_surfaces::LcmStatusResultV1",
        )?,
        retained_surface_executable_schema::<LcmDoctorRequestV1, LcmDoctorResultV1>(
            contribution,
            RetainedSurfaceOperation::LcmDoctor,
            "tracedecay_application::retained_surfaces::LcmDoctorRequestV1",
            "tracedecay_application::retained_surfaces::LcmDoctorResultV1",
        )?,
        retained_surface_executable_schema::<LcmLoadSessionRequestV1, LcmLoadSessionResultV1>(
            contribution,
            RetainedSurfaceOperation::LcmLoadSession,
            "tracedecay_application::retained_surfaces::LcmLoadSessionRequestV1",
            "tracedecay_application::retained_surfaces::LcmLoadSessionResultV1",
        )?,
        retained_surface_executable_schema::<LcmGrepRequestV1, LcmGrepResultV1>(
            contribution,
            RetainedSurfaceOperation::LcmGrep,
            "tracedecay_application::retained_surfaces::LcmGrepRequestV1",
            "tracedecay_application::retained_surfaces::LcmGrepResultV1",
        )?,
        retained_surface_executable_schema::<LcmDescribeRequestV1, LcmDescribeResultV1>(
            contribution,
            RetainedSurfaceOperation::LcmDescribe,
            "tracedecay_application::retained_surfaces::LcmDescribeRequestV1",
            "tracedecay_application::retained_surfaces::LcmDescribeResultV1",
        )?,
        retained_surface_executable_schema::<LcmExpandRequestV1, LcmExpandResultV1>(
            contribution,
            RetainedSurfaceOperation::LcmExpand,
            "tracedecay_application::retained_surfaces::LcmExpandRequestV1",
            "tracedecay_application::retained_surfaces::LcmExpandResultV1",
        )?,
        retained_surface_executable_schema::<LcmExpandQueryRequestV1, LcmExpandQueryResultV1>(
            contribution,
            RetainedSurfaceOperation::LcmExpandQuery,
            "tracedecay_application::retained_surfaces::LcmExpandQueryRequestV1",
            "tracedecay_application::retained_surfaces::LcmExpandQueryResultV1",
        )?,
        retained_surface_executable_schema::<WorkflowsRequestV1, WorkflowsResultV1>(
            contribution,
            RetainedSurfaceOperation::Workflows,
            "tracedecay_application::retained_surfaces::WorkflowsRequestV1",
            "tracedecay_application::retained_surfaces::WorkflowsResultV1",
        )?,
    ])
}

fn retained_surface_executable_schema<Request, Response>(
    contribution: &CatalogContributionV1,
    operation: RetainedSurfaceOperation,
    request_rust_type_path: &'static str,
    result_rust_type_path: &'static str,
) -> Result<ExecutableSchemaAuthority, ApplicationContractError>
where
    Request: JsonSchema,
    Response: JsonSchema,
{
    let capability_id = CapabilityId::new(capability_id(operation))?;
    let manifest = contribution
        .capabilities()
        .iter()
        .find(|manifest| manifest.capability_id() == &capability_id)
        .ok_or(ApplicationContractError::Inconsistent {
            field: "retained executable schema capability",
        })?;
    Ok(ExecutableSchemaAuthority::for_types_at_paths::<
        Request,
        Response,
    >(
        manifest, request_rust_type_path, result_rust_type_path
    )?)
}

pub fn retained_surface_handler_descriptors()
-> Result<Vec<ApplicationHandlerDescriptor>, ApplicationContractError> {
    surface_specs()
        .into_iter()
        .map(handler_descriptor)
        .collect()
}

pub fn retained_surface_application_operation(
    operation: RetainedSurfaceOperation,
) -> Result<ApplicationOperation, ApplicationContractError> {
    let spec = surface_specs()
        .into_iter()
        .find(|spec| spec.operation == operation)
        .ok_or(ApplicationContractError::Inconsistent {
            field: "retained surface operation",
        })?;
    application_operation(spec)
}

/// Verify that a successful retained terminal still belongs to the selected
/// operation and authenticated HTTP envelope before an adapter serializes it.
pub fn retained_surface_outcome_matches_terminal(
    operation: RetainedSurfaceOperation,
    request_id: &crate::RequestId,
    scope: &crate::ResolvedScope,
    outcome: &crate::ApplicationOutcome<RetainedSurfaceResultV1>,
) -> bool {
    if !service::outcome_matches_operation(operation, outcome) {
        return false;
    }
    let Ok(application_operation) = retained_surface_application_operation(operation) else {
        return false;
    };
    let Some(expected_effect_class) = surface_specs()
        .into_iter()
        .find(|spec| spec.operation == operation)
        .map(|spec| spec.effect)
    else {
        return false;
    };
    match outcome {
        crate::ApplicationOutcome::Evidence(_) => expected_effect_class == EffectClass::Read,
        crate::ApplicationOutcome::Effect(effect) => {
            effect.effect_class == expected_effect_class
                && effect.receipt.effect_class == expected_effect_class
                && effect.receipt.request_id == *request_id
                && effect.receipt.operation == *application_operation.use_case_id()
                && effect.receipt.scope == *scope
        }
        crate::ApplicationOutcome::Preview(_) => false,
    }
}

/// Verify an admitted retained problem against the exact selected effect.
///
/// Partial effects require the independently authenticated retained scope that
/// accompanied the daemon terminal; generic unscoped problems fail closed.
pub fn retained_surface_problem_matches_terminal(
    operation: RetainedSurfaceOperation,
    request_id: &crate::RequestId,
    scope: Option<&crate::ResolvedScope>,
    problem: &crate::ApplicationProblem,
) -> bool {
    let crate::ApplicationProblem::PartialEffect {
        committed_receipt, ..
    } = problem
    else {
        return true;
    };
    let Ok(application_operation) = retained_surface_application_operation(operation) else {
        return false;
    };
    let Some(expected_effect_class) = surface_specs()
        .into_iter()
        .find(|spec| spec.operation == operation)
        .map(|spec| spec.effect)
    else {
        return false;
    };
    let Some(scope) = scope else {
        return false;
    };
    retained_surface_operation_is_effect(operation)
        && committed_receipt.effect_class == expected_effect_class
        && committed_receipt.request_id == *request_id
        && committed_receipt.operation == *application_operation.use_case_id()
        && committed_receipt.scope == *scope
}

fn capability(
    spec: &RetainedSurfaceSpec,
    capability_id: CapabilityId,
    binding_ids: Vec<BindingId>,
) -> Result<CapabilityManifestV1, ApplicationContractError> {
    let is_effect = spec.effect.is_effect();
    Ok(CapabilityManifestV1::new(CapabilityManifestInputV1 {
        capability_id,
        use_case_id: UseCaseId::new(use_case_id(spec.operation))?,
        routing: RoutingContractV1::new(
            1,
            spec.summary,
            spec.description,
            vec![spec.example.to_owned()],
        )?,
        request_schema: schema(spec.operation, "request")?,
        result_schema: schema(spec.operation, "result")?,
        effect: spec.effect,
        scope: ScopeRequirement::new(spec.scope.to_vec())?,
        authority: AuthorityRequirement::CapabilityGrantWithRevalidation,
        denied_disclosure: DeniedDisclosurePolicy::Indistinguishable,
        privacy: PrivacyClass::Sensitive,
        lifecycle: LifecycleClass::Resumable,
        streaming: StreamingContract::Unsupported,
        cancellation: CancellationContract::cooperative(if is_effect {
            vec![
                CancellationPoint::BeforeAdmission,
                CancellationPoint::BeforeEffect,
                CancellationPoint::EffectInFlight,
                CancellationPoint::Reconciling,
                CancellationPoint::AfterCommit,
            ]
        } else {
            vec![
                CancellationPoint::BeforeAdmission,
                CancellationPoint::BeforeRead,
                CancellationPoint::DuringRead,
            ]
        })?,
        deadline: DeadlineContract::new(
            30_000,
            if is_effect {
                DeadlineBehavior::ReturnEffectReceipt
            } else {
                DeadlineBehavior::ReturnOperationReceipt
            },
        )?,
        pagination: spec
            .paginated
            .then(|| PaginationContract::new(20, 200, 262_144))
            .transpose()?,
        idempotency: if is_effect {
            IdempotencyContract::Required
        } else {
            IdempotencyContract::NotRequired
        },
        inverse: if is_effect {
            tracedecay_tool_catalog::InverseContract::Unavailable {
                reason: tracedecay_tool_catalog::InverseUnavailableReason::NoShippedInverse,
            }
        } else {
            tracedecay_tool_catalog::InverseContract::NotApplicable
        },
        authority_revalidation: RevalidationContract::required(vec![
            RevalidationPoint::Authority,
            RevalidationPoint::Scope,
            RevalidationPoint::Policy,
            RevalidationPoint::Configuration,
            RevalidationPoint::ExpectedState,
        ])?,
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
        terminal_states: TerminalStateContract::new(if is_effect {
            vec![
                TerminalState::Completed,
                TerminalState::Cancelled,
                TerminalState::TimedOut,
                TerminalState::Failed,
                TerminalState::Unavailable,
                TerminalState::EffectUnknown,
                TerminalState::Partial,
            ]
        } else {
            vec![
                TerminalState::Completed,
                TerminalState::Cancelled,
                TerminalState::TimedOut,
                TerminalState::Failed,
                TerminalState::Unavailable,
                TerminalState::Partial,
            ]
        })?,
        availability: AvailabilityContract::Available,
        binding_ids,
        profile_eligibility: vec![ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID)?],
        required_features: Vec::new(),
    })?)
}

fn handler_descriptor(
    spec: &RetainedSurfaceSpec,
) -> Result<ApplicationHandlerDescriptor, ApplicationContractError> {
    ApplicationHandlerDescriptor::new(
        application_operation(spec)?,
        schema(spec.operation, "request")?,
        schema(spec.operation, "result")?,
    )
}

fn application_operation(
    spec: &RetainedSurfaceSpec,
) -> Result<ApplicationOperation, ApplicationContractError> {
    let result_schema = schema(spec.operation, "result")?;
    Ok(ApplicationOperation::new(
        CapabilityId::new(capability_id(spec.operation))?,
        UseCaseId::new(use_case_id(spec.operation))?,
        ResultContractRef::from_schema(&result_schema),
        true,
    ))
}

fn schema(
    operation: RetainedSurfaceOperation,
    direction: &str,
) -> Result<SchemaRef, ApplicationContractError> {
    Ok(SchemaRef::new(
        SchemaId::new(format!(
            "schema.application.retained.{}.{direction}",
            operation.as_str().replace('_', "-")
        ))?,
        1,
    )?)
}

fn capability_id(operation: RetainedSurfaceOperation) -> String {
    format!(
        "capability.application.retained.{}",
        operation.as_str().replace('_', "-")
    )
}

fn use_case_id(operation: RetainedSurfaceOperation) -> String {
    format!(
        "use-case.application.retained.{}",
        operation.as_str().replace('_', "-")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_families_have_catalog_handler_parity() {
        let contribution = retained_surface_catalog_contribution().expect("contribution");
        let handlers = crate::ApplicationHandlerDescriptors::new(
            retained_surface_handler_descriptors().expect("handlers"),
        )
        .expect("handler index");
        handlers
            .validate_against(std::slice::from_ref(&contribution))
            .expect("catalog/handler parity");
        assert_eq!(contribution.capabilities().len(), surface_specs().len());
        assert_eq!(
            contribution.bindings().len(),
            surface_specs()
                .iter()
                .map(|spec| spec.surfaces.len())
                .sum::<usize>()
        );
        for spec in surface_specs() {
            let exposed = (!spec.surfaces.is_empty()).then_some(spec.operation);
            assert_eq!(
                RetainedSurfaceOperation::from_operation_name(spec.operation.as_str()),
                exposed
            );
            assert_eq!(
                RetainedSurfaceOperation::from_tool_name(&format!(
                    "tracedecay_{}",
                    spec.operation.as_str()
                )),
                exposed
            );
        }
        assert_eq!(
            surface_specs()
                .into_iter()
                .map(|spec| spec.operation)
                .collect::<Vec<_>>(),
            RetainedSurfaceOperation::ALL
        );
        assert_eq!(
            surface_specs()
                .into_iter()
                .map(|spec| spec.operation)
                .filter(|operation| operation.is_callable())
                .collect::<Vec<_>>(),
            RetainedSurfaceOperation::CALLABLE
        );
    }

    #[test]
    fn duplicate_session_refresh_aliases_are_not_v2_operations() {
        for name in [
            "session_refresh_start",
            "session_refresh_join",
            "session_refresh_resume",
        ] {
            assert_eq!(RetainedSurfaceOperation::from_operation_name(name), None);
        }
    }

    #[test]
    fn broad_fact_store_translator_is_not_a_v2_operation() {
        for name in ["fact_store", "tracedecay_fact_store"] {
            assert_eq!(RetainedSurfaceOperation::from_operation_name(name), None);
            assert_eq!(RetainedSurfaceOperation::from_tool_name(name), None);
        }
        for name in [
            "fact_store_add",
            "fact_store_search",
            "fact_store_probe",
            "fact_store_related",
            "fact_store_reason",
            "fact_store_contradict",
            "fact_store_get",
            "fact_store_update",
            "fact_store_remove",
            "fact_store_list",
            "fact_feedback",
        ] {
            assert!(RetainedSurfaceOperation::from_operation_name(name).is_some());
            assert!(RetainedSurfaceOperation::from_tool_name(name).is_none());
            assert!(
                RetainedSurfaceOperation::from_tool_name(&format!("tracedecay_{name}")).is_some()
            );
        }
    }

    #[test]
    fn fact_store_curate_is_the_only_public_automation_launcher() {
        let contribution = retained_surface_catalog_contribution().expect("contribution");
        let registry = retained_surface_executable_binding_registry().expect("registry");
        let operation = RetainedSurfaceOperation::FactStoreCurate;
        let capability = CapabilityId::new(capability_id(operation)).expect("capability id");
        let request_type = "tracedecay_application::retained_surfaces::FactStoreCurateRequestV1";
        let result_type = "tracedecay_application::retained_surfaces::AutomationRunResultV1";

        assert!(RetainedSurfaceOperation::ALL.contains(&operation));
        assert!(RetainedSurfaceOperation::CALLABLE.contains(&operation));
        assert!(RetainedSurfaceOperation::SDK_EXECUTABLE.contains(&operation));
        let catalog_launchers = contribution
            .executable_schemas()
            .iter()
            .filter(|authority| authority.result_schema().rust_type_path() == result_type)
            .collect::<Vec<_>>();
        assert_eq!(catalog_launchers.len(), 1);
        assert_eq!(catalog_launchers[0].capability_id(), &capability);
        assert_eq!(
            catalog_launchers[0].request_schema().rust_type_path(),
            request_type
        );
        assert_eq!(
            catalog_launchers[0].request_schema().body()["properties"]
                .as_object()
                .expect("curator request properties")
                .keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>(),
            ["fact_review_limit", "min_confidence_millionths"]
                .into_iter()
                .collect()
        );
        assert_eq!(
            catalog_launchers[0].request_schema().body()["additionalProperties"],
            serde_json::Value::Bool(false)
        );
        let executable_launchers = registry
            .iter()
            .filter_map(ExecutableBindingAvailabilityV1::binding)
            .filter(|binding| binding.result_schema().rust_type_path() == result_type)
            .collect::<Vec<_>>();
        assert_eq!(executable_launchers.len(), 1);
        assert_eq!(
            executable_launchers[0].operation_id().as_str(),
            "operation.application.fact_store_curate"
        );
        assert_eq!(
            executable_launchers[0].request_schema().rust_type_path(),
            request_type
        );
        assert_eq!(
            contribution
                .bindings()
                .iter()
                .filter(|binding| binding.capability_id() == &capability)
                .map(SurfaceBindingV1::surface)
                .collect::<std::collections::BTreeSet<_>>(),
            CURRENT_SURFACES.iter().copied().collect()
        );
        assert_eq!(
            automation::SPECS
                .iter()
                .map(|spec| spec.operation)
                .collect::<Vec<_>>(),
            [operation]
        );
        assert_eq!(
            RetainedSurfaceOperation::from_operation_name("fact_store_curate"),
            Some(operation)
        );
        assert_eq!(
            RetainedSurfaceOperation::from_tool_name("tracedecay_fact_store_curate"),
            Some(operation)
        );
        assert_eq!(
            operation.sdk_operation_contract(),
            RetainedSdkOperationContractV1 {
                request_id: SdkRequestIdControlV1::Required,
                result_semantics: SdkResultSemanticsV1::FactStoreCurateTerminal,
            }
        );
        retained_surface_application_operation(operation).expect("registered application use case");
    }

    #[test]
    fn exact_retained_tools_publish_their_mounted_cli_bindings() {
        let contribution = retained_surface_catalog_contribution().expect("contribution");
        for operation in RetainedSurfaceOperation::CALLABLE {
            let capability = CapabilityId::new(capability_id(operation)).expect("capability id");
            let surfaces = contribution
                .bindings()
                .iter()
                .filter(|binding| binding.capability_id() == &capability)
                .map(SurfaceBindingV1::surface)
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(
                surfaces,
                [
                    BindingSurface::Http,
                    BindingSurface::Cli,
                    BindingSurface::Mcp
                ]
                .into_iter()
                .collect(),
                "{} must expose the three mounted transports",
                operation.as_str(),
            );
        }
    }

    #[test]
    fn every_mounted_retained_action_is_sdk_executable() {
        let registry = retained_surface_executable_binding_registry().expect("registry");
        assert_eq!(
            RetainedSurfaceOperation::SDK_EXECUTABLE,
            RetainedSurfaceOperation::CALLABLE
        );
        assert_eq!(
            registry.iter().count(),
            RetainedSurfaceOperation::SDK_EXECUTABLE.len()
        );
        for operation in RetainedSurfaceOperation::SDK_EXECUTABLE {
            let operation_id = format!("operation.application.{}", operation.as_str());
            let binding = registry
                .iter()
                .find(|availability| availability.operation_id().as_str() == operation_id)
                .and_then(|availability| availability.binding())
                .expect("raw-proof retained action must have a daemon-owned binding");
            assert!(matches!(
                binding.exposure(),
                RouteExposureV1::Public { route_path, .. }
                    if route_path == &format!("/application/retained/{}", operation.as_str())
            ));
        }
    }
}
