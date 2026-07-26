//! Public feedback read bindings for Plan 21 / Plan 37 surfaces.
//!
//! These bindings project the PR11 feedback-cycle result. They never create a
//! second finding store and never execute follow-up work.

use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, BindingId, BindingStatus, BindingSurface,
    CancellationContract, CancellationPoint, CapabilityId, CapabilityManifestInputV1,
    CapabilityManifestV1, CatalogContributionInputV1, CatalogContributionV1, ContributionId,
    DeadlineBehavior, DeadlineContract, DeniedDisclosurePolicy, EffectClass, IdempotencyContract,
    LifecycleClass, PaginationContract, PrivacyClass, ProfileId, ProtocolRevisionRange,
    ReceiptContract, ReconciliationContract, RevalidationContract, RevalidationPoint,
    RoutingContractV1, SchemaId, SchemaRef, ScopeDimension, ScopeRequirement, StreamingContract,
    SurfaceBindingInputV1, SurfaceBindingV1, SurfaceOperationName, TerminalState,
    TerminalStateContract, UnavailabilityReason, UseCaseId,
};

use crate::error::ApplicationContractError;
use crate::handlers::{ApplicationHandlerDescriptor, ApplicationOperation};
use crate::result::ResultContractRef;
use crate::retrieval::catalog::APPLICATION_DEFAULT_PROFILE_ID;

use super::{
    CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1, CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
    FEEDBACK_DIAGNOSTICS_CAPABILITY_ID_V1, FEEDBACK_DIAGNOSTICS_USE_CASE_ID_V1,
    FEEDBACK_EXPAND_CAPABILITY_ID_V1, FEEDBACK_EXPAND_USE_CASE_ID_V1,
    FEEDBACK_GET_CAPABILITY_ID_V1, FEEDBACK_GET_USE_CASE_ID_V1, FEEDBACK_LIST_CAPABILITY_ID_V1,
    FEEDBACK_LIST_USE_CASE_ID_V1, FeedbackReadOperationsV1, GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
    GITHUB_REVIEW_INGEST_USE_CASE_ID_V1, PROXIMITY_CAPABILITY_ID_V1, PROXIMITY_USE_CASE_ID_V1,
};

struct FeedbackSurfaceSpec {
    capability: &'static str,
    use_case: &'static str,
    request_schema: &'static str,
    result_schema: &'static str,
    operation: &'static str,
    summary: &'static str,
    description: &'static str,
    example: &'static str,
    paginated: bool,
    surfaces: &'static [BindingSurface],
}

/// Canonical feedback reads remain callable through the shared PR12
/// CLI/MCP/HTTP transport owner.
const PR12_TRANSPORT_SURFACES: [BindingSurface; 3] = [
    BindingSurface::Cli,
    BindingSurface::Mcp,
    BindingSurface::Http,
];

/// PR13 advisory producers retain the shared transport reads and additionally
/// project the same canonical result through the mounted LSP/native host path.
/// Hook delivery is host-registration metadata rather than a callable catalog
/// surface, and dashboard binding remains owned by PR14.
const PR13_ADVISORY_SURFACES: [BindingSurface; 4] = [
    BindingSurface::Cli,
    BindingSurface::Mcp,
    BindingSurface::Http,
    BindingSurface::Lsp,
];

