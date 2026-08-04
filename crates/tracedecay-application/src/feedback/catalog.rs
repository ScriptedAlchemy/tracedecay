//! Public feedback read bindings for Plan 21 / Plan 37 surfaces.
//!
//! These bindings project the PR11 feedback-cycle result. They never create a
//! second finding store and never execute follow-up work.

use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, BindingId, BindingSurface, CancellationContract,
    CancellationPoint, CapabilityId, CapabilityManifestInputV1, CapabilityManifestV1,
    CatalogContributionInputV1, CatalogContributionV1, ContributionId, DeadlineBehavior,
    DeadlineContract, DeniedDisclosurePolicy, EffectClass, IdempotencyContract, LifecycleClass,
    PaginationContract, PrivacyClass, ReceiptContract, ReconciliationContract,
    RevalidationContract, RevalidationPoint, RoutingContractV1, SchemaId, SchemaRef,
    ScopeDimension, ScopeRequirement, StreamingContract, TerminalState, TerminalStateContract,
    UnavailabilityReason, UseCaseId,
};

use crate::current_bindings;
use crate::error::ApplicationContractError;
use crate::handlers::{ApplicationHandlerDescriptor, ApplicationOperation};
use crate::result::ResultContractRef;
use crate::retrieval::catalog::{
    APPLICATION_COMPACT_PROFILE_ID, APPLICATION_DEFAULT_PROFILE_ID, application_profile_ids,
};

use super::{
    ADVISORY_CYCLE_CAPABILITY_ID_V1, ADVISORY_CYCLE_USE_CASE_ID_V1,
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

/// Canonical feedback reads retain their PR12 transports and gain the PR14
/// dashboard adapter without changing the application owner.
const FEEDBACK_READ_SURFACES: [BindingSurface; 4] = [
    BindingSurface::Cli,
    BindingSurface::Mcp,
    BindingSurface::Http,
    BindingSurface::Dashboard,
];

/// PR13 advisory producers retain the shared transport reads and additionally
/// project the same canonical result through the mounted LSP/native host path.
/// Hook delivery is host-registration metadata rather than a callable catalog
/// surface. Dashboard consumes their results through the canonical feedback
/// readers above rather than advertising producer operations it cannot invoke.
const PR13_ADVISORY_SURFACES: [BindingSurface; 4] = [
    BindingSurface::Cli,
    BindingSurface::Mcp,
    BindingSurface::Http,
    BindingSurface::Lsp,
];

/// Producer contributions are application-callable only through the combined
/// cycle. They remain visible capability metadata for LSP/native projection,
/// but do not create three independent network orchestration paths.
const PR13_PROVIDER_CONTRIBUTION_SURFACES: [BindingSurface; 0] = [];

const FEEDBACK_SPECS: [FeedbackSurfaceSpec; 11] = [
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
        surfaces: &FEEDBACK_READ_SURFACES,
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
        surfaces: &FEEDBACK_READ_SURFACES,
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
        surfaces: &FEEDBACK_READ_SURFACES,
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
        surfaces: &FEEDBACK_READ_SURFACES,
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
        surfaces: &FEEDBACK_READ_SURFACES,
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
        surfaces: &FEEDBACK_READ_SURFACES,
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
        surfaces: &FEEDBACK_READ_SURFACES,
    },
    FeedbackSurfaceSpec {
        capability: ADVISORY_CYCLE_CAPABILITY_ID_V1,
        use_case: ADVISORY_CYCLE_USE_CASE_ID_V1,
        request_schema: "schema.application.feedback.advisory-cycle.request",
        result_schema: "schema.application.feedback.advisory-cycle.result",
        operation: "feedback_advisory_cycle",
        summary: "Run the advisory feedback cycle",
        description: "Run one authorized four-pillar feedback cycle and return a daemon-minted canonical read handle.",
        example: "Run the complete advisory cycle for this saved document",
        paginated: false,
        surfaces: &PR13_ADVISORY_SURFACES,
    },
    FeedbackSurfaceSpec {
        capability: GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
        use_case: GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
        request_schema: "schema.application.feedback.github-review-ingest.request",
        result_schema: "schema.application.feedback.github-review-ingest.result",
        operation: "github_review_ingest",
        summary: "Ingest existing GitHub review evidence",
        description: "Contribute allowlisted existing GitHub review comments and threads to feedback_advisory_cycle without an independent write or orchestration path.",
        example: "Read existing review threads for this pull request",
        paginated: true,
        surfaces: &PR13_PROVIDER_CONTRIBUTION_SURFACES,
    },
    FeedbackSurfaceSpec {
        capability: CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
        use_case: CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
        request_schema: "schema.application.feedback.ci-failure-localize.request",
        result_schema: "schema.application.feedback.ci-failure-localize.result",
        operation: "ci_failure_localize",
        summary: "Localize a reported CI failure",
        description: "Contribute anchored CI localization to feedback_advisory_cycle without running CI or exposing an independent orchestration path.",
        example: "Localize this reported CI failure",
        paginated: false,
        surfaces: &PR13_PROVIDER_CONTRIBUTION_SURFACES,
    },
    FeedbackSurfaceSpec {
        capability: PROXIMITY_CAPABILITY_ID_V1,
        use_case: PROXIMITY_USE_CASE_ID_V1,
        request_schema: "schema.application.feedback.proximity.request",
        result_schema: "schema.application.feedback.proximity.result",
        operation: "feedback_proximity",
        summary: "Inspect advisory concurrent-work proximity",
        description: "Contribute immediate or configured-threshold proximity evidence to feedback_advisory_cycle without locks, scheduling, continuation, or an independent orchestration path.",
        example: "Inspect concurrent-work proximity for this branch",
        paginated: false,
        surfaces: &PR13_PROVIDER_CONTRIBUTION_SURFACES,
    },
];

