//! Work intelligence read-handler composition.
//!
//! These handlers bind their read operation to the admitted capability and
//! use-case context before reading the registered Work authorities. Experience
//! also snapshots current configuration consent, so expertise never
//! relies on caller-provided or stale authorization state.

use tracedecay_application::work::RegisteredWorkApplicationServicesV1;
use tracedecay_contracts::{
    ApplicationProblem, Deadline, RequestContext, RequestId, SafeDiagnostic,
    WorkAttemptListOperationV1, WorkAttemptListRequestV1, WorkExperienceRequestV1,
    WorkExpertiseConsentSnapshotV1, WorkProductBindingV1, WorkProposalComparisonRequestV1,
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

fn intelligence_service(
    registered: &RegisteredWorkRuntime,
    capability: &str,
    use_case: &UseCaseId,
) -> Result<
    tracedecay_contracts::WorkIntelligenceServiceV1<
        tracedecay_rusqlite_runtime::work_product::AuthorizedWorkProductReadStorageV1,
        tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    >,
    ApplicationProblem,
> {
    let unavailable = || {
        work_product_problem(
            tracedecay_contracts::WorkProductApplicationErrorV1::GraphAuthorityUnavailable,
        )
    };
    let capability = CapabilityId::new(capability).map_err(|_| unavailable())?;
    tracedecay_application::work::authorized_work_intelligence_service(
        &registered.database,
        WorkProductBindingV1::new(capability, use_case.clone()),
        registered.authority.clone(),
    )
    .map_err(|_| unavailable())
}

#[tracing::instrument(
    name = "daemon.service.work.generate_proposal",
    level = "trace",
    skip_all
)]
pub(super) async fn generate_proposal(
    registered: &RegisteredWorkRuntime,
    services: &RegisteredWorkApplicationServicesV1,
    context: &RequestContext,
    capability: &str,
    use_case: &UseCaseId,
    request: tracedecay_contracts::GenerateProposalRequest,
) -> Result<tracedecay_contracts::GeneratedWorkProposal, ApplicationProblem> {
    let (registered, context, capability, use_case) = (
        registered.clone(),
        context.clone(),
        capability.to_owned(),
        use_case.clone(),
    );
    services
        .read(move |_| {
            intelligence_service(&registered, &capability, &use_case)?
                .generate_proposal(
                    &context,
                    registered.configuration_digest.clone(),
                    &registered.proposal_routing,
                    request,
                )
                .map_err(work_product_problem)
        })
        .await
}

#[allow(clippy::too_many_arguments)]
#[tracing::instrument(
    name = "daemon.service.work.execution_history",
    level = "trace",
    skip_all
)]
pub(super) async fn execution_history(
    registered: &RegisteredWorkRuntime,
    services: &RegisteredWorkApplicationServicesV1,
    request_id: String,
    context: &RequestContext,
    canonical_request_id: RequestId,
    operation_key: &str,
    capability: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    observed_at: UtcMicros,
    deadline: Deadline,
    request: WorkAttemptListRequestV1,
) -> DaemonInvocationResponse {
    let history = {
        let (registered, context, capability, use_case) = (
            registered.clone(),
            context.clone(),
            capability.to_owned(),
            use_case.clone(),
        );
        services
            .read(move |services| {
                let attempts = services.attempts().list(
                    &context,
                    WorkAttemptListOperationV1::ExecutionHistory,
                    &request,
                    || {
                        preparation::current_work_product_attempt_topology(
                            &registered,
                            &context,
                            &capability,
                            &use_case,
                            observed_at,
                        )
                    },
                )?;
                let storage = registered.database.work_storage().map_err(|_| {
                    ApplicationProblem::unavailable(SafeDiagnostic {
                        code: "application.work-execution-history.unavailable".to_owned(),
                        message: "The Work execution timing authority is unavailable.".to_owned(),
                    })
                })?;
                tracedecay_contracts::project_work_execution_history(&storage, &context, attempts)
            })
            .await
    };
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
#[tracing::instrument(name = "daemon.service.work.experience", level = "trace", skip_all)]
pub(super) async fn experience(
    registered: &RegisteredWorkRuntime,
    services: &RegisteredWorkApplicationServicesV1,
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
    let Ok(intelligence) = intelligence_service(registered, capability, &use_case) else {
        return unavailable(request_id);
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
    let read_context = context.clone();
    let experienced = services
        .read(move |_| {
            intelligence
                .experience(&read_context, request, consent)
                .map_err(work_product_problem)
        })
        .await;
    complete_work_read(
        registered,
        request_id,
        context,
        canonical_request_id,
        operation_key,
        use_case,
        input_digest,
        experienced,
        observed_at,
        deadline,
        WorkApplicationOutcomeV1::Experience,
    )
}

#[allow(clippy::too_many_arguments)]
#[tracing::instrument(
    name = "daemon.service.work.compare_proposal",
    level = "trace",
    skip_all
)]
pub(super) async fn compare_proposal(
    registered: &RegisteredWorkRuntime,
    services: &RegisteredWorkApplicationServicesV1,
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
    let Ok(intelligence) = intelligence_service(registered, capability, &use_case) else {
        return unavailable(request_id);
    };
    let read_context = context.clone();
    let compared = services
        .read(move |_| {
            intelligence
                .compare_proposal(&read_context, request)
                .map_err(work_product_problem)
        })
        .await;
    complete_work_read(
        registered,
        request_id,
        context,
        canonical_request_id,
        operation_key,
        use_case,
        input_digest,
        compared,
        observed_at,
        deadline,
        WorkApplicationOutcomeV1::CompareProposal,
    )
}

fn unavailable(request_id: String) -> DaemonInvocationResponse {
    DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::Unavailable)
}