const FEEDBACK_SPECS: [FeedbackSurfaceSpec; 10] = [
    FeedbackSurfaceSpec {
        capability: FEEDBACK_DIAGNOSTICS_CAPABILITY_ID_V1,
        use_case: FEEDBACK_DIAGNOSTICS_USE_CASE_ID_V1,
        request_schema: "schema.application.feedback.diagnostics.request",
        result_schema: "schema.application.feedback.diagnostics.result",
        operation: "feedback_diagnostics",
        summary: "Read feedback diagnostics",
        description: "Read the canonical completed feedback cycle for the authorized branch head.",
        example: "Read diagnostics from the current branch feedback cycle",
        paginated: false,
        surfaces: &PR12_TRANSPORT_SURFACES,
    },
    FeedbackSurfaceSpec {
        capability: FEEDBACK_GET_CAPABILITY_ID_V1,
        use_case: FEEDBACK_GET_USE_CASE_ID_V1,
        request_schema: "schema.application.feedback.get.request",
        result_schema: "schema.application.feedback.get.result",
        operation: "feedback_get",
        summary: "Get a feedback finding",
        description: "Fetch one authorized feedback finding by durable identity.",
        example: "Get this feedback finding",
        paginated: false,
        surfaces: &PR12_TRANSPORT_SURFACES,
    },
    FeedbackSurfaceSpec {
        capability: FEEDBACK_EXPAND_CAPABILITY_ID_V1,
        use_case: FEEDBACK_EXPAND_USE_CASE_ID_V1,
        request_schema: "schema.application.feedback.expand.request",
        result_schema: "schema.application.feedback.expand.result",
        operation: "feedback_expand",
        summary: "Expand feedback evidence",
        description: "Expand authorized anchors and evidence for one feedback finding.",
        example: "Expand this feedback finding",
        paginated: false,
        surfaces: &PR12_TRANSPORT_SURFACES,
    },
    FeedbackSurfaceSpec {
        capability: FEEDBACK_LIST_CAPABILITY_ID_V1,
        use_case: FEEDBACK_LIST_USE_CASE_ID_V1,
        request_schema: "schema.application.feedback.list.request",
        result_schema: "schema.application.feedback.list.result",
        operation: "feedback_list",
        summary: "List feedback findings",
        description: "List authorized feedback findings with Plan 05 cursors.",
        example: "List feedback findings for this branch",
        paginated: true,
        surfaces: &PR12_TRANSPORT_SURFACES,
    },
    FeedbackSurfaceSpec {
        capability: "capability.application.feedback.impact",
        use_case: "use-case.application.feedback.impact",
        request_schema: "schema.application.feedback.impact.request",
        result_schema: "schema.application.feedback.impact.result",
        operation: "feedback_impact",
        summary: "Read feedback impact",
        description: "Project the canonical impact and affected-test state from an authorized completed feedback cycle.",
        example: "Read impact from the current branch feedback cycle",
        paginated: false,
        surfaces: &PR12_TRANSPORT_SURFACES,
    },
    FeedbackSurfaceSpec {
        capability: "capability.application.feedback.affected-tests",
        use_case: "use-case.application.feedback.affected-tests",
        request_schema: "schema.application.feedback.affected-tests.request",
        result_schema: "schema.application.feedback.affected-tests.result",
        operation: "affected_tests",
        summary: "Read affected tests",
        description: "Project affected-test state from an authorized completed feedback cycle.",
        example: "Read affected tests from this feedback cycle",
        paginated: true,
        surfaces: &PR12_TRANSPORT_SURFACES,
    },
    FeedbackSurfaceSpec {
        capability: "capability.application.feedback.test-results",
        use_case: "use-case.application.feedback.test-results",
        request_schema: "schema.application.feedback.test-results.request",
        result_schema: "schema.application.feedback.test-results.result",
        operation: "test_results",
        summary: "Read recent test results",
        description: "Read the latest daemon-retained managed test result for the admitted project root.",
        example: "Read the latest managed test results",
        paginated: false,
        surfaces: &PR12_TRANSPORT_SURFACES,
    },
    FeedbackSurfaceSpec {
        capability: GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
        use_case: GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
        request_schema: "schema.application.feedback.github-review-ingest.request",
        result_schema: "schema.application.feedback.github-review-ingest.result",
        operation: "github_review_ingest",
        summary: "Ingest existing GitHub review evidence",
        description: "Read allowlisted existing GitHub review comments and threads without a write path.",
        example: "Read existing review threads for this pull request",
        paginated: true,
        surfaces: &PR13_ADVISORY_SURFACES,
    },
    FeedbackSurfaceSpec {
        capability: CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
        use_case: CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
        request_schema: "schema.application.feedback.ci-failure-localize.request",
        result_schema: "schema.application.feedback.ci-failure-localize.result",
        operation: "ci_failure_localize",
        summary: "Localize a reported CI failure",
        description: "Map anchored CI evidence to exact branch, generation, symbol, caller, and test evidence without running CI.",
        example: "Localize this reported CI failure",
        paginated: false,
        surfaces: &PR13_ADVISORY_SURFACES,
    },
    FeedbackSurfaceSpec {
        capability: PROXIMITY_CAPABILITY_ID_V1,
        use_case: PROXIMITY_USE_CASE_ID_V1,
        request_schema: "schema.application.feedback.proximity.request",
        result_schema: "schema.application.feedback.proximity.result",
        operation: "feedback_proximity",
        summary: "Inspect advisory concurrent-work proximity",
        description: "Return immediate or configured-threshold proximity evidence without locks, scheduling, or continuation.",
        example: "Inspect concurrent-work proximity for this branch",
        paginated: false,
        surfaces: &PR13_ADVISORY_SURFACES,
    },
];

