//! Backend-owned preparation of exact Work mutation commands.

use std::sync::Arc;

use tracedecay_application::{
    ApplicationProblem, RequestContext, RequestId, RetryDirective, SafeDiagnostic,
};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::{
    RegisteredWorkRuntime, work_product_problem, work_projection_problem, work_topology_problem,
};

pub(super) fn prepare_graph_mutation(
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    capability: &str,
    use_case: &UseCaseId,
    request: tracedecay_application::PrepareWorkProductMutationRequestV1,
    canonical_request_id: &RequestId,
    observed_at: UtcMicros,
) -> Result<tracedecay_application::WorkProductMutationRequestV1, ApplicationProblem> {
    let capability =
        CapabilityId::new(capability).map_err(|_| work_product_authority_unavailable())?;
    let binding = tracedecay_application::WorkProductBindingV1::new(capability, use_case.clone());
    let product_services = registered
        .database
        .work_product_services(binding.clone())
        .map_err(|_| work_product_authority_unavailable())?;
    let revisions = current_work_product_revision_pins(registered)?;
    let command_id =
        tracedecay_domain::WorkCommandId::new(canonical_request_id.as_str().to_owned())
            .map_err(|_| work_product_authority_unavailable())?;
    product_services
        .mutations()
        .prepare_mutation(
            context,
            &binding,
            request,
            command_id,
            observed_at,
            revisions,
        )
        .map_err(work_product_problem)
}

pub(super) fn prepare_duplicate_adjudication(
    registered: &RegisteredWorkRuntime,
    services: &crate::global_db::RegisteredWorkApplicationServicesV1,
    context: &RequestContext,
    request: tracedecay_application::PrepareWorkDuplicateAdjudicationRequestV1,
    canonical_request_id: &RequestId,
    observed_at: UtcMicros,
) -> Result<tracedecay_domain::WorkDuplicateAdjudicationCommandV1, ApplicationProblem> {
    let authority = tracedecay_domain::WorkAuthority::new(
        context.scope().project_id.clone(),
        context.scope().repository_id.clone(),
        context.scope().worktree_id.clone(),
        context.actor().clone(),
        context.grant().digest.clone(),
    )
    .map_err(|_| invalid_work_product_request())?;
    require_attempt(services, context, &request.first_attempt)?;
    require_attempt(services, context, &request.second_attempt)?;
    let snapshot = services
        .projections()
        .snapshot(
            context,
            tracedecay_application::MAX_WORK_PROJECTION_PAGE_SIZE,
        )
        .map_err(work_projection_problem)?;
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let topology = services
        .topology()
        .verified_snapshot(&authority, cancelled)
        .map_err(|error| match work_topology_problem(error) {
            Ok(_) => work_product_authority_unavailable(),
            Err(problem) => problem,
        })?;
    let topology_generation = topology
        .evidence_ref()
        .map_err(|_| work_product_authority_unavailable())?;
    let command_id =
        tracedecay_domain::WorkCommandId::new(canonical_request_id.as_str().to_owned())
            .map_err(|_| work_product_authority_unavailable())?;
    services.duplicate_adjudications().prepare_adjudication(
        context,
        request,
        snapshot.generation_id().clone(),
        topology_generation,
        command_id,
        observed_at,
    )
}

fn require_attempt(
    services: &crate::global_db::RegisteredWorkApplicationServicesV1,
    context: &RequestContext,
    identity: &tracedecay_domain::WorkAttemptIdentityV1,
) -> Result<(), ApplicationProblem> {
    services
        .attempts()
        .status(
            context,
            &tracedecay_application::WorkAttemptStatusRequestV1 {
                task_id: identity.task_id().clone(),
                run_id: identity.run_id().clone(),
                attempt_id: identity.attempt_id().clone(),
            },
        )
        .map(|_| ())
}

pub(super) fn current_work_product_revision_pins(
    registered: &RegisteredWorkRuntime,
) -> Result<tracedecay_application::WorkProductRevisionPinsV1, ApplicationProblem> {
    let policy_revision_id =
        tracedecay_domain::PolicyRevisionId::new(registered.policy_digest.as_str().to_owned())
            .map_err(|_| work_product_authority_unavailable())?;
    let catalog_digest = tracedecay_application::work_executable_catalog_digest()
        .map_err(|_| work_product_authority_unavailable())?;
    let catalog_generation_id =
        tracedecay_domain::CatalogGenerationId::new(catalog_digest.as_str().to_owned())
            .map_err(|_| work_product_authority_unavailable())?;
    Ok(tracedecay_application::WorkProductRevisionPinsV1 {
        policy_revision_id,
        configuration_revision_id: registered.proposal_routing.configuration_revision().clone(),
        catalog_generation_id,
    })
}

pub(super) fn decide_product_proposal(
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    capability: &str,
    use_case: &UseCaseId,
    request: tracedecay_application::DecideWorkProposalRequestV1,
    accepting: bool,
) -> Result<tracedecay_application::WorkProductMutationReceiptV1, ApplicationProblem> {
    if (request.disposition == tracedecay_domain::WorkProposalDispositionV1::Accepted) != accepting
    {
        return Err(invalid_work_product_request());
    }
    let capability =
        CapabilityId::new(capability).map_err(|_| work_product_authority_unavailable())?;
    let binding = tracedecay_application::WorkProductBindingV1::new(capability, use_case.clone());
    let services = registered
        .database
        .work_product_services(binding.clone())
        .map_err(|_| work_product_authority_unavailable())?;
    if request.mutation.revisions != current_work_product_revision_pins(registered)? {
        return Err(work_product_problem(
            tracedecay_application::WorkProductApplicationErrorV1::RevisionConflict,
        ));
    }
    services
        .mutations()
        .decide_proposal(context, &binding, request)
        .map_err(work_product_problem)
}

fn invalid_work_product_request() -> ApplicationProblem {
    ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic {
            code: "work.invalid_graph_operation".to_owned(),
            message: "The Work graph request is invalid".to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: vec![tracedecay_application::LegalAction::CorrectRequest],
    }
}

fn work_product_authority_unavailable() -> ApplicationProblem {
    ApplicationProblem::unavailable(SafeDiagnostic {
        code: "work.graph_authority_unavailable".to_owned(),
        message: "The Work graph authority is unavailable".to_owned(),
    })
}
