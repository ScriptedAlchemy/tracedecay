//! Work intelligence read-handler composition.
//!
//! These handlers bind their read operation to the admitted capability and
//! use-case context before reading the registered Work authorities. Experience
//! additionally snapshots current configuration consent, so expertise never
//! relies on caller-provided or stale authorization state.

use std::sync::Arc;

use tracedecay_application::{
    ApplicationProblem, Deadline, RequestContext, RequestId, SafeDiagnostic,
    WorkAttemptListRequestV1, WorkAttemptTopologyBindingV1, WorkAttemptTopologyStateV1,
    WorkExperienceRequestV1, WorkExpertiseConsentSnapshotV1, WorkProductBindingV1,
    WorkProposalComparisonRequestV1,
};
use tracedecay_domain::{ManifestDigest, UtcMicros};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use crate::daemon_contract::{
    DaemonInvocationProblem, DaemonInvocationResponse, WorkApplicationOutcomeV1,
};
use crate::global_db::configuration::{
    OwnedGlobalDbConfigurationControlStore, contracts::ConfigurationControlStore as _,
};

use super::{
    RegisteredWorkRuntime, complete_work_read, work_product_problem, work_topology_problem,
    work_topology_unavailable_problem,
};

pub(super) fn generate_proposal(
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    capability: &str,
    use_case: &UseCaseId,
    request: tracedecay_application::GenerateProposalRequest,
) -> Result<tracedecay_application::GeneratedWorkProposal, ApplicationProblem> {
    let capability = CapabilityId::new(capability).map_err(|_| {
        work_product_problem(
            tracedecay_application::WorkProductApplicationErrorV1::GraphAuthorityUnavailable,
        )
    })?;
    let binding = WorkProductBindingV1::new(capability, use_case.clone());
    registered
        .database
        .work_intelligence_service(binding)
        .map_err(|_| {
            tracedecay_application::WorkProductApplicationErrorV1::GraphAuthorityUnavailable
        })?
        .generate_proposal(
            context,
            registered.configuration_digest.clone(),
            &registered.proposal_routing,
            request,
        )
        .map_err(work_product_problem)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn execution_history(
    registered: &RegisteredWorkRuntime,
    services: &crate::global_db::RegisteredWorkApplicationServicesV1,
    request_id: String,
    context: &RequestContext,
    canonical_request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    observed_at: UtcMicros,
    deadline: Deadline,
    request: WorkAttemptListRequestV1,
) -> DaemonInvocationResponse {
    let attempts = services.attempts().list(context, &request, |authority| {
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        match services.topology().verified_snapshot(authority, cancelled) {
            Ok(topology) => {
                let task_count = u32::try_from(topology.task_count()).map_err(|_| {
                    work_topology_unavailable_problem("the verified topology task count overflowed")
                })?;
                Ok(WorkAttemptTopologyStateV1::Verified(
                    WorkAttemptTopologyBindingV1 {
                        generation: topology.generation().as_str().to_owned(),
                        task_count,
                    },
                ))
            }
            Err(error) => work_topology_problem(error),
        }
    });
    let history = attempts.and_then(|attempts| {
        let storage = registered.database.work_storage().map_err(|_| {
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "application.work-execution-history.unavailable".to_owned(),
                message: "The Work execution timing authority is unavailable.".to_owned(),
            })
        })?;
        tracedecay_application::project_work_execution_history(&storage, context, attempts)
    });
    complete_work_read(
        registered,
        request_id,
        context,
        canonical_request_id,
        operation_key,
        use_case,
        input_digest,
        history,
        observed_at,
        deadline,
        WorkApplicationOutcomeV1::ExecutionHistory,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn experience(
    registered: &RegisteredWorkRuntime,
    request_id: String,
    context: &RequestContext,
    canonical_request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    observed_at: UtcMicros,
    deadline: Deadline,
    capability: &str,
    request: WorkExperienceRequestV1,
) -> DaemonInvocationResponse {
    let Ok(capability) = CapabilityId::new(capability) else {
        return unavailable(request_id);
    };
    let binding = WorkProductBindingV1::new(capability, use_case.clone());
    let intelligence = match registered.database.work_intelligence_service(binding) {
        Ok(service) => service,
        Err(_) => return unavailable(request_id),
    };
    let configuration = OwnedGlobalDbConfigurationControlStore::from_registered_project_runtime_db(
        Arc::clone(&registered.database),
    );
    let current = match configuration.current().await {
        Ok(current) => current,
        Err(_) => return unavailable(request_id),
    };
    let consent = match WorkExpertiseConsentSnapshotV1::from_configuration(
        current.revision_id,
        current.snapshot,
    ) {
        Ok(consent) => consent,
        Err(_) => return unavailable(request_id),
    };
    complete_work_read(
        registered,
        request_id,
        context,
        canonical_request_id,
        operation_key,
        use_case,
        input_digest,
        intelligence
            .experience(context, request, consent)
            .map_err(work_product_problem),
        observed_at,
        deadline,
        WorkApplicationOutcomeV1::Experience,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn compare_proposal(
    registered: &RegisteredWorkRuntime,
    request_id: String,
    context: &RequestContext,
    canonical_request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    observed_at: UtcMicros,
    deadline: Deadline,
    capability: &str,
    request: WorkProposalComparisonRequestV1,
) -> DaemonInvocationResponse {
    let Ok(capability) = CapabilityId::new(capability) else {
        return unavailable(request_id);
    };
    let binding = WorkProductBindingV1::new(capability, use_case.clone());
    let intelligence = match registered.database.work_intelligence_service(binding) {
        Ok(service) => service,
        Err(_) => return unavailable(request_id),
    };
    complete_work_read(
        registered,
        request_id,
        context,
        canonical_request_id,
        operation_key,
        use_case,
        input_digest,
        intelligence
            .compare_proposal(context, request)
            .map_err(work_product_problem),
        observed_at,
        deadline,
        WorkApplicationOutcomeV1::CompareProposal,
    )
}

fn unavailable(request_id: String) -> DaemonInvocationResponse {
    DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::Unavailable)
}
