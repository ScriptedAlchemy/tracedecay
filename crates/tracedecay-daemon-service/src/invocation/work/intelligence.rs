//! Work intelligence read-handler composition.
//!
//! These handlers bind their read operation to the admitted capability and
//! use-case context before reading the registered Work authorities. Experience
//! additionally snapshots current configuration consent, so expertise never
//! relies on caller-provided or stale authorization state.

use tracedecay_contracts::{
    ApplicationProblem, Deadline, RequestContext, RequestId, SafeDiagnostic,
    WorkAttemptListRequestV1, WorkExperienceRequestV1, WorkExpertiseConsentSnapshotV1,
    WorkProductBindingV1, WorkProposalComparisonRequestV1,
};
use tracedecay_domain::{ManifestDigest, UtcMicros};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use tracedecay_daemon_protocol::{
    DaemonInvocationProblem, DaemonInvocationResponse, WorkApplicationOutcomeV1,
};
use tracedecay_global_db::configuration::{
    OwnedGlobalDbConfigurationControlStore, contracts::ConfigurationControlStore as _,
};

use super::{RegisteredWorkRuntime, complete_work_read, preparation, work_product_problem};

#[hotpath::measure(label = "daemon.service.work.generate_proposal")]
pub(super) fn generate_proposal(
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    capability: &str,
    use_case: &UseCaseId,
    request: tracedecay_contracts::GenerateProposalRequest,
) -> Result<tracedecay_contracts::GeneratedWorkProposal, ApplicationProblem> {
    let capability = CapabilityId::new(capability).map_err(|_| {
        work_product_problem(
            tracedecay_contracts::WorkProductApplicationErrorV1::GraphAuthorityUnavailable,
        )
    })?;
    let binding = WorkProductBindingV1::new(capability, use_case.clone());
    tracedecay_application::work::work_intelligence_service(&registered.database, binding)
        .map_err(|_| {
            work_product_problem(
                tracedecay_contracts::WorkProductApplicationErrorV1::GraphAuthorityUnavailable,
            )
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
#[hotpath::measure(label = "daemon.service.work.execution_history")]
pub(super) fn execution_history(
    registered: &RegisteredWorkRuntime,
    services: &tracedecay_application::work::RegisteredWorkApplicationServicesV1,
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
    // Execution history projects the same attempt page `list_attempts` reads,
    // under the same topology generation the cursor names, so both operations
    // must bind the executor topology or a cursor minted by one would be
    // judged against the other's generation.
    let attempts = services.attempts().list(context, &request, |authority| {
        preparation::current_executor_attempt_topology(services, authority)
    });
    let history = attempts.and_then(|attempts| {
        let storage = registered.database.work_storage().map_err(|_| {
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "application.work-execution-history.unavailable".to_owned(),
                message: "The Work execution timing authority is unavailable.".to_owned(),
            })
        })?;
        tracedecay_contracts::project_work_execution_history(&storage, context, attempts)
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
#[hotpath::measure(label = "daemon.service.work.experience", future = true)]
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
    let intelligence = match tracedecay_application::work::work_intelligence_service(
        &registered.database,
        binding,
    ) {
        Ok(service) => service,
        Err(_) => return unavailable(request_id),
    };
    let configuration = OwnedGlobalDbConfigurationControlStore::from_registered_project_runtime_db(
        registered.database.clone(),
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
#[hotpath::measure(label = "daemon.service.work.compare_proposal")]
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
    let intelligence = match tracedecay_application::work::work_intelligence_service(
        &registered.database,
        binding,
    ) {
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
