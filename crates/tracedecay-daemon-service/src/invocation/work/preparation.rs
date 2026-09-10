//! Backend-owned preparation of exact Work mutation commands.

use std::collections::BTreeSet;

use tracedecay_contracts::{
    ApplicationProblem, RequestContext, RequestId, RetryDirective, SafeDiagnostic,
};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::{RegisteredWorkRuntime, work_product_problem, work_projection_problem};

pub(super) fn prepare_graph_mutation(
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    capability: &str,
    use_case: &UseCaseId,
    request: tracedecay_contracts::PrepareWorkProductMutationRequestV1,
    canonical_request_id: &RequestId,
    observed_at: UtcMicros,
) -> Result<tracedecay_contracts::WorkProductMutationRequestV1, ApplicationProblem> {
    let capability =
        CapabilityId::new(capability).map_err(|_| work_product_authority_unavailable())?;
    let binding = tracedecay_contracts::WorkProductBindingV1::new(capability, use_case.clone());
    let product_services = tracedecay_application::work::RegisteredWorkProductServicesV1::attach(
        &registered.database,
        binding.clone(),
    )
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
    services: &tracedecay_application::work::RegisteredWorkApplicationServicesV1,
    context: &RequestContext,
    capability: &str,
    use_case: &UseCaseId,
    request: tracedecay_contracts::PrepareWorkDuplicateAdjudicationRequestV1,
    canonical_request_id: &RequestId,
    observed_at: UtcMicros,
) -> Result<tracedecay_domain::WorkDuplicateAdjudicationCommandV1, ApplicationProblem> {
    require_attempt(services, context, &request.first_attempt)?;
    require_attempt(services, context, &request.second_attempt)?;
    let attempt_snapshot = services
        .projections()
        .snapshot(context, tracedecay_contracts::MAX_WORK_PROJECTION_PAGE_SIZE)
        .map_err(work_projection_problem)?;
    let binding = work_product_binding(capability, use_case)?;
    let product_snapshot = current_product_snapshot(registered, context, binding, observed_at)?
        .ok_or_else(work_product_authority_unavailable)?;
    let topology_generation = product_snapshot
        .topology_generation_ref()
        .map_err(work_product_problem)?;
    let command_id =
        tracedecay_domain::WorkCommandId::new(canonical_request_id.as_str().to_owned())
            .map_err(|_| work_product_authority_unavailable())?;
    services.duplicate_adjudications().prepare_adjudication(
        context,
        request,
        tracedecay_domain::WorkDuplicateAdjudicationEvidenceV1 {
            work_generation: attempt_snapshot.generation_id().clone(),
            topology_generation,
        },
        command_id,
        observed_at,
    )
}

pub(super) fn current_product_topology(
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    capability: &str,
    use_case: &UseCaseId,
    observed_at: UtcMicros,
) -> Result<tracedecay_contracts::WorkAttemptTopologyStateV1, ApplicationProblem> {
    let binding = work_product_binding(capability, use_case)?;
    let Some(snapshot) = current_product_snapshot(registered, context, binding, observed_at)?
    else {
        return Ok(tracedecay_contracts::WorkAttemptTopologyStateV1::Absent);
    };
    let task_count = u32::try_from(snapshot.graph().items().len())
        .map_err(|_| work_product_authority_unavailable())?;
    let generation = snapshot
        .topology_generation_ref()
        .map_err(work_product_problem)?;
    Ok(tracedecay_contracts::WorkAttemptTopologyStateV1::Verified(
        tracedecay_contracts::WorkAttemptTopologyBindingV1 {
            generation: generation.as_str().to_owned(),
            task_count,
        },
    ))
}

