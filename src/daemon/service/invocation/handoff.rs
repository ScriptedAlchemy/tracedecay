//! Daemon-owned handoff-open execution and exact target-version rechecks.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tracedecay_application::feedback::FeedbackFindingReadV1;
use tracedecay_application::{
    HandoffAuthoritySnapshotV1, HandoffOpenBindingV1, HandoffOpenError, HandoffOpenService,
    HandoffOpenTargetError, HandoffOpenTargetPort, HandoffOpenTargetV1,
    investigation_owner_version_digest,
};

use super::administrative_effect::{administrative_authority, administrative_command_effect};
use super::*;

/// The token namespacing every handoff digest domain, policy id, idempotency
/// key, and effect id.
const HANDOFF_EFFECT_FAMILY: &str = "handoff";

#[derive(Clone)]
struct DaemonHandoffOpenTargets {
    feedback: Option<Arc<FeedbackRuntime>>,
    work: tracedecay_rusqlite_runtime::work::WorkSqliteStorage,
    observed_at: UtcMicros,
    recheck_sequence: Arc<AtomicU64>,
}

impl HandoffOpenTargetPort for DaemonHandoffOpenTargets {
    fn is_current<'a>(
        &'a self,
        context: &'a RequestContext,
        binding: &'a HandoffOpenBindingV1,
    ) -> Pin<Box<dyn Future<Output = Result<bool, HandoffOpenTargetError>> + Send + 'a>> {
        Box::pin(async move {
            match binding.target() {
                HandoffOpenTargetV1::Task {
                    task_id, version, ..
                } => {
                    let authority = WorkAuthority::new(
                        context.scope().project_id.clone(),
                        context.scope().repository_id.clone(),
                        context.scope().worktree_id.clone(),
                        context.actor().clone(),
                        context.grant().digest.clone(),
                    )
                    .map_err(|_| HandoffOpenTargetError::Unavailable)?;
                    match tracedecay_application::WorkStoragePort::projection(
                        &self.work, &authority, task_id,
                    ) {
                        Ok(projection) => Ok(projection.version() == *version),
                        Err(tracedecay_application::WorkStorageError::NotFoundOrNotAuthorized) => {
                            Ok(false)
                        }
                        Err(
                            tracedecay_application::WorkStorageError::VersionConflict
                            | tracedecay_application::WorkStorageError::IdempotencyConflict
                            | tracedecay_application::WorkStorageError::Unavailable,
                        ) => Err(HandoffOpenTargetError::Unavailable),
                    }
                }
                HandoffOpenTargetV1::Investigation {
                    finding_id,
                    owner_version_digest,
                } => {
                    let Some(feedback) = &self.feedback else {
                        return Err(HandoffOpenTargetError::Unavailable);
                    };
                    let request_id = internal_feedback_request_id(
                        context,
                        self.recheck_sequence.fetch_add(1, Ordering::Relaxed),
                    )?;
                    let handle = feedback
                        .mint_get(request_id, finding_id.clone(), self.observed_at)
                        .map_err(|_| HandoffOpenTargetError::Unavailable)?;
                    let invocation = feedback
                        .owner()
                        .invoke_with_controls(
                            FeedbackReadOperationV1::Get,
                            &handle,
                            self.observed_at,
                            context.deadline().clone(),
                            context.cancellation().clone(),
                        )
                        .await;
                    let finding = current_feedback_finding(invocation)?;
                    let Some(finding) = finding else {
                        return Ok(false);
                    };
                    let digest = investigation_owner_version_digest(&finding)
                        .map_err(|_| HandoffOpenTargetError::Unavailable)?;
                    Ok(
                        finding.finding.finding_id == *finding_id
                            && &digest == owner_version_digest,
                    )
                }
            }
        })
    }
}

fn internal_feedback_request_id(
    context: &RequestContext,
    recheck_sequence: u64,
) -> Result<String, HandoffOpenTargetError> {
    let digest = canonical_sha256(&(
        "tracedecay.daemon.handoff-open.feedback-recheck.v1",
        context.request_id(),
        recheck_sequence,
    ))
    .map_err(|_| HandoffOpenTargetError::Unavailable)?;
    Ok(format!(
        "request.handoff-feedback.{}",
        digest.as_str().trim_start_matches("sha256:")
    ))
}

