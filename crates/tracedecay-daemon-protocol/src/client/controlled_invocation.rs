//! Cancellation and authoritative-settlement control for daemon invocations.

use std::time::Duration;

use super::{
    CancellationSignal, CancellationStage, DaemonInvocationClient, DaemonInvocationError, Deadline,
    InvocationCancellationPolicy, deadline_remaining, wait_for_cancellation,
};

impl DaemonInvocationClient {
    #[hotpath::measure(label = "daemon.client.invoke_controlled", future = true)]
    pub async fn invoke_controlled(
        &self,
        request: crate::contract::DaemonInvocationRequest,
        deadline: Deadline,
        cancellation: CancellationSignal,
        policy: InvocationCancellationPolicy,
    ) -> Result<crate::contract::DaemonInvocationResponse, DaemonInvocationError> {
        if cancellation.is_cancelled() {
            return Err(DaemonInvocationError::Cancelled {
                stage: CancellationStage::BeforeAdmission,
            });
        }
        let remaining = deadline_remaining(&deadline).ok_or(DaemonInvocationError::TimedOut {
            stage: CancellationStage::BeforeAdmission,
        })?;
        let target_request_id = request.request_id.clone();
        let client = self.clone();
        tokio::spawn(async move {
            let stage = match policy {
                InvocationCancellationPolicy::ReadOnly => CancellationStage::DuringRead,
                InvocationCancellationPolicy::AuthoritativeEffect => {
                    CancellationStage::EffectInFlight
                }
            };
            let outcome = {
                let invocation = client.invoke(request);
                tokio::pin!(invocation);
                let cancellation_wait = wait_for_cancellation(cancellation);
                tokio::pin!(cancellation_wait);
                let timed_out = tokio::select! {
                    result = &mut invocation => return result.map_err(|_| DaemonInvocationError::Unavailable),
                    () = &mut cancellation_wait => false,
                    () = tokio::time::sleep(remaining) => true,
                };
                let _ = tokio::time::timeout(
                    Duration::from_millis(250),
                    client.cancel_invocation(&target_request_id),
                )
                .await;
                match policy {
                    InvocationCancellationPolicy::ReadOnly if timed_out => {
                        Err(DaemonInvocationError::TimedOut { stage })
                    }
                    InvocationCancellationPolicy::ReadOnly => {
                        Err(DaemonInvocationError::Cancelled { stage })
                    }
                    InvocationCancellationPolicy::AuthoritativeEffect => {
                        // An authoritative effect settles itself; keep reading
                        // over the same response grace the daemon's own
                        // clients use so its real terminal (e.g. a
                        // `PartialEffect` with a committed receipt) is the
                        // one reported, exactly as `settle_in_process_invocation`
                        // does. A transport failure after the cancel attempt
                        // is the same indeterminate state as an unanswered
                        // grace: the effect may have committed, so a
                        // retry-inviting `Unavailable` would be untruthful.
                        match tokio::time::timeout(
                            crate::connection::DAEMON_TOOL_RESPONSE_GRACE,
                            &mut invocation,
                        )
                        .await
                        {
                            Ok(Ok(response)) => Ok(response),
                            Ok(Err(_)) | Err(_) => Ok(
                                crate::contract::DaemonInvocationResponse::problem(
                                    target_request_id,
                                    crate::contract::DaemonInvocationProblem::ResetRequired,
                                ),
                            ),
                        }
                    }
                }
            };
            let indeterminate_effect = matches!(
                &outcome,
                Ok(crate::contract::DaemonInvocationResponse {
                    outcome:
                        crate::contract::DaemonInvocationOutcome::Problem {
                            problem: crate::contract::DaemonInvocationProblem::ResetRequired,
                        },
                    ..
                })
            );
            if matches!(
                outcome,
                Err(
                    DaemonInvocationError::Cancelled { .. }
                        | DaemonInvocationError::TimedOut { .. }
                )
            ) || indeterminate_effect
            {
                *client.state.lock().await = None;
            }
            outcome
        })
        .await
        .map_err(|_| DaemonInvocationError::Unavailable)?
    }
}