pub(super) fn current_product_snapshot(
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    binding: tracedecay_contracts::WorkProductBindingV1,
    observed_at: UtcMicros,
) -> Result<Option<tracedecay_contracts::WorkGraphVersionEntryV1>, ApplicationProblem> {
    let product = tracedecay_application::work::RegisteredWorkProductServicesV1::attach(
        &registered.database,
        binding,
    )
    .map_err(|_| work_product_authority_unavailable())?;
    let selection = tracedecay_contracts::WorkProductSelectionScopeV1::relations(BTreeSet::from([
        tracedecay_domain::WorkProductAuthorizedRelationScopeV1::Repository {
            project_id: context.scope().project_id.clone(),
            repository_id: context.scope().repository_id.clone(),
        },
    ]))
    .map_err(|_| work_product_authority_unavailable())?;
    let graph = match product.reads().read_graph(
        context,
        tracedecay_contracts::WorkGraphReadRequestV1::current(selection, observed_at),
    ) {
        Ok(graph) => graph,
        // `read_graph` has already admitted the binding and authorized this
        // exact relation selection. Its port uses this concealed result only
        // for the documented no-published-version state; owner denial and
        // authority failure take the distinct error arms below.
        Err(tracedecay_contracts::WorkProductApplicationErrorV1::NotFoundOrNotAuthorized) => {
            return Ok(None);
        }
        Err(error) => return Err(work_product_problem(error)),
    };
    let tracedecay_contracts::WorkGraphReadV1::Current { snapshot, .. } = graph else {
        return Err(work_product_authority_unavailable());
    };
    Ok(Some(snapshot))
}

fn work_product_binding(
    capability: &str,
    use_case: &UseCaseId,
) -> Result<tracedecay_contracts::WorkProductBindingV1, ApplicationProblem> {
    let capability =
        CapabilityId::new(capability).map_err(|_| work_product_authority_unavailable())?;
    Ok(tracedecay_contracts::WorkProductBindingV1::new(
        capability,
        use_case.clone(),
    ))
}

fn require_attempt(
    services: &tracedecay_application::work::RegisteredWorkApplicationServicesV1,
    context: &RequestContext,
    identity: &tracedecay_domain::WorkAttemptIdentityV1,
) -> Result<(), ApplicationProblem> {
    services
        .attempts()
        .status(
            context,
            &tracedecay_contracts::WorkAttemptStatusRequestV1 {
                task_id: identity.task_id().clone(),
                run_id: identity.run_id().clone(),
                attempt_id: identity.attempt_id().clone(),
            },
        )
        .map(|_| ())
}

pub(super) fn current_work_product_revision_pins(
    registered: &RegisteredWorkRuntime,
) -> Result<tracedecay_contracts::WorkProductRevisionPinsV1, ApplicationProblem> {
    let policy_revision_id =
        tracedecay_domain::PolicyRevisionId::new(registered.policy_digest.as_str().to_owned())
            .map_err(|_| work_product_authority_unavailable())?;
    let catalog_digest = tracedecay_contracts::work_executable_catalog_digest()
        .map_err(|_| work_product_authority_unavailable())?;
    let catalog_generation_id =
        tracedecay_domain::CatalogGenerationId::new(catalog_digest.as_str().to_owned())
            .map_err(|_| work_product_authority_unavailable())?;
    Ok(tracedecay_contracts::WorkProductRevisionPinsV1 {
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
    request: tracedecay_contracts::DecideWorkProposalRequestV1,
    accepting: bool,
) -> Result<tracedecay_contracts::WorkProductMutationReceiptV1, ApplicationProblem> {
    if (request.disposition == tracedecay_domain::WorkProposalDispositionV1::Accepted) != accepting
    {
        return Err(invalid_work_product_request());
    }
    let capability =
        CapabilityId::new(capability).map_err(|_| work_product_authority_unavailable())?;
    let binding = tracedecay_contracts::WorkProductBindingV1::new(capability, use_case.clone());
    let services = tracedecay_application::work::RegisteredWorkProductServicesV1::attach(
        &registered.database,
        binding.clone(),
    )
    .map_err(|_| work_product_authority_unavailable())?;
    if request.mutation.revisions != current_work_product_revision_pins(registered)? {
        return Err(work_product_problem(
            tracedecay_contracts::WorkProductApplicationErrorV1::RevisionConflict,
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
        legal_actions: vec![tracedecay_contracts::LegalAction::CorrectRequest],
    }
}

fn work_product_authority_unavailable() -> ApplicationProblem {
    ApplicationProblem::unavailable(SafeDiagnostic {
        code: "work.graph_authority_unavailable".to_owned(),
        message: "The Work graph authority is unavailable".to_owned(),
    })
}