fn current_feedback_finding(
    result: Result<FeedbackReadInvocationResultV1, FeedbackReadOwnerErrorV1>,
) -> Result<Option<FeedbackFindingReadV1>, HandoffOpenTargetError> {
    let result = match result {
        Ok(FeedbackReadInvocationResultV1::Get(result)) => result,
        Ok(_)
        | Err(FeedbackReadOwnerErrorV1::Unavailable)
        | Err(FeedbackReadOwnerErrorV1::Contract(_)) => {
            return Err(HandoffOpenTargetError::Unavailable);
        }
        Err(FeedbackReadOwnerErrorV1::NotFoundOrNotAuthorized) => return Ok(None),
    };
    let application = match result {
        Ok(application) => application,
        Err(problem)
            if problem.problem.kind() == ApplicationProblemKind::NotFoundOrNotAuthorized =>
        {
            return Ok(None);
        }
        Err(_) => return Err(HandoffOpenTargetError::Unavailable),
    };
    match application.outcome {
        ApplicationOutcome::Evidence(evidence) => evidence
            .payload
            .ok_or(HandoffOpenTargetError::Unavailable)
            .map(|result| Some(result.finding)),
        ApplicationOutcome::Preview(_) | ApplicationOutcome::Effect(_) => {
            Err(HandoffOpenTargetError::Unavailable)
        }
    }
}

pub(super) async fn execute_handoff_application(
    registered: RegisteredWorkRuntime,
    feedback: Option<Arc<FeedbackRuntime>>,
    request_id: String,
    request: HandoffApplicationInvocationV1,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let operation_key = request.operation_key();
    let Some((_, capability, use_case)) =
        tracedecay_application::HANDOFF_APPLICATION_OPERATION_IDS_V1
            .iter()
            .find(|(operation, _, _)| *operation == operation_key)
    else {
        return DaemonInvocationResponse::problem(
            request_id,
            DaemonInvocationProblem::InvalidRequest,
        );
    };
    let (context, canonical_request_id, use_case) = match handoff_request_context(
        &registered,
        &request_id,
        capability,
        use_case,
        observed_at,
        deadline.clone(),
        cancellation,
    ) {
        Ok(context) => context,
        Err(HandoffRequestContextError::Cancelled) => {
            return application_problem(
                request_id,
                ApplicationProblem::cancelled_before_admission(),
            );
        }
        Err(HandoffRequestContextError::TimedOut) => {
            return application_problem(
                request_id,
                ApplicationProblem::timed_out_before_admission(),
            );
        }
        Err(HandoffRequestContextError::Problem(problem)) => {
            return DaemonInvocationResponse::problem(request_id, problem);
        }
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
    let authority = match registered.database.handoff_open_storage() {
        Ok(authority) => authority,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    let authority_snapshot = match HandoffAuthoritySnapshotV1::new(
        registered.authority_digest.clone(),
        registered.policy_digest.clone(),
    ) {
        Ok(authority) => authority,
        Err(_) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    let targets = DaemonHandoffOpenTargets {
        feedback,
        work: match registered.database.work_storage() {
            Ok(work) => work,
            Err(_) => {
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::Unavailable,
                );
            }
        },
        observed_at,
        recheck_sequence: Arc::new(AtomicU64::new(0)),
    };
    let service = HandoffOpenService::new(authority, targets);
    let result = match request {
        HandoffApplicationInvocationV1::IssueTaskHandoff(request) => service
            .issue_task(&context, request, authority_snapshot, observed_at)
            .await
            .and_then(|grant| tracedecay_application::IssueTaskHandoffResultV1::from_grant(&grant))
            .map(HandoffApplicationResult::IssueTask),
        HandoffApplicationInvocationV1::ListTaskHandoffs(request) => service
            .list_task(&context, request, observed_at)
            .await
            .map(HandoffApplicationResult::ListTask),
        HandoffApplicationInvocationV1::OpenInvestigationHandoff(request) => service
            .open_investigation(&context, request, authority_snapshot, observed_at)
            .await
            .map(HandoffApplicationResult::Investigation),
        HandoffApplicationInvocationV1::OpenTaskHandoff(request) => service
            .open_task(&context, request, authority_snapshot, observed_at)
            .await
            .map(HandoffApplicationResult::Task),
    };
    let result = match result {
        Ok(result) => result,
        Err(HandoffOpenError::Cancelled) => {
            return application_problem(
                request_id,
                ApplicationProblem::cancelled_before_admission(),
            );
        }
        Err(HandoffOpenError::TimedOut) => {
            return application_problem(
                request_id,
                ApplicationProblem::timed_out_before_admission(),
            );
        }
        Err(HandoffOpenError::InvalidToken | HandoffOpenError::NotFoundOrNotAuthorized) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        }
        Err(HandoffOpenError::AuthorityUnavailable) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
        Err(
            HandoffOpenError::InvalidBinding
            | HandoffOpenError::InvalidExpiry
            | HandoffOpenError::Conflict,
        ) => {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::InvalidRequest,
            );
        }
    };
    complete_handoff_effect(
        &registered,
        request_id,
        &context,
        canonical_request_id,
        operation_key,
        use_case,
        input_digest,
        result,
        observed_at,
        deadline,
    )
}

