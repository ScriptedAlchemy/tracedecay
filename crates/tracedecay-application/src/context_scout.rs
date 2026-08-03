//! Canonical Context Scout operations for PR13 CLI/MCP/HTTP surfaces.
//!
//! The application crate owns operation identity and catalog metadata only.
//! Exact-address authorization and durable mutation remain daemon authorities.

use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, BindingSurface, CancellationContract,
    CancellationPoint, CapabilityId, CapabilityManifestInputV1, CapabilityManifestV1,
    CatalogContributionInputV1, CatalogContributionV1, ContributionId, DeadlineBehavior,
    DeadlineContract, DeniedDisclosurePolicy, EffectClass, IdempotencyContract, LifecycleClass,
    PaginationContract, PrivacyClass, ProfileId, ReceiptContract, ReconciliationContract,
    RevalidationContract, RevalidationPoint, RoutingContractV1, SchemaId, SchemaRef,
    ScopeDimension, ScopeRequirement, StreamingContract, TerminalState, TerminalStateContract,
    UseCaseId,
};

use crate::current_bindings;
use crate::error::ApplicationContractError;
use crate::handlers::{ApplicationHandlerDescriptor, ApplicationOperation};
use crate::result::ResultContractRef;
use crate::retrieval::catalog::APPLICATION_DEFAULT_PROFILE_ID;

const SCOUT_SURFACES: [BindingSurface; 3] = [
    BindingSurface::Cli,
    BindingSurface::Mcp,
    BindingSurface::Http,
];

#[derive(Clone, Copy)]
struct ContextScoutOperationSpec {
    operation: &'static str,
    summary: &'static str,
    description: &'static str,
    effect: EffectClass,
    paginated: bool,
}

const CONTEXT_SCOUT_SPECS: [ContextScoutOperationSpec; 11] = [
    read_spec("context_scout_status", "Read Context Scout status"),
    read_spec("context_scout_recent", "Read recent Context Scout state"),
    read_spec("context_scout_explain", "Explain Context Scout state"),
    read_spec("context_scout_capability", "Read Context Scout capability"),
    read_spec("context_scout_budget", "Read Context Scout budget"),
    control_spec("context_scout_pause", "Pause Context Scout"),
    control_spec("context_scout_resume", "Resume Context Scout"),
    control_spec("context_scout_cancel", "Cancel Context Scout work"),
    control_spec("context_scout_claim", "Claim a Context Scout delivery"),
    control_spec("context_scout_delivery", "Record a Context Scout delivery"),
    control_spec("context_scout_feedback", "Record Context Scout feedback"),
];

const fn read_spec(operation: &'static str, summary: &'static str) -> ContextScoutOperationSpec {
    ContextScoutOperationSpec {
        operation,
        summary,
        description: "Execute the exact-address Context Scout read through the daemon-owned application authority.",
        effect: EffectClass::Read,
        paginated: false,
    }
}

const fn control_spec(operation: &'static str, summary: &'static str) -> ContextScoutOperationSpec {
    ContextScoutOperationSpec {
        operation,
        summary,
        description: "Execute the exact-address Context Scout control through the daemon-owned application authority.",
        effect: EffectClass::Administrative,
        paginated: false,
    }
}

pub fn context_scout_surface_catalog_contribution()
-> Result<CatalogContributionV1, ApplicationContractError> {
    let mut capabilities = Vec::with_capacity(CONTEXT_SCOUT_SPECS.len());
    let mut bindings = Vec::with_capacity(CONTEXT_SCOUT_SPECS.len() * SCOUT_SURFACES.len());
    for spec in &CONTEXT_SCOUT_SPECS {
        let is_effect = spec.effect.is_effect();
        let capability_id = capability_id(spec)?;
        let (spec_bindings, binding_ids) =
            current_bindings(&capability_id, spec.operation, SCOUT_SURFACES)?;
        bindings.extend(spec_bindings);
        capabilities.push(CapabilityManifestV1::new(CapabilityManifestInputV1 {
            capability_id,
            use_case_id: use_case_id(spec)?,
            routing: RoutingContractV1::new(
                1,
                spec.summary,
                spec.description,
                vec![format!("{} for this exact address", spec.summary)],
            )?,
            request_schema: request_schema(spec)?,
            result_schema: result_schema(spec)?,
            effect: spec.effect,
            scope: ScopeRequirement::new(vec![
                ScopeDimension::Project,
                ScopeDimension::Worktree,
                ScopeDimension::Session,
                ScopeDimension::Resource,
            ])?,
            authority: AuthorityRequirement::CapabilityGrantWithRevalidation,
            denied_disclosure: DeniedDisclosurePolicy::Indistinguishable,
            privacy: PrivacyClass::ScopedMetadata,
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
                15_000,
                if is_effect {
                    DeadlineBehavior::ReturnEffectReceipt
                } else {
                    DeadlineBehavior::ReturnOperationReceipt
                },
            )?,
            pagination: spec
                .paginated
                .then(|| PaginationContract::new(8, 32, 60_000))
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
        })?);
    }
    Ok(CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.application.context-scout-surface")?,
        depends_on: Vec::new(),
        capabilities,
        retrieval_primitives: Vec::new(),
        bindings,
    })?)
}

