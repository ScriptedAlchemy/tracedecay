//! Closed, authenticated daemon invocation protocol.
//!
//! This module deliberately accepts a small typed operation set after the
//! daemon handshake. It is not a generic application invoke endpoint and it
//! never accepts a raw Git request, database selector, or LSP socket address.
//! LSP frames are handled by a daemon-owned protocol actor; the bridge only
//! receives the actor's bounded responses through explicit frame operations.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::daemon::lsp_gateway::{
    AdmittedRoot, AuthorizedLspSession, ClientCapabilities, DaemonLspGateway,
    DaemonLspProtocolSession, DaemonLspSessionEndpoint, FeedbackCyclePort,
    FeedbackCycleRequest, FeedbackCycleResponse, GatewayCapabilities, LSP_SESSION_TTL_MS,
    LspEndpointError, LspSessionAccess, LspSessionAdmissionPort, LspSessionCredential,
    LspSessionId, LspSessionOpenRequest, LspSessionRegistry, SessionLifecycle,
    UnavailableSemanticProvider, UpstreamCapabilities, negotiate_capabilities,
};
use crate::lsp_bridge::MAX_LSP_FRAME_BYTES;

/// Stable discriminator for the closed post-handshake invocation protocol.
pub(crate) const DAEMON_INVOCATION_PROTOCOL: &str = "tracedecay.daemon.invocation";
/// Initial revision of the daemon-owned invocation wire shape.
pub(crate) const DAEMON_INVOCATION_REVISION: u16 = 1;

const MAX_INVOCATION_REQUEST_ID_BYTES: usize = 128;
const MAX_CLIENT_REVISION_BYTES: usize = 128;
const MAX_ROOT_HINT_BYTES: usize = 4_096;
const MAX_OPAQUE_HANDLE_BYTES: usize = 256;

/// Closed operations accepted by the daemon invocation connection.
///
/// Git and feedback operations deliberately carry only daemon-minted opaque
/// handles. Their application requests stay inside their owning daemon
/// services and cannot be reconstructed by a client.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DaemonInvocationOperation {
    GitPreview,
    GitApply,
    FeedbackDiagnostics,
    FeedbackGet,
    FeedbackExpand,
    FeedbackList,
    LspOpen,
    LspFrame,
    LspPoll,
    LspAcknowledge,
    LspDetach,
}

impl DaemonInvocationOperation {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::GitPreview => "git_preview",
            Self::GitApply => "git_apply",
            Self::FeedbackDiagnostics => "feedback_diagnostics",
            Self::FeedbackGet => "feedback_get",
            Self::FeedbackExpand => "feedback_expand",
            Self::FeedbackList => "feedback_list",
            Self::LspOpen => "lsp_open",
            Self::LspFrame => "lsp_frame",
            Self::LspPoll => "lsp_poll",
            Self::LspAcknowledge => "lsp_acknowledge",
            Self::LspDetach => "lsp_detach",
        }
    }
}

/// Credential-bearing access data exchanged only between a bridge and the
/// authenticated daemon. Its debug representation never prints the secret.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct DaemonLspSessionAccess {
    pub(crate) session_id: String,
    credential: String,
}

impl fmt::Debug for DaemonLspSessionAccess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DaemonLspSessionAccess")
            .field("session_id", &self.session_id)
            .field("credential", &"[redacted]")
            .finish()
    }
}

impl DaemonLspSessionAccess {
    fn from_access(access: &LspSessionAccess) -> Self {
        Self {
            session_id: access.session_id().as_str().to_owned(),
            credential: hex::encode(access.credential().as_bytes()),
        }
    }

    fn into_access(self) -> Result<LspSessionAccess, DaemonInvocationProblem> {
        let session_id =
            LspSessionId::new(self.session_id).map_err(|_| DaemonInvocationProblem::InvalidRequest)?;
        let credential = hex::decode(self.credential)
            .ok()
            .and_then(|credential| LspSessionCredential::new(credential).ok())
            .ok_or(DaemonInvocationProblem::InvalidRequest)?;
        Ok(LspSessionAccess::new(session_id, credential))
    }
}

/// One versioned, request-correlated daemon operation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct DaemonInvocationRequest {
    pub(crate) protocol: String,
    pub(crate) revision: u16,
    pub(crate) request_id: String,
    #[serde(flatten)]
    pub(crate) payload: DaemonInvocationPayload,
}

