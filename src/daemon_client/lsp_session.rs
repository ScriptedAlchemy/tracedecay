use tracedecay_lsp::{FramePoll, FrameSend};

use super::DaemonInvocationClient;
use crate::request_identity::ConnectionLocalRequestSequence;

/// Typed client for one daemon-owned LSP session. Every method maps to a
/// closed invocation operation; no method exposes a generic local socket.
pub struct DaemonLspSessionClient {
    invocation: DaemonInvocationClient,
    session: crate::daemon_contract::DaemonLspSessionAccess,
    scope_set_id: Option<tracedecay_domain::ScopeSetId>,
    scope_set_digest: Option<tracedecay_domain::ManifestDigest>,
    next_request: ConnectionLocalRequestSequence,
    detached: bool,
}

impl DaemonLspSessionClient {
    pub async fn open(
        invocation: DaemonInvocationClient,
        client_revision: impl Into<String>,
        requested_root_uri: Option<String>,
        workspace_folders: Vec<String>,
    ) -> crate::errors::Result<Self> {
        let response = invocation
            .invoke(crate::daemon_contract::DaemonInvocationRequest::lsp_open(
                "lsp.1",
                client_revision,
                requested_root_uri,
                workspace_folders,
            ))
            .await?;
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
            detached: false,
        })
    }

    pub fn scope_set_id(&self) -> Option<&tracedecay_domain::ScopeSetId> {
        self.scope_set_id.as_ref()
    }

    pub fn scope_set_digest(&self) -> Option<&tracedecay_domain::ManifestDigest> {
        self.scope_set_digest.as_ref()
    }

    pub async fn try_send_client_frame(&mut self, frame: &str) -> crate::errors::Result<FrameSend> {
        let request_id = self.next_request_id()?;
        let response = self
            .invoke(crate::daemon_contract::DaemonInvocationRequest::lsp_frame(
                request_id,
                self.session.clone(),
                frame,
            ))
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

    pub async fn poll_daemon_frame(&mut self) -> crate::errors::Result<FramePoll> {
        let request_id = self.next_request_id()?;
        let response = self
            .invoke(crate::daemon_contract::DaemonInvocationRequest::lsp_poll(
                request_id,
                self.session.clone(),
            ))
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

    pub async fn acknowledge_daemon_frame(&mut self) -> crate::errors::Result<()> {
        let request_id = self.next_request_id()?;
        let response = self
            .invoke(
                crate::daemon_contract::DaemonInvocationRequest::lsp_acknowledge(
                    request_id,
                    self.session.clone(),
                ),
            )
            .await?;
        match response.outcome {
            crate::daemon_contract::DaemonInvocationOutcome::LspAcknowledged { .. } => Ok(()),
            outcome => Err(invocation_outcome_error(outcome)),
        }
    }

    pub async fn reconnect(&mut self) -> crate::errors::Result<()> {
        let request_id = self.next_request_id()?;
        let response = self
            .invoke(
                crate::daemon_contract::DaemonInvocationRequest::lsp_reconnect(
                    request_id,
                    self.session.clone(),
                ),
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

    pub async fn detach(&mut self) -> crate::errors::Result<()> {
        let request_id = self.next_request_id()?;
        let response = self
            .invoke(crate::daemon_contract::DaemonInvocationRequest::lsp_detach(
                request_id,
                self.session.clone(),
            ))
            .await?;
        match response.outcome {
            crate::daemon_contract::DaemonInvocationOutcome::LspDetached => {
                self.detached = true;
                Ok(())
            }
            outcome => Err(invocation_outcome_error(outcome)),
        }
    }

    async fn invoke(
        &self,
        request: crate::daemon_contract::DaemonInvocationRequest,
    ) -> crate::errors::Result<crate::daemon_contract::DaemonInvocationResponse> {
        self.invocation.invoke(request).await
    }

    fn next_request_id(&mut self) -> crate::errors::Result<String> {
        self.next_request.next_string("lsp.").map_err(|error| {
            crate::errors::TraceDecayError::Config {
                message: error.to_string(),
            }
        })
    }
}

impl Drop for DaemonLspSessionClient {
    fn drop(&mut self) {
        if self.detached {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let invocation = self.invocation.clone();
        let session = self.session.clone();
        let Ok(request_id) = self.next_request_id() else {
            return;
        };
        runtime.spawn(async move {
            let _ = invocation
                .invoke(crate::daemon_contract::DaemonInvocationRequest::lsp_detach(
                    request_id, session,
                ))
                .await;
        });
    }
}

fn invocation_outcome_error(
    outcome: crate::daemon_contract::DaemonInvocationOutcome,
) -> crate::errors::TraceDecayError {
    let message = match outcome {
        crate::daemon_contract::DaemonInvocationOutcome::Problem { problem } => match problem {
            crate::daemon_contract::DaemonInvocationProblem::InvalidRequest => {
                "daemon rejected the invocation input"
            }
            crate::daemon_contract::DaemonInvocationProblem::UnsupportedRevision => {
                "daemon does not support this invocation revision"
            }
            crate::daemon_contract::DaemonInvocationProblem::NotFoundOrNotAuthorized => {
                "daemon invocation was not found or is not authorized"
            }
            crate::daemon_contract::DaemonInvocationProblem::Unavailable => {
                "daemon invocation authority is unavailable"
            }
        },
        _ => "daemon returned an unexpected invocation response",
    };
    crate::errors::TraceDecayError::Config {
        message: message.to_owned(),
    }
}