pub fn context_scout_surface_handler_descriptors()
-> Result<Vec<ApplicationHandlerDescriptor>, ApplicationContractError> {
    CONTEXT_SCOUT_SPECS
        .iter()
        .map(|spec| {
            ApplicationHandlerDescriptor::new(
                context_scout_surface_operation(spec.operation)?.ok_or(
                    ApplicationContractError::Inconsistent {
                        field: "Context Scout operation spec",
                    },
                )?,
                request_schema(spec)?,
                result_schema(spec)?,
            )
        })
        .collect()
}

pub fn context_scout_surface_operation(
    name: &str,
) -> Result<Option<ApplicationOperation>, ApplicationContractError> {
    CONTEXT_SCOUT_SPECS
        .iter()
        .find(|spec| spec.operation == name)
        .map(|spec| {
            Ok(ApplicationOperation::new(
                capability_id(spec)?,
                use_case_id(spec)?,
                ResultContractRef::from_schema(&result_schema(spec)?),
                true,
            ))
        })
        .transpose()
}

fn capability_id(
    spec: &ContextScoutOperationSpec,
) -> Result<CapabilityId, ApplicationContractError> {
    Ok(CapabilityId::new(format!(
        "capability.application.{}",
        spec.operation.replace('_', "-")
    ))?)
}

fn use_case_id(spec: &ContextScoutOperationSpec) -> Result<UseCaseId, ApplicationContractError> {
    Ok(UseCaseId::new(format!(
        "use-case.application.{}",
        spec.operation.replace('_', "-")
    ))?)
}

fn request_schema(spec: &ContextScoutOperationSpec) -> Result<SchemaRef, ApplicationContractError> {
    schema(spec, "request")
}

fn result_schema(spec: &ContextScoutOperationSpec) -> Result<SchemaRef, ApplicationContractError> {
    schema(spec, "result")
}

fn schema(
    spec: &ContextScoutOperationSpec,
    suffix: &str,
) -> Result<SchemaRef, ApplicationContractError> {
    Ok(SchemaRef::new(
        SchemaId::new(format!(
            "schema.application.{}.{}",
            spec.operation.replace('_', "-"),
            suffix
        ))?,
        1,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_exposes_every_scout_operation_on_cli_mcp_and_http_only() {
        let contribution = context_scout_surface_catalog_contribution().unwrap();
        assert_eq!(contribution.capabilities().len(), CONTEXT_SCOUT_SPECS.len());
        let routing_examples = contribution
            .capabilities()
            .iter()
            .flat_map(|capability| capability.routing().examples())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(routing_examples.len(), CONTEXT_SCOUT_SPECS.len());
        for spec in CONTEXT_SCOUT_SPECS {
            let capability = contribution
                .capabilities()
                .iter()
                .find(|capability| capability.capability_id() == &capability_id(&spec).unwrap())
                .expect("every Scout operation has one capability");
            assert_eq!(capability.effect(), spec.effect);
            if spec.effect.is_effect() {
                assert_eq!(capability.receipt(), ReceiptContract::DurableEffect);
                assert_eq!(capability.idempotency(), IdempotencyContract::Required);
                assert_eq!(
                    capability.reconciliation(),
                    ReconciliationContract::Required
                );
                assert_eq!(
                    capability.deadline().behavior(),
                    DeadlineBehavior::ReturnEffectReceipt
                );
                assert!(
                    capability
                        .cancellation()
                        .points()
                        .contains(&CancellationPoint::EffectInFlight)
                );
                assert!(
                    capability
                        .terminal_states()
                        .states()
                        .contains(&TerminalState::EffectUnknown)
                );
            } else {
                assert_eq!(capability.receipt(), ReceiptContract::Operation);
                assert_eq!(capability.idempotency(), IdempotencyContract::NotRequired);
                assert_eq!(
                    capability.reconciliation(),
                    ReconciliationContract::NotRequired
                );
                assert_eq!(
                    capability.deadline().behavior(),
                    DeadlineBehavior::ReturnOperationReceipt
                );
                assert!(
                    !capability
                        .terminal_states()
                        .states()
                        .contains(&TerminalState::EffectUnknown)
                );
            }
            let surfaces = contribution
                .bindings()
                .iter()
                .filter(|binding| binding.operation().as_str() == spec.operation)
                .map(|binding| binding.surface())
                .collect::<Vec<_>>();
            assert_eq!(surfaces.len(), SCOUT_SURFACES.len());
            for expected in SCOUT_SURFACES {
                assert!(surfaces.contains(&expected));
            }
        }
    }

    #[test]
    fn application_catalog_and_handlers_reach_every_scout_operation() {
        let contributions = crate::application_catalog_contributions().unwrap();
        let handlers = crate::application_handler_descriptors().unwrap();
        handlers.validate_against(&contributions).unwrap();

        for spec in CONTEXT_SCOUT_SPECS {
            let operation = context_scout_surface_operation(spec.operation)
                .unwrap()
                .expect("Scout operation is application-reachable");
            let handler = handlers
                .get(operation.use_case_id())
                .expect("Scout operation has one canonical handler");
            assert_eq!(handler.operation(), &operation);
        }
    }
}