/// Operation-specific fields for the closed invocation set.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub(crate) enum DaemonInvocationPayload {
    GitPreview {
        request_handle: String,
    },
    GitApply {
        request_handle: String,
    },
    FeedbackDiagnostics {
        request_handle: String,
    },
    FeedbackGet {
        request_handle: String,
    },
    FeedbackExpand {
        request_handle: String,
    },
    FeedbackList {
        request_handle: String,
    },
    LspOpen {
        client_revision: String,
        requested_root_uri: Option<String>,
        workspace_folders: Vec<String>,
    },
    LspFrame {
        session: DaemonLspSessionAccess,
        frame: String,
    },
    LspPoll {
        session: DaemonLspSessionAccess,
    },
    LspAcknowledge {
        session: DaemonLspSessionAccess,
    },
    LspDetach {
        session: DaemonLspSessionAccess,
    },
}

impl DaemonInvocationRequest {
    pub(crate) fn lsp_open(
        request_id: impl Into<String>,
        client_revision: impl Into<String>,
        requested_root_uri: Option<String>,
        workspace_folders: Vec<String>,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            payload: DaemonInvocationPayload::LspOpen {
                client_revision: client_revision.into(),
                requested_root_uri,
                workspace_folders,
            },
        }
    }

    pub(crate) fn lsp_frame(
        request_id: impl Into<String>,
        session: DaemonLspSessionAccess,
        frame: impl Into<String>,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            payload: DaemonInvocationPayload::LspFrame {
                session,
                frame: frame.into(),
            },
        }
    }

    pub(crate) fn lsp_poll(
        request_id: impl Into<String>,
        session: DaemonLspSessionAccess,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            payload: DaemonInvocationPayload::LspPoll { session },
        }
    }

    pub(crate) fn lsp_acknowledge(
        request_id: impl Into<String>,
        session: DaemonLspSessionAccess,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            payload: DaemonInvocationPayload::LspAcknowledge { session },
        }
    }

    pub(crate) fn lsp_detach(
        request_id: impl Into<String>,
        session: DaemonLspSessionAccess,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            payload: DaemonInvocationPayload::LspDetach { session },
        }
    }

    pub(crate) fn operation(&self) -> DaemonInvocationOperation {
        match self.payload {
            DaemonInvocationPayload::GitPreview { .. } => DaemonInvocationOperation::GitPreview,
            DaemonInvocationPayload::GitApply { .. } => DaemonInvocationOperation::GitApply,
            DaemonInvocationPayload::FeedbackDiagnostics { .. } => {
                DaemonInvocationOperation::FeedbackDiagnostics
            }
            DaemonInvocationPayload::FeedbackGet { .. } => DaemonInvocationOperation::FeedbackGet,
            DaemonInvocationPayload::FeedbackExpand { .. } => {
                DaemonInvocationOperation::FeedbackExpand
            }
            DaemonInvocationPayload::FeedbackList { .. } => {
                DaemonInvocationOperation::FeedbackList
            }
            DaemonInvocationPayload::LspOpen { .. } => DaemonInvocationOperation::LspOpen,
            DaemonInvocationPayload::LspFrame { .. } => DaemonInvocationOperation::LspFrame,
            DaemonInvocationPayload::LspPoll { .. } => DaemonInvocationOperation::LspPoll,
            DaemonInvocationPayload::LspAcknowledge { .. } => {
                DaemonInvocationOperation::LspAcknowledge
            }
            DaemonInvocationPayload::LspDetach { .. } => DaemonInvocationOperation::LspDetach,
        }
    }

    pub(crate) fn requires_project(&self) -> bool {
        matches!(
            self.operation(),
            DaemonInvocationOperation::GitPreview
                | DaemonInvocationOperation::GitApply
                | DaemonInvocationOperation::FeedbackDiagnostics
                | DaemonInvocationOperation::FeedbackGet
                | DaemonInvocationOperation::FeedbackExpand
                | DaemonInvocationOperation::FeedbackList
                | DaemonInvocationOperation::LspOpen
        )
    }

    fn validate(&self) -> Result<(), DaemonInvocationProblem> {
        if self.protocol != DAEMON_INVOCATION_PROTOCOL {
            return Err(DaemonInvocationProblem::InvalidRequest);
        }
        if self.revision != DAEMON_INVOCATION_REVISION {
            return Err(DaemonInvocationProblem::UnsupportedRevision);
        }
        if !valid_token(&self.request_id, MAX_INVOCATION_REQUEST_ID_BYTES) {
            return Err(DaemonInvocationProblem::InvalidRequest);
        }
        match &self.payload {
            DaemonInvocationPayload::GitPreview { request_handle }
            | DaemonInvocationPayload::GitApply { request_handle }
            | DaemonInvocationPayload::FeedbackDiagnostics { request_handle }
            | DaemonInvocationPayload::FeedbackGet { request_handle }
            | DaemonInvocationPayload::FeedbackExpand { request_handle }
            | DaemonInvocationPayload::FeedbackList { request_handle } => {
                if !valid_token(request_handle, MAX_OPAQUE_HANDLE_BYTES) {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::LspOpen {
                client_revision,
                requested_root_uri,
                workspace_folders,
            } => {
                if !valid_printable(client_revision, MAX_CLIENT_REVISION_BYTES)
                    || requested_root_uri
                        .as_deref()
                        .is_some_and(|uri| !valid_printable(uri, MAX_ROOT_HINT_BYTES))
                    || workspace_folders.len() > 1
                    || workspace_folders
                        .iter()
                        .any(|folder| !valid_printable(folder, MAX_ROOT_HINT_BYTES))
                {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::LspFrame { session, frame } => {
                let _ = session.clone().into_access()?;
                if frame.len() > MAX_LSP_FRAME_BYTES {
                    return Err(DaemonInvocationProblem::InvalidRequest);
                }
            }
            DaemonInvocationPayload::LspPoll { session }
            | DaemonInvocationPayload::LspAcknowledge { session }
            | DaemonInvocationPayload::LspDetach { session } => {
                let _ = session.clone().into_access()?;
            }
        }
        Ok(())
    }
}

/// Parse an invocation only when it explicitly selects this protocol. Ordinary
/// MCP JSON-RPC frames continue through the established daemon route.
pub(crate) fn parse_daemon_invocation_request(
    line: &str,
) -> Option<Result<DaemonInvocationRequest, DaemonInvocationResponse>> {
    let value = serde_json::from_str::<serde_json::Value>(line.trim()).ok()?;
    if value.get("protocol").and_then(serde_json::Value::as_str)
        != Some(DAEMON_INVOCATION_PROTOCOL)
    {
        return None;
    }
    let request_id = value
        .get("request_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Some(
        serde_json::from_value(value).map_err(|_| {
            DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::InvalidRequest)
        }),
    )
}

/// A safe, deliberately non-diagnostic daemon invocation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DaemonInvocationProblem {
    InvalidRequest,
    UnsupportedRevision,
    NotFoundOrNotAuthorized,
    Unavailable,
}

/// Response envelope paired with one invocation request id.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct DaemonInvocationResponse {
    pub(crate) protocol: String,
    pub(crate) revision: u16,
    pub(crate) request_id: String,
    #[serde(flatten)]
    pub(crate) outcome: DaemonInvocationOutcome,
}

