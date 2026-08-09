//! Typed client for one daemon-owned LSP session.
//!
//! Relocated verbatim from `daemon_client.rs` as a pure structural split; no
//! logic, naming, or visibility changes beyond the imports this file needs.

use tracedecay_application::{CancellationSignal, Deadline};
use tracedecay_lsp::{FramePoll, FrameSend};

use super::{
    ConnectionLocalRequestSequence, DaemonInvocationClient, InvocationCancellationPolicy,
    InvocationError, invocation_error_from_problem, map_invocation_error,
};

/// Typed client for one daemon-owned LSP session. Every method maps to a
/// closed invocation operation; no method exposes a generic local socket.
pub struct DaemonLspSessionClient {
    invocation: DaemonInvocationClient,
    session: crate::daemon_contract::DaemonLspSessionAccess,
    scope_set_id: Option<tracedecay_domain::ScopeSetId>,
    scope_set_digest: Option<tracedecay_domain::ManifestDigest>,
    next_request: ConnectionLocalRequestSequence,
}

impl DaemonLspSessionClient {
    pub async fn open(
        invocation: DaemonInvocationClient,
        client_revision: impl Into<String>,
        requested_root_uri: Option<String>,
        workspace_folders: Vec<String>,
        deadline: Deadline,
        cancellation: CancellationSignal,
    ) -> Result<Self, InvocationError> {
        let cancellation_context = cancellation.context();
        let response = invocation
            .invoke_controlled(
                crate::daemon_contract::DaemonInvocationRequest::lsp_open(
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
        let crate::daemon_contract::DaemonInvocationOutcome::LspOpened {
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
                crate::daemon_contract::DaemonInvocationRequest::lsp_frame(
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
            crate::daemon_contract::DaemonInvocationOutcome::LspFrameAccepted {
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

    pub async fn poll_daemon_frame(
        &mut self,
        deadline: Deadline,
        cancellation: CancellationSignal,
    ) -> Result<FramePoll, InvocationError> {
        let request_id = self.next_request_id()?;
        let cancellation_context = cancellation.context();
        let response = self
            .invoke(
                crate::daemon_contract::DaemonInvocationRequest::lsp_poll(
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
            crate::daemon_contract::DaemonInvocationOutcome::LspFrame { frame, closed } => {
                Ok(match (frame, closed) {
                    (Some(frame), _) => FramePoll::Frame(frame.into_bytes()),
                    (None, true) => FramePoll::Closed,
                    (None, false) => FramePoll::Pending,
                })
            }
            outcome => Err(invocation_outcome_error(outcome)),
        }
    }

    pub async fn acknowledge_daemon_frame(
        &mut self,
        deadline: Deadline,
        cancellation: CancellationSignal,
    ) -> Result<(), InvocationError> {
        let request_id = self.next_request_id()?;
        let cancellation_context = cancellation.context();
        let response = self
            .invoke(
                crate::daemon_contract::DaemonInvocationRequest::lsp_acknowledge(
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
            crate::daemon_contract::DaemonInvocationOutcome::LspAcknowledged { .. } => Ok(()),
            outcome => Err(invocation_outcome_error(outcome)),
        }
    }

    pub async fn reconnect(
        &mut self,
        deadline: Deadline,
        cancellation: CancellationSignal,
    ) -> Result<(), InvocationError> {
        let request_id = self.next_request_id()?;
        let cancellation_context = cancellation.context();
        let response = self
            .invoke(
                crate::daemon_contract::DaemonInvocationRequest::lsp_reconnect(
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
            crate::daemon_contract::DaemonInvocationOutcome::LspReconnected { session } => {
                self.session = session;
                Ok(())
            }
            outcome => Err(invocation_outcome_error(outcome)),
        }
    }

    pub async fn detach(
        &mut self,
        deadline: Deadline,
        cancellation: CancellationSignal,
    ) -> Result<(), InvocationError> {
        let request_id = self.next_request_id()?;
        let cancellation_context = cancellation.context();
        let response = self
            .invoke(
                crate::daemon_contract::DaemonInvocationRequest::lsp_detach(
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
            crate::daemon_contract::DaemonInvocationOutcome::LspDetached => Ok(()),
            outcome => Err(invocation_outcome_error(outcome)),
        }
    }

    async fn invoke(
        &self,
        request: crate::daemon_contract::DaemonInvocationRequest,
        deadline: Deadline,
        cancellation: CancellationSignal,
    ) -> Result<crate::daemon_contract::DaemonInvocationResponse, InvocationError> {
        self.invocation
            .invoke_controlled(
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

fn invocation_outcome_error(
    outcome: crate::daemon_contract::DaemonInvocationOutcome,
) -> InvocationError {
    match outcome {
        crate::daemon_contract::DaemonInvocationOutcome::ApplicationProblem { problem } => {
            invocation_error_from_problem(&problem)
        }
        crate::daemon_contract::DaemonInvocationOutcome::Problem { problem } => match problem {
            crate::daemon_contract::DaemonInvocationProblem::InvalidRequest
            | crate::daemon_contract::DaemonInvocationProblem::UnsupportedRevision => {
                InvocationError::InvalidRequest
            }
            crate::daemon_contract::DaemonInvocationProblem::NotFoundOrNotAuthorized => {
                InvocationError::Denied
            }
            crate::daemon_contract::DaemonInvocationProblem::ResetRequired => {
                InvocationError::Unavailable
            }
            crate::daemon_contract::DaemonInvocationProblem::ApplicationContractViolation => {
                InvocationError::Unavailable
            }
            crate::daemon_contract::DaemonInvocationProblem::Unavailable => {
                InvocationError::Unavailable
            }
        },
        _ => InvocationError::Unavailable,
    }
}
