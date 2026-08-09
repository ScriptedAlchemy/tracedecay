use super::*;

pub(super) async fn execute_retained_application(
    request_id: String,
    registered: Option<RegisteredRetainedRuntime>,
    request: tracedecay_application::retained_surfaces::RetainedSurfaceRequestV1,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
    request_cancellation: tracedecay_runtime_core::cancellation::CancellationToken,
) -> DaemonInvocationResponse {
    let Some(registered) = registered else {
        return DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::ApplicationProblem {
                problem: ApplicationProblem::unavailable(SafeDiagnostic {
                    code: "application.retained.authority-unavailable".to_owned(),
                    message: "The retained application authority is unavailable.".to_owned(),
                }),
            },
        );
    };
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
    let cancellation_signal = match tracedecay_application::CancellationSignal::active(
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
    let service = tracedecay_application::retained_surfaces::RetainedSurfaceServiceV1::new(
        registered.ports.as_ref().clone(),
    );
    let execution = service.execute(&context, &cancellation_signal, observed_at, &request);
    tokio::pin!(execution);
    let outcome = tokio::select! {
        outcome = &mut execution => outcome,
        () = request_cancellation.cancelled() => {
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
        Err(problem) => DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::ApplicationProblem { problem },
        ),
    }
}
