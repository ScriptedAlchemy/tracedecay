//! Cancellation and authoritative-settlement control for daemon invocations.

use std::time::Duration;

use super::{
    CancellationSignal, CancellationStage, DaemonInvocationClient, DaemonInvocationError,
    DaemonInvocationResult, Deadline, InvocationCancellationPolicy, InvocationConnectionLease,
    deadline_remaining, wait_for_cancellation, with_daemon_version_skew_context,
};
use crate::connection::{DAEMON_CONNECT_DOWN, DAEMON_CONNECT_SATURATED};

enum ControlledConnectionOutcome {
    Completed(tracedecay_domain::errors::Result<crate::contract::DaemonInvocationResponse>),
    Cancelled,
    TimedOut,
    Indeterminate,
}

/// Classify one transport-level invoke failure for controlled callers.
///
/// A connect-phase failure (`daemon_connect_down` / `daemon_connect_saturated`
/// after the restart grace) means the request was never sent; it becomes the
/// typed [`DaemonInvocationError::Unreachable`] carrying the connect
/// diagnostic. Every other transport failure — a closed connection after the
/// request was written, a stalled response, a refused handshake — keeps the
/// indeterminate [`DaemonInvocationError::Unavailable`].
pub(super) fn classify_invoke_transport_error(
    error: tracedecay_domain::errors::TraceDecayError,
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
    pub(super) async fn checkout_connection_controlled(
        &self,
        deadline: &Deadline,
        cancellation: &CancellationSignal,
    ) -> Result<
        (
            InvocationConnectionLease,
            super::DaemonInvocationClientActivityGuard,
        ),
        DaemonInvocationError,
    > {
        if cancellation.is_cancelled() {
            return Err(DaemonInvocationError::Cancelled {
                stage: CancellationStage::BeforeAdmission,
            });
        }
        let remaining = deadline_remaining(deadline).ok_or(DaemonInvocationError::TimedOut {
            stage: CancellationStage::BeforeAdmission,
        })?;
        let checkout = self.checkout_connection();
        tokio::pin!(checkout);
        let cancellation_wait = wait_for_cancellation(cancellation.clone());
        tokio::pin!(cancellation_wait);
        tokio::select! {
            result = &mut checkout => result.map_err(|error| {
                classify_invoke_transport_error(with_daemon_version_skew_context(
                    error,
                    &self.connection,
                    &self.handshake,
                ))
            }),
            () = &mut cancellation_wait => Err(DaemonInvocationError::Cancelled {
                stage: CancellationStage::BeforeAdmission,
            }),
            () = tokio::time::sleep(remaining) => Err(DaemonInvocationError::TimedOut {
                stage: CancellationStage::BeforeAdmission,
            }),
        }
    }

    pub(super) async fn invoke_controlled_on_connection(
        &self,
        lease: &mut InvocationConnectionLease,
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
        let stage = match policy {
            InvocationCancellationPolicy::ReadOnly => CancellationStage::DuringRead,
            InvocationCancellationPolicy::AuthoritativeEffect => CancellationStage::EffectInFlight,
        };
        let outcome = {
            let invocation = self.invoke_on_connection(lease, request);
            tokio::pin!(invocation);
            let cancellation_wait = wait_for_cancellation(cancellation);
            tokio::pin!(cancellation_wait);
            let interrupted = tokio::select! {
                result = &mut invocation => ControlledConnectionOutcome::Completed(result),
                () = &mut cancellation_wait => ControlledConnectionOutcome::Cancelled,
                () = tokio::time::sleep(remaining) => ControlledConnectionOutcome::TimedOut,
            };
            match interrupted {
                ControlledConnectionOutcome::Completed(result) => {
                    ControlledConnectionOutcome::Completed(result)
                }
                interrupted @ (ControlledConnectionOutcome::Cancelled
                | ControlledConnectionOutcome::TimedOut) => {
                    let authoritative_settlement_deadline =
                        tokio::time::Instant::now() + crate::connection::DAEMON_TOOL_RESPONSE_GRACE;
                    let _ = tokio::time::timeout(
                        Duration::from_millis(250),
                        self.cancel_invocation(&target_request_id),
                    )
                    .await;
                    match policy {
                        InvocationCancellationPolicy::ReadOnly => interrupted,
                        InvocationCancellationPolicy::AuthoritativeEffect => {
                            match tokio::time::timeout_at(
                                authoritative_settlement_deadline,
                                &mut invocation,
                            )
                            .await
                            {
                                Ok(Ok(response)) => {
                                    ControlledConnectionOutcome::Completed(Ok(response))
                                }
                                Ok(Err(_)) | Err(_) => ControlledConnectionOutcome::Indeterminate,
                            }
                        }
                    }
                }
                ControlledConnectionOutcome::Indeterminate => {
                    ControlledConnectionOutcome::Indeterminate
                }
            }
        };
        match outcome {
            ControlledConnectionOutcome::Completed(Ok(response)) => Ok(response),
            ControlledConnectionOutcome::Completed(Err(error)) => {
                lease.connection.take();
                Err(classify_invoke_transport_error(error))
            }
            ControlledConnectionOutcome::Cancelled => {
                lease.connection.take();
                Err(DaemonInvocationError::Cancelled { stage })
            }
            ControlledConnectionOutcome::TimedOut => {
                lease.connection.take();
                Err(DaemonInvocationError::TimedOut { stage })
            }
            ControlledConnectionOutcome::Indeterminate => {
                lease.connection.take();
                Ok(crate::contract::DaemonInvocationResponse::problem(
                    target_request_id,
                    crate::contract::DaemonInvocationProblem::ResetRequired,
                ))
            }
        }
    }

    #[hotpath::measure(label = "daemon.client.invoke_controlled", future = true)]
    pub async fn invoke_controlled(
        &self,
        request: crate::contract::DaemonInvocationRequest,
        deadline: Deadline,
        cancellation: CancellationSignal,
        policy: InvocationCancellationPolicy,
    ) -> Result<crate::contract::DaemonInvocationResponse, DaemonInvocationError> {
        self.invoke_controlled_with_delivery(request, deadline, cancellation, policy)
            .await
            .map(DaemonInvocationResult::into_response)
    }

    #[hotpath::measure(label = "daemon.client.invoke_controlled.delivery", future = true)]
    pub async fn invoke_controlled_with_delivery(
        &self,
        request: crate::contract::DaemonInvocationRequest,
        deadline: Deadline,
        cancellation: CancellationSignal,
        policy: InvocationCancellationPolicy,
    ) -> Result<DaemonInvocationResult, DaemonInvocationError> {
        if cancellation.is_cancelled() {
            return Err(DaemonInvocationError::Cancelled {
                stage: CancellationStage::BeforeAdmission,
            });
        }
        deadline_remaining(&deadline).ok_or(DaemonInvocationError::TimedOut {
            stage: CancellationStage::BeforeAdmission,
        })?;
        let target_request_id = request.request_id.clone();
        let delivery_ack_required = request.delivery_ack_deadline().is_some();
        let (mut lease, _in_flight) = self
            .checkout_connection_controlled(&deadline, &cancellation)
            .await?;
        let response = self
            .invoke_controlled_on_connection(&mut lease, request, deadline, cancellation, policy)
            .await?;
        let delivery = if delivery_ack_required && lease.connection.is_some() {
            Some(super::DaemonInvocationDelivery {
                lease,
                target_request_id,
                connection: self.connection.clone(),
            })
        } else {
            lease.release_to_pool();
            None
        };
        Ok(DaemonInvocationResult { response, delivery })
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
        let closed = tracedecay_domain::errors::TraceDecayError::Config {
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
