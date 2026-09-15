use super::*;
use crate::project_runtime::ProjectRuntimePublicationStateV1;

/// The response for an admitted project route whose retained runtime is not
/// registered.
///
/// The retained runtime registers in the full server's owner phase, behind the
/// core publication that already admits this route. Under a warming
/// publication the missing runtime is that owner still mounting — the same
/// retryable state the primitive and configuration owners answer — and under
/// a failed publication it is the terminal owner failure. Only a settled
/// publication without a retained owner (a read-only project database mounts
/// none) is truthfully "no retained runtime is registered for this scope".
pub(super) fn missing_retained_runtime_problem(
    publication: Option<ProjectRuntimePublicationStateV1>,
    request_id: String,
) -> DaemonInvocationResponse {
    if matches!(
        publication,
        Some(ProjectRuntimePublicationStateV1::Warming | ProjectRuntimePublicationStateV1::Failed)
    ) {
        return missing_registered_owner_problem(publication, request_id);
    }
    DaemonInvocationResponse::with_outcome(
        request_id,
        DaemonInvocationOutcome::ApplicationProblem {
            problem: ApplicationProblem::unavailable(SafeDiagnostic {
                code: "application.retained.authority-unavailable".to_owned(),
                message: "The retained application authority is unavailable: \
                          no retained runtime is registered for this scope."
                    .to_owned(),
            }),
        },
    )
}

#[hotpath::measure(label = "daemon.service.retained.execute", future = true)]
pub(super) async fn execute_retained_application(
    request_id: String,
    registered: RegisteredRetainedRuntime,
    request: tracedecay_contracts::retained_surfaces::RetainedSurfaceRequestV1,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
    request_cancellation: tracedecay_runtime_core::cancellation::CancellationToken,
) -> DaemonInvocationResponse {
    let effective_deadline = Deadline {
        expires_at: UtcMicros(deadline.expires_at.0.min(registered.grant.expires_at.0)),
    };
    let context = match RequestId::new(request_id.clone()).and_then(|request_id| {
        RequestContext::new(
            registered.actor.clone(),
            registered.scope.clone(),
            registered.grant.clone(),
            request_id,
            effective_deadline,
            cancellation,
        )
    }) {
        Ok(context) => context,
        Err(_) => {
            return DaemonInvocationResponse::with_outcome(
                request_id,
                DaemonInvocationOutcome::ApplicationProblem {
                    problem: ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
                },
            );
        }
    };
    let cancellation_signal = match tracedecay_contracts::CancellationSignal::active(
        context.cancellation().token_id.as_str(),
    ) {
        Ok(signal) => signal,
        Err(_) => {
            return DaemonInvocationResponse::with_outcome(
                request_id,
                DaemonInvocationOutcome::ApplicationProblem {
                    problem: ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
                },
            );
        }
    };
    if let CancellationState::Cancelled { requested_at } = &context.cancellation().state {
        cancellation_signal.cancel(*requested_at);
    }
    let service = tracedecay_contracts::retained_surfaces::RetainedSurfaceServiceV1::new(
        registered.ports.as_ref().clone(),
    );
    let execution = hotpath::future!(
        service.execute(&context, &cancellation_signal, observed_at, &request),
        label = "daemon.service.retained.handler"
    );
    tokio::pin!(execution);
    let outcome = tokio::select! {
        outcome = &mut execution => outcome,
        () = request_cancellation.cancelled() => {
            hotpath::gauge!("daemon.service.retained.cancelled_total").inc(1_u64);
            cancellation_signal.cancel(now_micros());
            execution.await
        }
    };
    match outcome {
        Ok(outcome) => DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::RetainedApplication {
                scope: registered.scope,
                outcome,
            },
        ),
        Err(problem) => DaemonInvocationResponse::retained_application_problem(
            request_id,
            registered.scope,
            problem,
        ),
    }
}