enum HandoffRequestContextError {
    Cancelled,
    TimedOut,
    Problem(DaemonInvocationProblem),
}

#[allow(clippy::too_many_arguments)]
fn handoff_request_context(
    registered: &RegisteredWorkRuntime,
    request_id: &str,
    capability: &str,
    use_case: &str,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<(RequestContext, RequestId, UseCaseId), HandoffRequestContextError> {
    let capability = CapabilityId::new(capability)
        .map_err(|_| HandoffRequestContextError::Problem(DaemonInvocationProblem::Unavailable))?;
    let use_case = UseCaseId::new(use_case)
        .map_err(|_| HandoffRequestContextError::Problem(DaemonInvocationProblem::Unavailable))?;
    let canonical_request_id = RequestId::new(request_id).map_err(|_| {
        HandoffRequestContextError::Problem(DaemonInvocationProblem::InvalidRequest)
    })?;
    let context = RequestContext::new(
        registered.actor.clone(),
        registered.grant.scope.clone(),
        registered.grant.clone(),
        canonical_request_id.clone(),
        deadline,
        cancellation,
    )
    .map_err(|_| {
        HandoffRequestContextError::Problem(DaemonInvocationProblem::NotFoundOrNotAuthorized)
    })?;
    match context.admission_at(observed_at) {
        RequestAdmission::Cancelled => return Err(HandoffRequestContextError::Cancelled),
        RequestAdmission::TimedOut => return Err(HandoffRequestContextError::TimedOut),
        RequestAdmission::Admitted => {}
    }
    if !context.allows(&capability, &use_case) {
        return Err(HandoffRequestContextError::Problem(
            DaemonInvocationProblem::NotFoundOrNotAuthorized,
        ));
    }
    Ok((context, canonical_request_id, use_case))
}

enum HandoffApplicationResult {
    IssueTask(tracedecay_application::IssueTaskHandoffResultV1),
    ListTask(tracedecay_application::ListTaskHandoffsResultV1),
    Investigation(tracedecay_application::OpenInvestigationHandoffResultV1),
    Task(tracedecay_application::OpenTaskHandoffResultV1),
}

#[allow(clippy::too_many_arguments)]
fn complete_handoff_effect(
    registered: &RegisteredWorkRuntime,
    request_id: String,
    context: &RequestContext,
    canonical_request_id: RequestId,
    operation_key: &str,
    use_case: UseCaseId,
    input_digest: ManifestDigest,
    result: HandoffApplicationResult,
    observed_at: UtcMicros,
    deadline: Deadline,
) -> DaemonInvocationResponse {
    let outcome = match result {
        HandoffApplicationResult::IssueTask(result) => administrative_command_effect(
            HANDOFF_EFFECT_FAMILY,
            registered,
            context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            result,
            observed_at,
            deadline,
        )
        .map(HandoffApplicationOutcomeV1::IssueTaskHandoff),
        // The read takes the evidence path, not the command-effect path.
        // Minting an effect id, an idempotency key and a durable effect
        // receipt for an operation that committed nothing would put a
        // permanent record of a mutation that never happened into the
        // reconciliation ledger.
        HandoffApplicationResult::ListTask(result) => handoff_evidence(
            registered,
            context,
            operation_key,
            use_case,
            result,
            observed_at,
            deadline,
        )
        .map(HandoffApplicationOutcomeV1::ListTaskHandoffs),
        HandoffApplicationResult::Investigation(result) => administrative_command_effect(
            HANDOFF_EFFECT_FAMILY,
            registered,
            context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            result,
            observed_at,
            deadline,
        )
        .map(HandoffApplicationOutcomeV1::OpenInvestigationHandoff),
        HandoffApplicationResult::Task(result) => administrative_command_effect(
            HANDOFF_EFFECT_FAMILY,
            registered,
            context,
            canonical_request_id,
            operation_key,
            use_case,
            input_digest,
            result,
            observed_at,
            deadline,
        )
        .map(HandoffApplicationOutcomeV1::OpenTaskHandoff),
    };
    let Ok(outcome) = outcome else {
        return DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::Unavailable);
    };
    DaemonInvocationResponse::with_outcome(
        request_id,
        DaemonInvocationOutcome::HandoffApplication {
            scope: context.scope().clone(),
            outcome,
        },
    )
}