/// Bounded operation outcomes. LSP payloads remain protocol frames, not an
/// unrestricted stream or arbitrary daemon-socket response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum DaemonInvocationOutcome {
    LspOpened {
        session: DaemonLspSessionAccess,
        expires_at_ms: u64,
    },
    LspFrameAccepted {
        backpressured: bool,
        closed: bool,
    },
    LspFrame {
        frame: Option<String>,
        closed: bool,
    },
    LspAcknowledged {
        acknowledged: bool,
    },
    LspDetached,
    Problem {
        problem: DaemonInvocationProblem,
    },
}

impl DaemonInvocationResponse {
    pub(crate) fn problem(
        request_id: impl Into<String>,
        problem: DaemonInvocationProblem,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id: request_id.into(),
            outcome: DaemonInvocationOutcome::Problem { problem },
        }
    }

    fn lsp_opened(
        request_id: String,
        session: DaemonLspSessionAccess,
        expires_at_ms: u64,
    ) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id,
            outcome: DaemonInvocationOutcome::LspOpened {
                session,
                expires_at_ms,
            },
        }
    }

    fn with_outcome(request_id: String, outcome: DaemonInvocationOutcome) -> Self {
        Self {
            protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
            revision: DAEMON_INVOCATION_REVISION,
            request_id,
            outcome,
        }
    }
}

/// Retained daemon state for the typed LSP invocation operations.
#[derive(Clone, Default)]
pub(crate) struct DaemonInvocationService {
    lsp_sessions: Arc<Mutex<BTreeMap<LspSessionId, RuntimeLspSession>>>,
}

struct RuntimeLspSession {
    expires_at_ms: u64,
    actor: RuntimeLspActor,
}

