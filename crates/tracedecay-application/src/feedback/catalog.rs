//! Public feedback read bindings for Plan 21 / Plan 37 surfaces.
//!
//! These bindings project the PR11 feedback-cycle result. They never create a
//! second finding store and never execute follow-up work.

use tracedecay_tool_catalog::{
    AuthorityRequirement, AvailabilityContract, BindingId, BindingStatus, BindingSurface,
    CancellationContract, CancellationPoint, CapabilityId, CapabilityManifestInputV1,
    CapabilityManifestV1, CatalogContributionInputV1, CatalogContributionV1, ContributionId,
    DeadlineBehavior, DeadlineContract, DeniedDisclosurePolicy, EffectClass, IdempotencyContract,
    LifecycleClass, PaginationContract, PrivacyClass, ProtocolRevisionRange, ReceiptContract,
    ReconciliationContract, RevalidationContract, RevalidationPoint, RoutingContractV1, SchemaId,
    SchemaRef, ScopeDimension, ScopeRequirement, StreamingContract, SurfaceBindingInputV1,
    SurfaceBindingV1, SurfaceOperationName, TerminalState, TerminalStateContract,
    UnavailabilityReason, UseCaseId,
};

use crate::error::ApplicationContractError;
use crate::handlers::{ApplicationHandlerDescriptor, ApplicationOperation};
use crate::result::ResultContractRef;

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
}

const FEEDBACK_SPECS: [FeedbackSurfaceSpec; 7] = [
    FeedbackSurfaceSpec {
        capability: "capability.application.feedback.diagnostics",
        use_case: "use-case.application.feedback.diagnostics",
        request_schema: "schema.application.feedback.diagnostics.request",
        result_schema: "schema.application.feedback.diagnostics.result",
        operation: "feedback_diagnostics",
        summary: "Run feedback diagnostics",
        description: "Invoke the branch-aware feedback cycle and return the typed result.",
        example: "Diagnose the current branch feedback cycle",
        paginated: false,
    },
    FeedbackSurfaceSpec {
        capability: "capability.application.feedback.get",
        use_case: "use-case.application.feedback.get",
        request_schema: "schema.application.feedback.get.request",
        result_schema: "schema.application.feedback.get.result",
        operation: "feedback_get",
        summary: "Get a feedback finding",
        description: "Fetch one authorized feedback finding by durable identity.",
        example: "Get this feedback finding",
        paginated: false,
    },
    FeedbackSurfaceSpec {
        capability: "capability.application.feedback.expand",
        use_case: "use-case.application.feedback.expand",
        request_schema: "schema.application.feedback.expand.request",
        result_schema: "schema.application.feedback.expand.result",
        operation: "feedback_expand",
        summary: "Expand feedback evidence",
        description: "Expand authorized anchors and evidence for one feedback finding.",
        example: "Expand this feedback finding",
        paginated: false,
    },
    FeedbackSurfaceSpec {
        capability: "capability.application.feedback.list",
        use_case: "use-case.application.feedback.list",
        request_schema: "schema.application.feedback.list.request",
        result_schema: "schema.application.feedback.list.result",
        operation: "feedback_list",
        summary: "List feedback findings",
        description: "List authorized feedback findings with Plan 05 cursors.",
        example: "List feedback findings for this branch",
        paginated: true,
    },
    FeedbackSurfaceSpec {
        capability: "capability.application.feedback.github-review-ingest",
        use_case: "use-case.application.feedback.github-review-ingest",
        request_schema: "schema.application.feedback.github-review-ingest.request",
        result_schema: "schema.application.feedback.github-review-ingest.result",
        operation: "github_review_ingest",
        summary: "Ingest existing GitHub review evidence",
        description: "Read allowlisted existing GitHub review comments and threads without a write path.",
        example: "Read existing review threads for this pull request",
        paginated: true,
    },
    FeedbackSurfaceSpec {
        capability: "capability.application.feedback.ci-failure-localize",
        use_case: "use-case.application.feedback.ci-failure-localize",
        request_schema: "schema.application.feedback.ci-failure-localize.request",
        result_schema: "schema.application.feedback.ci-failure-localize.result",
        operation: "ci_failure_localize",
        summary: "Localize a reported CI failure",
        description: "Map anchored CI evidence to exact branch, generation, symbol, caller, and test evidence without running CI.",
        example: "Localize this reported CI failure",
        paginated: false,
    },
    FeedbackSurfaceSpec {
        capability: "capability.application.feedback.proximity",
        use_case: "use-case.application.feedback.proximity",
        request_schema: "schema.application.feedback.proximity.request",
        result_schema: "schema.application.feedback.proximity.result",
        operation: "feedback_proximity",
        summary: "Inspect advisory concurrent-work proximity",
        description: "Return immediate or configured-threshold proximity evidence without locks, scheduling, or continuation.",
        example: "Inspect concurrent-work proximity for this branch",
        paginated: false,
    },
];

const SURFACES: [BindingSurface; 3] = [
    BindingSurface::Cli,
    BindingSurface::Mcp,
    BindingSurface::Http,
];

pub fn feedback_surface_catalog_contribution()
-> Result<CatalogContributionV1, ApplicationContractError> {
    let mut capabilities = Vec::with_capacity(FEEDBACK_SPECS.len());
    let mut bindings = Vec::with_capacity(FEEDBACK_SPECS.len() * SURFACES.len());

    for spec in &FEEDBACK_SPECS {
        let capability_id = CapabilityId::new(spec.capability)?;
        let mut binding_ids = Vec::with_capacity(SURFACES.len());
        for surface in SURFACES {
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
        capabilities.push(capability(spec, capability_id, binding_ids)?);
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
    FEEDBACK_SPECS.iter().map(handler_descriptor).collect()
}

fn capability(
    spec: &FeedbackSurfaceSpec,
    capability_id: CapabilityId,
    binding_ids: Vec<BindingId>,
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
        availability: AvailabilityContract::Unavailable {
            reason: UnavailabilityReason::NotImplemented,
        },
        binding_ids,
        profile_eligibility: Vec::new(),
        required_features: Vec::new(),
    })?)
}

fn handler_descriptor(
    spec: &FeedbackSurfaceSpec,
) -> Result<ApplicationHandlerDescriptor, ApplicationContractError> {
    let result_schema = schema(spec.result_schema)?;
    ApplicationHandlerDescriptor::new(
        ApplicationOperation::new(
            CapabilityId::new(spec.capability)?,
            UseCaseId::new(spec.use_case)?,
            ResultContractRef::from_schema(&result_schema),
            true,
        ),
        schema(spec.request_schema)?,
        result_schema,
    )
}

fn schema(id: &str) -> Result<SchemaRef, ApplicationContractError> {
    Ok(SchemaRef::new(SchemaId::new(id)?, 1, 8_192)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feedback_surface_names_are_exact() {
        let contribution = feedback_surface_catalog_contribution().expect("contribution");
        let mut names: Vec<_> = contribution
            .bindings()
            .iter()
            .map(|binding| binding.operation().as_str().to_owned())
            .collect();
        names.sort();
        names.dedup();
        assert_eq!(
            names,
            vec![
                "ci_failure_localize".to_owned(),
                "feedback_diagnostics".to_owned(),
                "feedback_expand".to_owned(),
                "feedback_get".to_owned(),
                "feedback_list".to_owned(),
                "feedback_proximity".to_owned(),
                "github_review_ingest".to_owned(),
            ]
        );
    }
}