/// Specs with concrete internal application owners.
///
/// Registration proves the handler exists; it does not prove that a host can
/// construct the request. Transport availability is narrowed independently
/// below.
const REGISTERED_FEEDBACK_HANDLER_SPECS: [usize; 10] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];

pub fn feedback_surface_catalog_contribution()
-> Result<CatalogContributionV1, ApplicationContractError> {
    let handlers = feedback_surface_handler_descriptors()?;
    feedback_surface_catalog_contribution_for_handlers(&handlers)
}

fn feedback_surface_catalog_contribution_for_handlers(
    handlers: &[ApplicationHandlerDescriptor],
) -> Result<CatalogContributionV1, ApplicationContractError> {
    let mut capabilities = Vec::with_capacity(FEEDBACK_SPECS.len());
    let mut bindings =
        Vec::with_capacity(FEEDBACK_SPECS.iter().map(|spec| spec.surfaces.len()).sum());

    for spec in &FEEDBACK_SPECS {
        let capability_id = CapabilityId::new(spec.capability)?;
        let mut binding_ids = Vec::new();
        let callable =
            spec.operation == "test_results" && handlers.contains(&handler_descriptor(spec)?);
        if callable {
            binding_ids.reserve(spec.surfaces.len());
            for &surface in spec.surfaces {
                let binding_id = BindingId::new(format!(
                    "binding.{}.{}.{}",
                    match surface {
                        BindingSurface::Cli => "cli",
                        BindingSurface::Mcp => "mcp",
                        BindingSurface::Http => "http",
                        BindingSurface::Lsp => "lsp",
                        BindingSurface::Dashboard => "dashboard",
                    },
                    spec.operation,
                    "v1"
                ))?;
                bindings.push(SurfaceBindingV1::new(SurfaceBindingInputV1 {
                    binding_id: binding_id.clone(),
                    capability_id: capability_id.clone(),
                    surface,
                    operation: SurfaceOperationName::new(spec.operation)?,
                    protocol_revisions: ProtocolRevisionRange::new(1, 1)?,
                    required_features: Vec::new(),
                    status: BindingStatus::Current,
                    alias_of: None,
                })?);
                binding_ids.push(binding_id);
            }
        }
        capabilities.push(capability(spec, capability_id, binding_ids, callable)?);
    }

    Ok(CatalogContributionV1::new(CatalogContributionInputV1 {
        contribution_id: ContributionId::new("contribution.application.feedback-surface")?,
        depends_on: Vec::new(),
        capabilities,
        retrieval_primitives: Vec::new(),
        bindings,
    })?)
}

pub fn feedback_surface_handler_descriptors()
-> Result<Vec<ApplicationHandlerDescriptor>, ApplicationContractError> {
    REGISTERED_FEEDBACK_HANDLER_SPECS
        .iter()
        .map(|index| {
            FEEDBACK_SPECS
                .get(*index)
                .ok_or(ApplicationContractError::Inconsistent {
                    field: "registered feedback handler spec",
                })
                .and_then(handler_descriptor)
        })
        .collect()
}

pub fn feedback_surface_operation(
    name: &str,
) -> Result<Option<ApplicationOperation>, ApplicationContractError> {
    FEEDBACK_SPECS
        .iter()
        .find(|spec| spec.operation == name)
        .map(application_operation)
        .transpose()
}

/// Exact PR12 operation set consumed by `FeedbackReadService`.
pub fn feedback_read_operations() -> Result<FeedbackReadOperationsV1, ApplicationContractError> {
    FeedbackReadOperationsV1::new(
        application_operation(&FEEDBACK_SPECS[0])?,
        application_operation(&FEEDBACK_SPECS[1])?,
        application_operation(&FEEDBACK_SPECS[2])?,
        application_operation(&FEEDBACK_SPECS[3])?,
    )
}

