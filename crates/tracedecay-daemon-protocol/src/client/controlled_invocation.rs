//! Cancellation and authoritative-settlement control for daemon invocations.

use std::time::Duration;

use super::{
    CancellationSignal, CancellationStage, DaemonInvocationClient, DaemonInvocationError, Deadline,
    InvocationCancellationPolicy, deadline_remaining, wait_for_cancellation,
};
use crate::connection::{DAEMON_CONNECT_DOWN, DAEMON_CONNECT_SATURATED};

/// Classify one transport-level invoke failure for controlled callers.
///
/// A connect-phase failure (`daemon_connect_down` / `daemon_connect_saturated`
/// after the restart grace) means the request was never sent; it becomes the
/// typed [`DaemonInvocationError::Unreachable`] carrying the connect
/// diagnostic. Every other transport failure — a closed connection after the
/// request was written, a stalled response, a refused handshake — keeps the
/// indeterminate [`DaemonInvocationError::Unavailable`].
fn classify_invoke_transport_error(
    error: tracedecay_runtime_core::errors::TraceDecayError,
) -> DaemonInvocationError {
    match error.project_route_context() {
        Some((reason_code @ (DAEMON_CONNECT_DOWN | DAEMON_CONNECT_SATURATED), _, detail)) => {
            DaemonInvocationError::Unreachable {
                reason_code: reason_code.to_owned(),
                detail: detail.to_owned(),
            }
        }
        _ => DaemonInvocationError::Unavailable,
    }
}

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
                    result = &mut invocation => return result.map_err(classify_invoke_transport_error),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_phase_failures_classify_as_unreachable_with_their_reason_code() {
        let down = crate::connection::daemon_connect_failure(
            "/tmp/dead-daemon.sock",
            &std::io::Error::from(std::io::ErrorKind::ConnectionRefused),
        );
        match classify_invoke_transport_error(down) {
            DaemonInvocationError::Unreachable {
                reason_code,
                detail,
            } => {
                assert_eq!(reason_code, DAEMON_CONNECT_DOWN);
                assert!(
                    detail.contains("could not connect") && detail.contains("dead-daemon.sock"),
                    "the connect diagnostic must survive classification: {detail}"
                );
            }
            other => panic!("connect-down must classify as unreachable: {other:?}"),
        }

        let saturated = crate::connection::daemon_connect_failure(
            "/tmp/dead-daemon.sock",
            &std::io::Error::from(std::io::ErrorKind::WouldBlock),
        );
        assert!(matches!(
            classify_invoke_transport_error(saturated),
            DaemonInvocationError::Unreachable { reason_code, .. }
                if reason_code == DAEMON_CONNECT_SATURATED
        ));
    }

    #[test]
    fn post_send_transport_failures_stay_indeterminate_unavailable() {
        // A closed connection after the request was written may have an
        // in-flight outcome; it must never classify as never-sent.
        let closed = tracedecay_runtime_core::errors::TraceDecayError::Config {
            message: "daemon closed the invocation connection after 'status' was sent".to_owned(),
        };
        assert_eq!(
            classify_invoke_transport_error(closed),
            DaemonInvocationError::Unavailable
        );

        let stalled = crate::connection::daemon_response_stalled(Duration::from_secs(12));
        assert_eq!(
            classify_invoke_transport_error(stalled),
            DaemonInvocationError::Unavailable
        );
    }
}
