//! Product-graph operations composed into the canonical Work invocation owner.

use serde::Serialize;
use tracedecay_application::{
    ApplicationOutcome, ApplicationProblem, CancellationContext, Deadline, LegalAction, RequestId,
    RetryDirective, SafeDiagnostic, WorkProductApplicationError,
};
use tracedecay_domain::{ManifestDigest, UtcMicros, canonical_sha256};
use tracedecay_tool_catalog::UseCaseId;

use crate::daemon::service::invocation::RegisteredWorkRuntime;
use crate::daemon_contract::{
    DaemonInvocationProblem, DaemonInvocationResponse, WorkApplicationInvocationV1,
    WorkApplicationOutcomeV1,
};

use super::{complete_work_effect, complete_work_read, work_request_context};

pub(super) fn execute(
    registered: RegisteredWorkRuntime,
    request_id: String,
    request: WorkApplicationInvocationV1,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let operation_key = request.operation_key();
    let Some((_, capability, use_case)) = tracedecay_application::WORK_APPLICATION_OPERATION_IDS_V1
        .iter()
        .find(|(operation, _, _)| *operation == operation_key)
    else {
        return DaemonInvocationResponse::problem(
            request_id,
            DaemonInvocationProblem::InvalidRequest,
        );
    };
    let (context, canonical_request_id, use_case) = match work_request_context(
        &registered,
        &request_id,
        capability,
        use_case,
        observed_at,
        deadline.clone(),
        cancellation,
    ) {
        Ok(context) => context,
        Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
    };
    let input_digest = match canonical_sha256(&request) {
        Ok(digest) => digest,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::InvalidRequest,
            );
        }
    };
    let service = registered.product_service();

    match request {
        WorkApplicationInvocationV1::ProductSnapshot(_) => complete_read(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            service.snapshot(&context),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::ProductSnapshot,
        ),
        WorkApplicationInvocationV1::ProductProjections(_) => complete_read(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            service.projections(&context),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::ProductProjections,
        ),
        WorkApplicationInvocationV1::TaskEvidence(request) => complete_read(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            service.task_evidence(&context, &request.task_id, request.limit),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::TaskEvidence,
        ),
        WorkApplicationInvocationV1::ExpandTaskEvidence(request) => complete_read(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            service.expand_evidence(&context, &request.task_id, &request.link_id),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::ExpandTaskEvidence,
        ),
        WorkApplicationInvocationV1::GenerateWorkProposal(request) => complete_read(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            service.generate_proposal(&context, request),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::GenerateWorkProposal,
        ),
        WorkApplicationInvocationV1::ApplyWorkCommand(request) => complete_effect(
            &registered,
            request_id,
            &context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            service.execute_mutation(&context, request),
            observed_at,
            deadline,
            WorkApplicationOutcomeV1::ApplyWorkCommand,
        ),
        _ => DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::InvalidRequest),
    }
}

#[allow(clippy::too_many_arguments)]
fn complete_read<T>(
    registered: &RegisteredWorkRuntime,
    request_id: String,
    context: &tracedecay_application::RequestContext,
    canonical_request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    result: Result<T, WorkProductApplicationError>,
    observed_at: UtcMicros,
    deadline: Deadline,
    wrap: fn(ApplicationOutcome<T>) -> WorkApplicationOutcomeV1,
) -> DaemonInvocationResponse
where
    T: Serialize,
{
    complete_work_read(
        registered,
        request_id,
        context,
        canonical_request_id,
        operation_key,
        use_case,
        input_digest,
        result.map_err(product_problem),
        observed_at,
        deadline,
        wrap,
    )
}

#[allow(clippy::too_many_arguments)]
fn complete_effect<T>(
    registered: &RegisteredWorkRuntime,
    request_id: String,
    context: &tracedecay_application::RequestContext,
    canonical_request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    result: Result<T, WorkProductApplicationError>,
    observed_at: UtcMicros,
    deadline: Deadline,
    wrap: fn(ApplicationOutcome<T>) -> WorkApplicationOutcomeV1,
) -> DaemonInvocationResponse
where
    T: Serialize,
{
    complete_work_effect(
        registered,
        request_id,
        context,
        canonical_request_id,
        operation_key,
        use_case,
        input_digest,
        result.map_err(product_problem),
        observed_at,
        deadline,
        wrap,
    )
}

fn product_problem(error: WorkProductApplicationError) -> ApplicationProblem {
    match error {
        WorkProductApplicationError::NotAuthorized
        | WorkProductApplicationError::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        WorkProductApplicationError::Cancelled => ApplicationProblem::cancelled_before_admission(),
        WorkProductApplicationError::TimedOut => ApplicationProblem::timed_out_before_admission(),
        WorkProductApplicationError::VersionConflict => ApplicationProblem::Conflict {
            diagnostic: SafeDiagnostic {
                code: "work.product.version_conflict".to_owned(),
                message: "The Work product graph changed after this command was prepared"
                    .to_owned(),
            },
            retry: RetryDirective::AfterRevalidate,
            legal_actions: vec![LegalAction::Refresh],
        },
        WorkProductApplicationError::IdempotencyConflict => ApplicationProblem::Conflict {
            diagnostic: SafeDiagnostic {
                code: "work.product.idempotency_conflict".to_owned(),
                message: "The Work command identity was reused with different input".to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: vec![LegalAction::CorrectRequest],
        },
        WorkProductApplicationError::InvalidRequest => ApplicationProblem::InvalidRequest {
            diagnostic: SafeDiagnostic {
                code: "work.product.invalid_request".to_owned(),
                message: "The Work product command is invalid".to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: vec![LegalAction::CorrectRequest],
        },
        WorkProductApplicationError::TopologyUnavailable
        | WorkProductApplicationError::EvidenceUnavailable => {
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "work.product.authority_unavailable".to_owned(),
                message: "The Work product authority is unavailable".to_owned(),
            })
        }
    }
}