/// The read path: an evidence packet for an enumeration that commits nothing.
///
/// Deliberately NOT `administrative_command_effect`. That function mints an
/// `EffectId`, an idempotency key and a `DurableEffect` receipt, all of which
/// assert a committed state change. This operation reads the grant store and
/// leaves it byte-identical, so it carries an operation receipt and a coverage
/// claim instead — and the coverage claim is only `Complete` when the
/// enumeration did not hit its ceiling.
fn handoff_evidence(
    registered: &RegisteredWorkRuntime,
    context: &RequestContext,
    operation_key: &str,
    use_case: UseCaseId,
    result: tracedecay_application::ListTaskHandoffsResultV1,
    observed_at: UtcMicros,
    deadline: Deadline,
) -> Result<
    ApplicationOutcome<tracedecay_application::ListTaskHandoffsResultV1>,
    ApplicationContractError,
> {
    let (authority, execution) = administrative_authority(
        HANDOFF_EFFECT_FAMILY,
        registered,
        context,
        operation_key,
        &use_case,
        observed_at,
        deadline,
    )?;
    let returned = result.handoffs.len() as u64;
    // A truncated read has no eligible denominator: the store holds an unknown
    // number of further grants past the ceiling. Claiming `Complete` over the
    // rows that happened to fit would be a coverage lie.
    let coverage = if result.truncated {
        EvidenceCoverage {
            requested_domains: vec![EvidenceDomain::Operational],
            visited: Some(returned),
            eligible: None,
            returned,
            completeness: CoverageCompleteness::Partial,
            domains: vec![CoverageDomainState {
                domain: EvidenceDomain::Operational,
                completeness: CoverageCompleteness::Partial,
            }],
        }
    } else {
        EvidenceCoverage::complete(
            vec![EvidenceDomain::Operational],
            returned,
            returned,
            returned,
        )?
    };
    coverage.validate()?;
    Ok(ApplicationOutcome::Evidence(EvidencePacket {
        temporal: TemporalState::current(execution.ended_at),
        authority,
        evidence_authorities: Vec::new(),
        coverage,
        omissions: Vec::new(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page: PageState::first_page(
            SortContractId::new("sort.handoff.list-task.issued-desc.v1").map_err(|_| {
                ApplicationContractError::Inconsistent {
                    field: "handoff list sort contract",
                }
            })?,
            tracedecay_application::MAX_HANDOFF_LIST_RESULTS_V1.into(),
            (!result.truncated).then_some(returned),
            returned,
        )?,
        execution,
        payload: Some(result),
    }))
}