/// Specs with concrete internal application owners.
///
/// Registration proves the handler exists; it does not prove that a host can
/// construct the request. Transport availability is narrowed independently
/// below.
const REGISTERED_FEEDBACK_HANDLER_SPECS: [usize; 11] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];

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
        // Handler registration is the executable-owner proof. Keep this
        // symmetric with `feedback_surface_handler_descriptors`: narrowing a
        // registered handler here leaves root composition with a handler for
        // an unavailable capability and breaks the catalog/handler bijection.
        let callable = handlers.contains(&handler_descriptor(spec)?);
        let mut binding_ids = Vec::new();
        if callable {
            let (spec_bindings, spec_binding_ids) = current_bindings(
                &capability_id,
                spec.operation,
                spec.surfaces.iter().copied(),
            )?;
            bindings.extend(spec_bindings);
            binding_ids = spec_binding_ids;
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
        profile_eligibility: if callable && !spec.surfaces.is_empty() {
            application_profile_ids(if spec.operation == "test_results" {
                &[
                    APPLICATION_DEFAULT_PROFILE_ID,
                    APPLICATION_COMPACT_PROFILE_ID,
                ]
            } else {
                &[APPLICATION_DEFAULT_PROFILE_ID]
            })?
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
    Ok(SchemaRef::new(SchemaId::new(id)?, 1)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_advertises_every_transport_exposed_feedback_operation() {
        let contribution = feedback_surface_catalog_contribution().expect("contribution");
        let mut names: Vec<_> = contribution
            .bindings()
            .iter()
            .map(|binding| binding.operation().as_str().to_owned())
            .collect();
        names.sort();
        names.dedup();
        let mut expected = FEEDBACK_SPECS
            .iter()
            .filter(|spec| !spec.surfaces.is_empty())
            .map(|spec| spec.operation.to_owned())
            .collect::<Vec<_>>();
        expected.sort();
        assert_eq!(names, expected);
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
            assert!(capability.availability().is_callable());
            assert_eq!(capability.binding_ids().len(), spec.surfaces.len());
        }
    }
}