type RuntimeLspActor =
    DaemonLspProtocolSession<UnavailableFeedbackCycle, UnavailableSemanticProvider>;

/// The LSP endpoint must not fabricate feedback findings when the feedback
/// application owner is not mounted. It records a truthful deferred outcome.
#[derive(Clone, Copy, Debug, Default)]
struct UnavailableFeedbackCycle;

impl FeedbackCyclePort for UnavailableFeedbackCycle {
    fn request_feedback_cycle(&self, _request: FeedbackCycleRequest) -> FeedbackCycleResponse {
        FeedbackCycleResponse::Deferred {
            reason: "feedback cycle authority is unavailable".to_owned(),
        }
    }
}

/// Admission binds a session to the root independently resolved by the daemon
/// before this protocol is invoked. Client root hints are never consulted.
#[derive(Clone, Debug)]
struct AdmittedRootSessionAdmission {
    root: AdmittedRoot,
}

impl LspSessionAdmissionPort for AdmittedRootSessionAdmission {
    fn admit_lsp_session(
        &self,
        _request: &LspSessionOpenRequest,
        now_ms: u64,
    ) -> Result<AuthorizedLspSession, LspEndpointError> {
        let mut session_bytes = [0_u8; 16];
        let mut credential_bytes = [0_u8; 32];
        getrandom::getrandom(&mut session_bytes).map_err(|_| LspEndpointError::AdmissionRejected)?;
        getrandom::getrandom(&mut credential_bytes)
            .map_err(|_| LspEndpointError::AdmissionRejected)?;
        let session_id = LspSessionId::new(format!("lsp-{}", hex::encode(session_bytes)))?;
        let credential = LspSessionCredential::new(credential_bytes.to_vec())?;
        Ok(AuthorizedLspSession {
            session_id,
            credential,
            root: self.root.clone(),
            expires_at_ms: now_ms.saturating_add(LSP_SESSION_TTL_MS),
        })
    }
}

impl DaemonInvocationService {
    /// Executes a closed request after daemon socket authentication. `root` is
    /// supplied only after the daemon has opened and authorized the project;
    /// existing LSP session operations do not re-resolve client paths.
    pub(crate) async fn invoke(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        root: Option<AdmittedRoot>,
        request: DaemonInvocationRequest,
    ) -> DaemonInvocationResponse {
        let request_id = request.request_id.clone();
        if let Err(problem) = request.validate() {
            return DaemonInvocationResponse::problem(request_id, problem);
        }
        let now_ms = now_millis();
        self.expire_sessions(now_ms).await;

        match request.payload {
            DaemonInvocationPayload::LspOpen {
                client_revision,
                requested_root_uri,
                workspace_folders,
            } => {
                self.open_lsp_session(
                    lsp_registry,
                    root,
                    request_id,
                    client_revision,
                    requested_root_uri,
                    workspace_folders,
                    now_ms,
                )
                .await
            }
            DaemonInvocationPayload::LspFrame { session, frame } => {
                self.send_lsp_frame(lsp_registry, request_id, session, frame, now_ms)
                    .await
            }
            DaemonInvocationPayload::LspPoll { session } => {
                self.poll_lsp_frame(lsp_registry, request_id, session, now_ms)
                    .await
            }
            DaemonInvocationPayload::LspAcknowledge { session } => {
                self.acknowledge_lsp_frame(lsp_registry, request_id, session, now_ms)
                    .await
            }
            DaemonInvocationPayload::LspDetach { session } => {
                self.detach_lsp_session(lsp_registry, request_id, session, now_ms)
                    .await
            }
            // These surface names intentionally do not deserialize a Git
            // request or feedback finding identifier. Their authoritative
            // owners have not yet minted a handle for this connection, so a
            // truthful unavailable outcome is safer than a local fallback.
            DaemonInvocationPayload::GitPreview { .. }
            | DaemonInvocationPayload::GitApply { .. }
            | DaemonInvocationPayload::FeedbackDiagnostics { .. }
            | DaemonInvocationPayload::FeedbackGet { .. }
            | DaemonInvocationPayload::FeedbackExpand { .. }
            | DaemonInvocationPayload::FeedbackList { .. } => {
                DaemonInvocationResponse::problem(request_id, DaemonInvocationProblem::Unavailable)
            }
        }
    }

    pub(crate) async fn expire_all(&self) {
        self.lsp_sessions.lock().await.clear();
    }

