//! Catalog contracts for retained memory, session, and workflow operations.
//!
//! These records sit beside the application boundary. Transport adapters keep
//! their public wire schemas, but resolve the operation identity here before
//! invoking the retained owner.

use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, BindingDeprecation, BindingId, BindingStatus,
    BindingSurface, CancellationContract, CancellationPoint, CapabilityId,
    CapabilityManifestInputV1, CapabilityManifestV1, CatalogContributionInputV1,
    CatalogContributionV1, ContributionId, DeadlineBehavior, DeadlineContract,
    DeniedDisclosurePolicy, EffectClass, IdempotencyContract, LifecycleClass, PaginationContract,
    PrivacyClass, ProfileId, ProtocolRevisionRange, ReceiptContract, ReconciliationContract,
    RevalidationContract, RevalidationPoint, RoutingContractV1, SchemaId, SchemaRef,
    ScopeDimension, ScopeRequirement, StreamingContract, SurfaceBindingInputV1, SurfaceBindingV1,
    SurfaceOperationName, TerminalState, TerminalStateContract, UseCaseId,
};

use crate::error::ApplicationContractError;
use crate::handlers::{ApplicationHandlerDescriptor, ApplicationOperation};
use crate::result::ResultContractRef;
use crate::retrieval::catalog::APPLICATION_DEFAULT_PROFILE_ID;
use crate::surface_name;

mod memory;
mod session;
mod workflow;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetainedSurfaceOperation {
    FactStore,
    FactFeedback,
    MemoryStatus,
    SessionRefresh,
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
    LcmPreflight,
    LcmCompress,
    LcmSessionBoundary,
    SessionStart,
    SessionEnd,
}

impl RetainedSurfaceOperation {
    pub const ALL: [Self; 19] = [
        Self::FactStore,
        Self::FactFeedback,
        Self::MemoryStatus,
        Self::SessionRefresh,
        Self::MessageSearch,
        Self::SessionsFor,
        Self::LcmStatus,
        Self::LcmDoctor,
        Self::LcmLoadSession,
        Self::LcmGrep,
        Self::LcmDescribe,
        Self::LcmExpand,
        Self::LcmExpandQuery,
        Self::LcmPreflight,
        Self::LcmCompress,
        Self::LcmSessionBoundary,
        Self::SessionStart,
        Self::SessionEnd,
        Self::Workflows,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FactStore => "fact_store",
            Self::FactFeedback => "fact_feedback",
            Self::MemoryStatus => "memory_status",
            Self::SessionRefresh => "session_refresh",
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
            Self::LcmPreflight => "lcm_preflight",
            Self::LcmCompress => "lcm_compress",
            Self::LcmSessionBoundary => "lcm_session_boundary",
            Self::SessionStart => "session_start",
            Self::SessionEnd => "session_end",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.strip_prefix("tracedecay_").unwrap_or(name);
        surface_specs()
            .into_iter()
            .find(|spec| spec.operation.as_str() == name)
            .map(|spec| spec.operation)
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
}

fn surface_specs() -> Vec<&'static RetainedSurfaceSpec> {
    memory::SPECS
        .iter()
        .chain(session::SPECS.iter())
        .chain(workflow::SPECS.iter())
        .collect()
}

const SURFACES: [BindingSurface; 2] = [BindingSurface::Cli, BindingSurface::Mcp];

pub fn retained_surface_catalog_contribution()
-> Result<CatalogContributionV1, ApplicationContractError> {
    let specs = surface_specs();
    let mut capabilities = Vec::with_capacity(specs.len());
    let mut bindings = Vec::with_capacity(specs.len() * SURFACES.len());
    for spec in specs {
        let capability_id = CapabilityId::new(capability_id(spec.operation))?;
        let mut binding_ids = Vec::with_capacity(SURFACES.len());
        for surface in SURFACES {
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
                status: if matches!(
                    spec.operation,
                    RetainedSurfaceOperation::SessionStart | RetainedSurfaceOperation::SessionEnd
                ) {
                    BindingStatus::Deprecated {
                        deprecation: BindingDeprecation::new(2)?,
                    }
                } else {
                    BindingStatus::Current
                },
                alias_of: None,
            })?);
            binding_ids.push(binding_id);
        }
        capabilities.push(capability(spec, capability_id, binding_ids)?);
    }
    Ok(CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new(
            "contribution.application.retained-memory-session-workflow",
        )?,
        depends_on: Vec::new(),
        capabilities,
        retrieval_primitives: Vec::new(),
        bindings,
    })?)
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
                TerminalState::EffectUnknown,
                TerminalState::Partial,
            ]
        } else {
            vec![
                TerminalState::Completed,
                TerminalState::Cancelled,
                TerminalState::TimedOut,
                TerminalState::Failed,
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
    fn retained_families_have_callable_cli_and_mcp_owners() {
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
            surface_specs().len() * SURFACES.len()
        );
        for spec in surface_specs() {
            assert_eq!(
                RetainedSurfaceOperation::from_name(spec.operation.as_str()),
                Some(spec.operation)
            );
            assert_eq!(
                RetainedSurfaceOperation::from_name(&format!(
                    "tracedecay_{}",
                    spec.operation.as_str()
                )),
                Some(spec.operation)
            );
        }
        assert_eq!(
            surface_specs()
                .into_iter()
                .map(|spec| spec.operation)
                .collect::<Vec<_>>(),
            RetainedSurfaceOperation::ALL
        );
    }
}