fn capability(
    spec: &FeedbackSurfaceSpec,
    capability_id: CapabilityId,
    binding_ids: Vec<BindingId>,
    callable: bool,
) -> Result<CapabilityManifestV1, ApplicationContractError> {
    Ok(CapabilityManifestV1::new(CapabilityManifestInputV1 {
        capability_id,
        use_case_id: UseCaseId::new(spec.use_case)?,
        routing: RoutingContractV1::new(
            1,
            spec.summary,
            spec.description,
            vec![spec.example.to_owned()],
        )?,
        request_schema: schema(spec.request_schema)?,
        result_schema: schema(spec.result_schema)?,
        effect: EffectClass::Read,
        scope: ScopeRequirement::new(vec![ScopeDimension::Project, ScopeDimension::Branch])?,
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
        deadline: DeadlineContract::new(15_000, DeadlineBehavior::ReturnOperationReceipt)?,
        pagination: if spec.paginated {
            Some(PaginationContract::new(10, 100, 60_000)?)
        } else {
            None
        },
        idempotency: IdempotencyContract::NotRequired,
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
            TerminalState::Partial,
        ])?,
        availability: if callable {
            AvailabilityContract::Available
        } else {
            AvailabilityContract::Unavailable {
                reason: UnavailabilityReason::NotImplemented,
            }
        },
        binding_ids,
        profile_eligibility: if callable {
            vec![ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID)?]
        } else {
            Vec::new()
        },
        required_features: Vec::new(),
    })?)
}

fn handler_descriptor(
    spec: &FeedbackSurfaceSpec,
) -> Result<ApplicationHandlerDescriptor, ApplicationContractError> {
    let result_schema = schema(spec.result_schema)?;
    ApplicationHandlerDescriptor::new(
        application_operation(spec)?,
        schema(spec.request_schema)?,
        result_schema,
    )
}

fn application_operation(
    spec: &FeedbackSurfaceSpec,
) -> Result<ApplicationOperation, ApplicationContractError> {
    let result_schema = schema(spec.result_schema)?;
    Ok(ApplicationOperation::new(
        CapabilityId::new(spec.capability)?,
        UseCaseId::new(spec.use_case)?,
        ResultContractRef::from_schema(&result_schema),
        true,
    ))
}

fn schema(id: &str) -> Result<SchemaRef, ApplicationContractError> {
    Ok(SchemaRef::new(SchemaId::new(id)?, 1, 8_192)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_advertises_only_feedback_operations_with_host_routes() {
        let contribution = feedback_surface_catalog_contribution().expect("contribution");
        let mut names: Vec<_> = contribution
            .bindings()
            .iter()
            .map(|binding| binding.operation().as_str().to_owned())
            .collect();
        names.sort();
        names.dedup();
        assert_eq!(names, vec!["test_results".to_owned()]);
    }

    #[test]
    fn internal_feedback_handlers_do_not_imply_transport_availability() {
        let unavailable =
            feedback_surface_catalog_contribution_for_handlers(&[]).expect("unavailable catalog");
        for spec in &FEEDBACK_SPECS {
            let capability = unavailable
                .capabilities()
                .iter()
                .find(|capability| capability.capability_id().as_str() == spec.capability)
                .expect("declared feedback capability");
            assert!(!capability.availability().is_callable());
            assert!(capability.binding_ids().is_empty());

            let handler = handler_descriptor(spec).expect("registered feedback handler");
            let available = feedback_surface_catalog_contribution_for_handlers(&[handler])
                .expect("available catalog");
            let capability = available
                .capabilities()
                .iter()
                .find(|capability| capability.capability_id().as_str() == spec.capability)
                .expect("registered feedback capability");
            if spec.operation == "test_results" {
                assert!(capability.availability().is_callable());
                assert_eq!(capability.binding_ids().len(), spec.surfaces.len());
            } else {
                assert!(!capability.availability().is_callable());
                assert!(capability.binding_ids().is_empty());
            }
        }
    }
}