    async fn open_lsp_session(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        root: Option<AdmittedRoot>,
        request_id: String,
        client_revision: String,
        requested_root_uri: Option<String>,
        workspace_folders: Vec<String>,
        now_ms: u64,
    ) -> DaemonInvocationResponse {
        let Some(root) = root else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        let request = LspSessionOpenRequest {
            requested_root_uri,
            workspace_folders,
            client_revision,
        };
        let access = {
            let mut registry = lsp_registry.lock().await;
            let existing = std::mem::take(&mut *registry);
            let mut endpoint = DaemonLspSessionEndpoint::with_registry(
                AdmittedRootSessionAdmission { root: root.clone() },
                existing,
            );
            let result = endpoint.open(request, now_ms);
            *registry = endpoint.into_registry();
            result
        };
        let access = match access {
            Ok(access) => access,
            Err(_) => {
                return DaemonInvocationResponse::problem(
                    request_id,
                    DaemonInvocationProblem::NotFoundOrNotAuthorized,
                );
            }
        };
        let expires_at_ms = now_ms.saturating_add(LSP_SESSION_TTL_MS);
        let session_id = access.session_id().clone();
        let actor = runtime_lsp_actor(root);
        self.lsp_sessions.lock().await.insert(
            session_id,
            RuntimeLspSession {
                expires_at_ms,
                actor,
            },
        );
        DaemonInvocationResponse::lsp_opened(
            request_id,
            DaemonLspSessionAccess::from_access(&access),
            expires_at_ms,
        )
    }

    async fn send_lsp_frame(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        request_id: String,
        session: DaemonLspSessionAccess,
        frame: String,
        now_ms: u64,
    ) -> DaemonInvocationResponse {
        let access = match self.authenticate(lsp_registry, session, now_ms).await {
            Ok(access) => access,
            Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
        };
        let mut sessions = self.lsp_sessions.lock().await;
        let Some(session) = sessions.get_mut(access.session_id()) else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        let dispatch = session.actor.handle_payload(frame.as_bytes(), now_ms);
        DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::LspFrameAccepted {
                backpressured: dispatch.backpressured,
                closed: dispatch.closed,
            },
        )
    }

    async fn poll_lsp_frame(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        request_id: String,
        session: DaemonLspSessionAccess,
        now_ms: u64,
    ) -> DaemonInvocationResponse {
        let access = match self.authenticate(lsp_registry, session, now_ms).await {
            Ok(access) => access,
            Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
        };
        let mut sessions = self.lsp_sessions.lock().await;
        let Some(session) = sessions.get_mut(access.session_id()) else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        let dispatch = session.actor.flush_due(now_ms);
        let frame = session
            .actor
            .poll_outbound()
            .and_then(|frame| std::str::from_utf8(frame).ok())
            .map(str::to_owned);
        let closed = dispatch.closed
            || matches!(
                session.actor.lifecycle(),
                SessionLifecycle::Exited | SessionLifecycle::Expired
            );
        DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::LspFrame { frame, closed },
        )
    }

    async fn acknowledge_lsp_frame(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        request_id: String,
        session: DaemonLspSessionAccess,
        now_ms: u64,
    ) -> DaemonInvocationResponse {
        let access = match self.authenticate(lsp_registry, session, now_ms).await {
            Ok(access) => access,
            Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
        };
        let mut sessions = self.lsp_sessions.lock().await;
        let Some(session) = sessions.get_mut(access.session_id()) else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        DaemonInvocationResponse::with_outcome(
            request_id,
            DaemonInvocationOutcome::LspAcknowledged {
                acknowledged: session.actor.acknowledge_outbound(),
            },
        )
    }

    async fn detach_lsp_session(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        request_id: String,
        session: DaemonLspSessionAccess,
        now_ms: u64,
    ) -> DaemonInvocationResponse {
        let access = match self.authenticate(lsp_registry, session, now_ms).await {
            Ok(access) => access,
            Err(problem) => return DaemonInvocationResponse::problem(request_id, problem),
        };
        let endpoint_detached = {
            let mut registry = lsp_registry.lock().await;
            registry.detach(&access, now_ms).is_ok()
        };
        let mut sessions = self.lsp_sessions.lock().await;
        let Some(session) = sessions.get_mut(access.session_id()) else {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        };
        if !endpoint_detached || session.actor.detach().is_err() {
            return DaemonInvocationResponse::problem(
                request_id,
                DaemonInvocationProblem::NotFoundOrNotAuthorized,
            );
        }
        DaemonInvocationResponse::with_outcome(request_id, DaemonInvocationOutcome::LspDetached)
    }

    async fn authenticate(
        &self,
        lsp_registry: &Arc<Mutex<LspSessionRegistry>>,
        session: DaemonLspSessionAccess,
        now_ms: u64,
    ) -> Result<LspSessionAccess, DaemonInvocationProblem> {
        let access = session.into_access()?;
        let authenticated = {
            let mut registry = lsp_registry.lock().await;
            registry.authenticate(&access, now_ms).is_ok()
        };
        if authenticated {
            Ok(access)
        } else {
            self.lsp_sessions.lock().await.remove(access.session_id());
            Err(DaemonInvocationProblem::NotFoundOrNotAuthorized)
        }
    }

    async fn expire_sessions(&self, now_ms: u64) {
        self.lsp_sessions
            .lock()
            .await
            .retain(|_, session| session.expires_at_ms > now_ms);
    }
}

