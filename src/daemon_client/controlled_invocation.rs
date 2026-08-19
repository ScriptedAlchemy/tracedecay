//! Cancellation and authoritative-settlement control for daemon invocations.

use std::time::Duration;

use super::{
    CancellationSignal, CancellationStage, DaemonInvocationClient, DaemonInvocationError, Deadline,
    InvocationCancellationPolicy, deadline_remaining, wait_for_cancellation,
};

impl DaemonInvocationClient {
    pub(crate) async fn invoke_controlled(
        &self,
        request: crate::daemon_contract::DaemonInvocationRequest,
        deadline: Deadline,
        cancellation: CancellationSignal,
        policy: InvocationCancellationPolicy,
    ) -> Result<crate::daemon_contract::DaemonInvocationResponse, DaemonInvocationError> {
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
                        // An authoritative effect settles itself: its own
                        // budget bounds it, and when that budget expires after
                        // the commit point it reports `PartialEffect` with a
                        // committed receipt. Waiting only
                        // `DAEMON_TASK_ABORT_DEADLINE` — two seconds, a
                        // *shutdown* bound — replaced that answer with a
                        // fabricated `ResetRequired` whenever settlement took
                        // a moment longer, exactly as the in-process executor
                        // once did (`settle_in_process_invocation`). Keep
                        // reading over the same response grace the daemon's
                        // own clients use so the effect's real terminal is
                        // the one reported.
                        match tokio::time::timeout(
                            crate::daemon::DAEMON_TOOL_RESPONSE_GRACE,
                            &mut invocation,
                        )
                        .await
                        {
                            Ok(result) => result.map_err(|_| DaemonInvocationError::Unavailable),
                            Err(_) => Ok(
                                crate::daemon_contract::DaemonInvocationResponse::problem(
                                    target_request_id,
                                    crate::daemon_contract::DaemonInvocationProblem::ResetRequired,
                                ),
                            ),
                        }
                    }
                }
            };
            let indeterminate_effect = matches!(
                &outcome,
                Ok(crate::daemon_contract::DaemonInvocationResponse {
                    outcome:
                        crate::daemon_contract::DaemonInvocationOutcome::Problem {
                            problem: crate::daemon_contract::DaemonInvocationProblem::ResetRequired,
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
