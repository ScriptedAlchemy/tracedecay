//! Typed client for one daemon-owned LSP session.
//!
//! Relocated verbatim from `daemon_client.rs` as a pure structural split; no
//! logic, naming, or visibility changes beyond the imports this file needs.

use crate::lsp_wire::{FramePoll, FrameSend};
use tracedecay_application::{CancellationSignal, Deadline};

use super::{
    ConnectionLocalRequestSequence, DaemonInvocationClient, InvocationCancellationPolicy,
    InvocationConnectionLease, InvocationError, invocation_error_from_problem,
    map_invocation_error,
};

/// Typed client for one daemon-owned LSP session. Every method maps to a
/// closed invocation operation; no method exposes a generic local socket.
pub struct DaemonLspSessionClient {
    invocation: DaemonInvocationClient,
    connection: Option<InvocationConnectionLease>,
    session: crate::contract::DaemonLspSessionAccess,
    scope_set_id: Option<tracedecay_domain::ScopeSetId>,
    scope_set_digest: Option<tracedecay_domain::ManifestDigest>,
    next_request: ConnectionLocalRequestSequence,
}

impl DaemonLspSessionClient {
    #[hotpath::measure(label = "daemon.client.lsp.open", future = true)]
    pub async fn open(
        invocation: DaemonInvocationClient,
        client_revision: impl Into<String>,
        requested_root_uri: Option<String>,
        workspace_folders: Vec<String>,
        deadline: Deadline,
        cancellation: CancellationSignal,
    ) -> Result<Self, InvocationError> {
        let cancellation_context = cancellation.context();
        let (mut connection, _in_flight) = invocation
            .checkout_connection_controlled(&deadline, &cancellation)
            .await
            .map_err(map_invocation_error)?;
        let response = invocation
            .invoke_controlled_on_connection(
                &mut connection,
                crate::contract::DaemonInvocationRequest::lsp_open(
                    "lsp.1",
                    client_revision,
                    requested_root_uri,
                    workspace_folders,
                    deadline.clone(),
                    cancellation_context,
                ),
                deadline,
                cancellation,
                InvocationCancellationPolicy::ReadOnly,
            )
            .await
            .map_err(map_invocation_error)?;
        let crate::contract::DaemonInvocationOutcome::LspOpened {
            session,
            scope_set_id,
            scope_set_digest,
            ..
        } = response.outcome
        else {
            return Err(invocation_outcome_error(response.outcome));
        };
        Ok(Self {
            invocation,
            connection: Some(connection),
            session,
            scope_set_id,
            scope_set_digest,
            next_request: ConnectionLocalRequestSequence::starting_at(2),
        })
    }

    pub fn scope_set_id(&self) -> Option<&tracedecay_domain::ScopeSetId> {
        self.scope_set_id.as_ref()
    }

    pub fn scope_set_digest(&self) -> Option<&tracedecay_domain::ManifestDigest> {
        self.scope_set_digest.as_ref()
    }

    #[hotpath::skip]
    pub async fn try_send_client_frame(
        &mut self,
        frame: &str,
        deadline: Deadline,
        cancellation: CancellationSignal,
    ) -> Result<FrameSend, InvocationError> {
        let request_id = self.next_request_id()?;
        let cancellation_context = cancellation.context();
        let response = self
            .invoke(
                crate::contract::DaemonInvocationRequest::lsp_frame(
                    request_id,
                    self.session.clone(),
                    frame,
                    deadline.clone(),
                    cancellation_context,
                ),
                deadline,
                cancellation,
            )
            .await?;
        match response.outcome {
            crate::contract::DaemonInvocationOutcome::LspFrameAccepted {
                backpressured,
                closed,
            } => Ok(if closed {
                FrameSend::Closed
            } else if backpressured {
                FrameSend::Backpressured
            } else {
                FrameSend::Sent
            }),
            outcome => Err(invocation_outcome_error(outcome)),
        }
    }

    #[hotpath::skip]
    pub async fn poll_daemon_frame(
        &mut self,
        deadline: Deadline,
        cancellation: CancellationSignal,
    ) -> Result<FramePoll, InvocationError> {
        let request_id = self.next_request_id()?;
        let cancellation_context = cancellation.context();
        let response = self
            .invoke(
                crate::contract::DaemonInvocationRequest::lsp_poll(
                    request_id,
                    self.session.clone(),
                    deadline.clone(),
                    cancellation_context,
                ),
                deadline,
                cancellation,
            )
            .await?;
        match response.outcome {
            crate::contract::DaemonInvocationOutcome::LspFrame { frame, closed } => {
                Ok(match (frame, closed) {
                    (Some(frame), _) => FramePoll::Frame(frame.into_bytes()),
                    (None, true) => FramePoll::Closed,
                    (None, false) => FramePoll::Pending,
                })
            }
            outcome => Err(invocation_outcome_error(outcome)),
        }
    }