fn runtime_lsp_actor(root: AdmittedRoot) -> RuntimeLspActor {
    let gateway_capabilities = GatewayCapabilities::default();
    let upstream_capabilities = UpstreamCapabilities::default();
    let effective = negotiate_capabilities(
        &ClientCapabilities::default(),
        &gateway_capabilities,
        &upstream_capabilities,
    );
    DaemonLspProtocolSession::without_diagnostic_provider(
        DaemonLspGateway::new(root, effective, UnavailableFeedbackCycle),
        gateway_capabilities,
        upstream_capabilities,
    )
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

fn valid_token(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn valid_printable(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() || byte == b' ')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_protocol_frames_select_the_invocation_route() {
        assert!(parse_daemon_invocation_request(r#"{"jsonrpc":"2.0","method":"ping"}"#)
            .is_none());
        let request = DaemonInvocationRequest::lsp_open(
            "request.1",
            "client.1",
            Some("file:///untrusted".to_owned()),
            Vec::new(),
        );
        let encoded = serde_json::to_string(&request).expect("encode request");
        assert!(matches!(
            parse_daemon_invocation_request(&encoded),
            Some(Ok(_))
        ));
    }

    #[tokio::test]
    async fn lsp_session_uses_the_admitted_root_not_the_client_hint() {
        let service = DaemonInvocationService::default();
        let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
        let response = service
            .invoke(
                &registry,
                Some(AdmittedRoot::new("file:///authoritative")),
                DaemonInvocationRequest::lsp_open(
                    "request.1",
                    "client.1",
                    Some("file:///untrusted".to_owned()),
                    Vec::new(),
                ),
            )
            .await;
        let DaemonInvocationOutcome::LspOpened { session, .. } = response.outcome else {
            panic!("expected an admitted LSP session");
        };

        let initialize = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":"file:///untrusted","capabilities":{}}}"#;
        let response = service
            .invoke(
                &registry,
                None,
                DaemonInvocationRequest::lsp_frame("request.2", session.clone(), initialize),
            )
            .await;
        assert!(matches!(
            response.outcome,
            DaemonInvocationOutcome::LspFrameAccepted { .. }
        ));

        let response = service
            .invoke(
                &registry,
                None,
                DaemonInvocationRequest::lsp_poll("request.3", session),
            )
            .await;
        let DaemonInvocationOutcome::LspFrame {
            frame: Some(frame), ..
        } = response.outcome
        else {
            panic!("expected initialize response");
        };
        assert!(frame.contains("rootUri"));
        assert!(frame.contains("authoritative"));
    }

    #[tokio::test]
    async fn git_and_feedback_handles_fail_closed_without_an_owner() {
        let service = DaemonInvocationService::default();
        let registry = Arc::new(Mutex::new(LspSessionRegistry::default()));
        for payload in [
            DaemonInvocationPayload::GitPreview {
                request_handle: "handle.1".to_owned(),
            },
            DaemonInvocationPayload::FeedbackList {
                request_handle: "handle.2".to_owned(),
            },
        ] {
            let response = service
                .invoke(
                    &registry,
                    Some(AdmittedRoot::new("file:///authoritative")),
                    DaemonInvocationRequest {
                        protocol: DAEMON_INVOCATION_PROTOCOL.to_owned(),
                        revision: DAEMON_INVOCATION_REVISION,
                        request_id: "request.1".to_owned(),
                        payload,
                    },
                )
                .await;
            assert_eq!(
                response.outcome,
                DaemonInvocationOutcome::Problem {
                    problem: DaemonInvocationProblem::Unavailable,
                }
            );
        }
    }
}
