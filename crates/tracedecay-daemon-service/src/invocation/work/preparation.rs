//! Backend-owned preparation of exact Work mutation commands.

use tracedecay_contracts::{
    ApplicationProblem, RequestContext, RequestId, RetryDirective, SafeDiagnostic,
};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::{RegisteredWorkRuntime, work_product_problem};

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
    let snapshot =
        current_work_product_snapshot(registered, context, capability, use_case, observed_at)?;
    let topology_generation = match current_work_product_attempt_topology(
        registered,
        context,
        capability,
        use_case,
        observed_at,
    )? {
        tracedecay_contracts::WorkAttemptTopologyStateV1::Verified(binding) => {
            tracedecay_domain::WorkTopologyGenerationRefV1::new(binding.generation)
                .map_err(|_| work_product_authority_unavailable())?
        }
        tracedecay_contracts::WorkAttemptTopologyStateV1::Absent => {
            return Err(work_product_authority_unavailable());
        }
    };
    let command_id =
        tracedecay_domain::WorkCommandId::new(canonical_request_id.as_str().to_owned())
            .map_err(|_| work_product_authority_unavailable())?;
    services.duplicate_adjudications().prepare_adjudication(
        context,
        request,
        tracedecay_domain::WorkDuplicateAdjudicationEvidenceV1 {
            work_generation: tracedecay_contracts::work_product_projection_generation(&snapshot)
                .map_err(work_product_problem)?,
            topology_generation,
        },
        command_id,
        observed_at,
    )
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

pub(super) fn prepare_execution_snapshot(
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    binding: &tracedecay_contracts::WorkProductBindingV1,
    request: &tracedecay_contracts::AdmitWorkExecutionRequestV1,
) -> Result<tracedecay_domain::WorkExecutionSnapshot, ApplicationProblem> {
    let services = tracedecay_application::work::RegisteredWorkProductServicesV1::attach(
        &registered.database,
        binding.clone(),
    )
    .map_err(|_| work_product_authority_unavailable())?;
    let read = services
        .reads()
        .read_graph(
            context,
            tracedecay_contracts::WorkGraphReadRequestV1::current(
                request.selection.clone(),
                request.mutation.occurred_at,
            ),
        )
        .map_err(work_product_problem)?;
    if read.selection_coverage().is_partial() {
        return Err(work_product_problem(
            tracedecay_contracts::WorkProductApplicationErrorV1::SelectionCoverageIncomplete,
        ));
    }
    let tracedecay_contracts::WorkGraphReadV1::Current { snapshot, .. } = read else {
        return Err(work_product_authority_unavailable());
    };
    let tracedecay_contracts::WorkProductExpectedAuthorityV1::Verified { verified_version } =
        &request.mutation.expected_authority
    else {
        return Err(invalid_work_product_request());
    };
    if snapshot.verified_version() != verified_version
        || snapshot.graph().version() != request.based_on_version
    {
        return Err(work_product_problem(
            tracedecay_contracts::WorkProductApplicationErrorV1::VersionConflict,
        ));
    }
    let item = snapshot.graph().item(&request.task_id).ok_or_else(|| {
        work_product_problem(
            tracedecay_contracts::WorkProductApplicationErrorV1::NotFoundOrNotAuthorized,
        )
    })?;
    let accepted_proposal = item.accepted_proposal().ok_or_else(|| {
        work_product_problem(tracedecay_contracts::WorkProductApplicationErrorV1::InvalidRequest)
    })?;
    let proposal = snapshot
        .graph()
        .proposal_decisions()
        .iter()
        .find(|decision| {
            decision.proposal().proposal_id() == accepted_proposal
                && decision.disposition() == &tracedecay_domain::WorkProposalDispositionV1::Accepted
        })
        .map(tracedecay_domain::WorkProposalDecisionV1::proposal)
        .ok_or_else(|| {
            work_product_problem(
                tracedecay_contracts::WorkProductApplicationErrorV1::GraphAuthorityUnavailable,
            )
        })?;
    registered
        .proposal_routing
        .execution_snapshot(
            proposal,
            &registered.work_topology_policy,
            request.mutation.occurred_at,
        )
        .map_err(|_| work_product_authority_unavailable())
}

pub(super) fn current_work_product_attempt_topology(
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    capability: &str,
    use_case: &UseCaseId,
    observed_at: UtcMicros,
) -> Result<tracedecay_contracts::WorkAttemptTopologyStateV1, ApplicationProblem> {
    let capability =
        CapabilityId::new(capability).map_err(|_| work_product_authority_unavailable())?;
    let binding = tracedecay_contracts::WorkProductBindingV1::new(capability, use_case.clone());
    tracedecay_application::work::RegisteredWorkProductServicesV1::attach(
        &registered.database,
        binding,
    )
    .map_err(|_| work_product_authority_unavailable())?
    .reads()
    .read_attempt_topology(context, observed_at)
    .map_err(work_product_problem)
}

pub(super) fn current_work_product_snapshot(
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    capability: &str,
    use_case: &UseCaseId,
    observed_at: UtcMicros,
) -> Result<tracedecay_contracts::WorkGraphVersionEntryV1, ApplicationProblem> {
    let capability =
        CapabilityId::new(capability).map_err(|_| work_product_authority_unavailable())?;
    let binding = tracedecay_contracts::WorkProductBindingV1::new(capability, use_case.clone());
    let selection = tracedecay_contracts::WorkProductSelectionScopeV1::relations(
        std::collections::BTreeSet::from([tracedecay_contracts::WorkRelationScopeV1::Repository {
            project_id: context.scope().project_id.clone(),
            repository_id: context.scope().repository_id.clone(),
        }]),
    )
    .map_err(|_| work_product_authority_unavailable())?;
    let read = tracedecay_application::work::RegisteredWorkProductServicesV1::attach(
        &registered.database,
        binding,
    )
    .map_err(|_| work_product_authority_unavailable())?
    .reads()
    .read_graph(
        context,
        tracedecay_contracts::WorkGraphReadRequestV1::current(selection, observed_at),
    )
    .map_err(work_product_problem)?;
    match read {
        tracedecay_contracts::WorkGraphReadV1::Current { snapshot, .. } => Ok(snapshot),
        tracedecay_contracts::WorkGraphReadV1::AsOf { .. }
        | tracedecay_contracts::WorkGraphReadV1::Evolution { .. }
        | tracedecay_contracts::WorkGraphReadV1::Forensic { .. } => {
            Err(work_product_authority_unavailable())
        }
    }
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