    #[hotpath::skip]
    pub async fn acknowledge_daemon_frame(
        &mut self,
        deadline: Deadline,
        cancellation: CancellationSignal,
    ) -> Result<(), InvocationError> {
        let request_id = self.next_request_id()?;
        let cancellation_context = cancellation.context();
        let response = self
            .invoke(
                crate::contract::DaemonInvocationRequest::lsp_acknowledge(
                    request_id,
                    self.session.clone(),
                    deadline.clone(),
                    cancellation_context,
                ),
                deadline,
                cancellation,
            )
            .await?;
        match response.outcome {
            crate::contract::DaemonInvocationOutcome::LspAcknowledged { .. } => Ok(()),
            outcome => Err(invocation_outcome_error(outcome)),
        }
    }

    #[hotpath::skip]
    pub async fn reconnect(
        &mut self,
        deadline: Deadline,
        cancellation: CancellationSignal,
    ) -> Result<(), InvocationError> {
        let request_id = self.next_request_id()?;
        let cancellation_context = cancellation.context();
        // Reconnect exists because the pinned connection is already known
        // broken: an interrupted invocation took its transport out of the
        // lease. Reusing it would fail every reconnect with `Unavailable`, so
        // retire the lease without returning it to the pool — its drop also
        // discards any idle sibling that shared the same failed daemon
        // process — and resume the session over a freshly handshaked one.
        drop(self.connection.take());
        let (mut connection, _in_flight) = self
            .invocation
            .checkout_connection_controlled(&deadline, &cancellation)
            .await
            .map_err(map_invocation_error)?;
        let response = self
            .invocation
            .invoke_controlled_on_connection(
                &mut connection,
                crate::contract::DaemonInvocationRequest::lsp_reconnect(
                    request_id,
                    self.session.clone(),
                    deadline.clone(),
                    cancellation_context,
                ),
                deadline,
                cancellation,
                InvocationCancellationPolicy::ReadOnly,
            )
            .await
            .map_err(map_invocation_error)?;
        match response.outcome {
            crate::contract::DaemonInvocationOutcome::LspReconnected { session } => {
                self.session = session;
                self.connection = Some(connection);
                Ok(())
            }
            outcome => Err(invocation_outcome_error(outcome)),
        }
    }

    #[hotpath::skip]
    pub async fn detach(
        &mut self,
        deadline: Deadline,
        cancellation: CancellationSignal,
    ) -> Result<(), InvocationError> {
        let request_id = self.next_request_id()?;
        let cancellation_context = cancellation.context();
        let response = self
            .invoke(
                crate::contract::DaemonInvocationRequest::lsp_detach(
                    request_id,
                    self.session.clone(),
                    deadline.clone(),
                    cancellation_context,
                ),
                deadline,
                cancellation,
            )
            .await?;
        match response.outcome {
            crate::contract::DaemonInvocationOutcome::LspDetached => {
                if let Some(mut connection) = self.connection.take() {
                    connection.release_to_pool();
                }
                Ok(())
            }
            outcome => Err(invocation_outcome_error(outcome)),
        }
    }

    #[hotpath::skip]
    async fn invoke(
        &mut self,
        request: crate::contract::DaemonInvocationRequest,
        deadline: Deadline,
        cancellation: CancellationSignal,
    ) -> Result<crate::contract::DaemonInvocationResponse, InvocationError> {
        let connection = self
            .connection
            .as_mut()
            .ok_or(InvocationError::Unavailable)?;
        self.invocation
            .invoke_controlled_on_connection(
                connection,
                request,
                deadline,
                cancellation,
                InvocationCancellationPolicy::ReadOnly,
            )
            .await
            .map_err(map_invocation_error)
    }

    fn next_request_id(&mut self) -> Result<String, InvocationError> {
        self.next_request
            .next_string("lsp.")
            .map_err(|_| InvocationError::Unavailable)
    }
}

fn invocation_outcome_error(outcome: crate::contract::DaemonInvocationOutcome) -> InvocationError {
    match outcome {
        crate::contract::DaemonInvocationOutcome::ApplicationProblem { problem } => {
            invocation_error_from_problem(&problem)
        }
        crate::contract::DaemonInvocationOutcome::Problem { problem } => match problem {
            crate::contract::DaemonInvocationProblem::InvalidRequest
            | crate::contract::DaemonInvocationProblem::UnsupportedRevision => {
                InvocationError::InvalidRequest
            }
            crate::contract::DaemonInvocationProblem::NotFoundOrNotAuthorized => {
                InvocationError::Denied
            }
            crate::contract::DaemonInvocationProblem::ResetRequired => InvocationError::Unavailable,
            crate::contract::DaemonInvocationProblem::ApplicationContractViolation => {
                InvocationError::Unavailable
            }
            crate::contract::DaemonInvocationProblem::Unavailable => InvocationError::Unavailable,
        },
        _ => InvocationError::Unavailable,
    }
}
