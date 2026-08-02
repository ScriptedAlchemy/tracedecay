//! Daemon-owned LSP session lifecycle, request admission, and publication
//! ordering. The stdio bridge has no copy of this state.

use std::collections::BTreeMap;
use std::fmt;

use tracedecay_domain::{ContentDigest, ManifestDigest};

use crate::gateway::AdmittedRoot;

pub const MAX_PENDING_REQUESTS: usize = 64;
/// A single `publishDiagnostics` JSON-RPC publication is bounded separately
/// from the four-MiB transport frame limit so noisy documents cannot starve
/// unrelated interactive requests.
pub const MAX_PUBLICATION_BYTES: usize = 256 * 1024;
/// Maximum number of live bridge sessions in one daemon process.
pub const MAX_LSP_SESSIONS: usize = 64;
/// Maximum roots admitted into one exact workspace-folder set.
/// A client may only admit the bounded root set authorized for this session.
/// Keeping this small also bounds federated provider fan-out before any graph
/// or analyzer operation is started.
pub const MAX_LSP_WORKSPACE_ROOTS: usize = 8;
/// Detached session state is deterministically discarded after this TTL.
pub const LSP_SESSION_TTL_MS: u64 = 15 * 60 * 1_000;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LspRequestId {
    Number(i64),
    String(String),
}

/// Opaque daemon-assigned LSP session identity.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LspSessionId(String);

impl LspSessionId {
    pub fn new(value: impl Into<String>) -> Result<Self, LspEndpointError> {
        let value = value.into();
        if value.is_empty() || value.len() > 128 {
            return Err(LspEndpointError::InvalidSessionId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Opaque credential minted by the daemon admission authority.
#[derive(Clone, Eq, PartialEq)]
pub struct LspSessionCredential(Vec<u8>);

impl LspSessionCredential {
    pub fn new(value: impl Into<Vec<u8>>) -> Result<Self, LspEndpointError> {
        let value = value.into();
        if value.len() < 16 || value.len() > 256 {
            return Err(LspEndpointError::InvalidCredential);
        }
        Ok(Self(value))
    }

    /// Returns credential material only to an authenticated daemon wire
    /// adapter. Presentation code must never log or render this value.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for LspSessionCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LspSessionCredential([redacted])")
    }
}

/// Credential-bearing bridge access to one daemon LSP session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspSessionAccess {
    session_id: LspSessionId,
    credential: LspSessionCredential,
}

impl LspSessionAccess {
    pub fn new(session_id: LspSessionId, credential: LspSessionCredential) -> Self {
        Self {
            session_id,
            credential,
        }
    }

    pub fn session_id(&self) -> &LspSessionId {
        &self.session_id
    }

    /// Returns the opaque credential only to the authenticated daemon
    /// invocation service.
    pub fn credential(&self) -> &LspSessionCredential {
        &self.credential
    }
}

/// Request to begin one typed daemon session. Requested roots are
/// presentation hints; the admission authority resolves the canonical root.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LspSessionOpenRequest {
    pub requested_root_uri: Option<String>,
    pub workspace_folders: Vec<String>,
    pub client_revision: String,
}

/// Exact workspace-folder authority returned by daemon admission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizedLspWorkspace {
    scope_set_digest: Option<ManifestDigest>,
    roots: Vec<AdmittedRoot>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LspWorkspaceRouteError {
    OutsideAdmittedRoots,
    AmbiguousAdmittedRoots,
}

impl AuthorizedLspWorkspace {
    pub fn single(root: AdmittedRoot) -> Self {
        Self {
            scope_set_digest: None,
            roots: vec![root],
        }
    }

    pub fn new(
        scope_set_digest: Option<ManifestDigest>,
        mut roots: Vec<AdmittedRoot>,
    ) -> Result<Self, LspEndpointError> {
        if roots.is_empty() || roots.len() > MAX_LSP_WORKSPACE_ROOTS {
            return Err(LspEndpointError::AdmissionRejected);
        }
        if roots.len() > 1
            && (scope_set_digest.is_none()
                || roots.iter().any(|root| root.scope_digest().is_none()))
        {
            return Err(LspEndpointError::AdmissionRejected);
        }
        if roots.iter().any(|root| !root.is_valid()) {
            return Err(LspEndpointError::AdmissionRejected);
        }
        roots.sort_by(|left, right| {
            left.scope_digest()
                .cmp(&right.scope_digest())
                .then_with(|| left.uri().cmp(right.uri()))
        });
        for (index, root) in roots.iter().enumerate() {
            if roots[..index].iter().any(|candidate| {
                candidate.scope_digest().is_some()
                    && candidate.scope_digest() == root.scope_digest()
                    || candidate.matches_root_uri(root.uri())
            }) {
                return Err(LspEndpointError::AdmissionRejected);
            }
        }
        Ok(Self {
            scope_set_digest,
            roots,
        })
    }

    pub fn roots(&self) -> &[AdmittedRoot] {
        &self.roots
    }

    pub fn scope_set_digest(&self) -> Option<&ManifestDigest> {
        self.scope_set_digest.as_ref()
    }

    pub fn root_ordinal(&self, root: &AdmittedRoot) -> Option<usize> {
        self.roots.iter().position(|candidate| candidate == root)
    }

    pub fn resolve_document(
        &self,
        document_uri: &str,
    ) -> Result<&AdmittedRoot, LspWorkspaceRouteError> {
        let mut matches = self
            .roots
            .iter()
            .filter_map(|root| {
                root.document_root_depth(document_uri)
                    .map(|depth| (depth, root))
            })
            .collect::<Vec<_>>();
        matches.sort_by_key(|(depth, _)| std::cmp::Reverse(*depth));
        let Some((deepest, root)) = matches.first().copied() else {
            return Err(LspWorkspaceRouteError::OutsideAdmittedRoots);
        };
        if matches
            .get(1)
            .is_some_and(|(candidate_depth, _)| *candidate_depth == deepest)
        {
            return Err(LspWorkspaceRouteError::AmbiguousAdmittedRoots);
        }
        Ok(root)
    }

    pub fn resolve_root_uri(
        &self,
        root_uri: &str,
    ) -> Result<&AdmittedRoot, LspWorkspaceRouteError> {
        let mut matches = self
            .roots
            .iter()
            .filter(|root| root.matches_root_uri(root_uri));
        let Some(root) = matches.next() else {
            return Err(LspWorkspaceRouteError::OutsideAdmittedRoots);
        };
        if matches.next().is_some() {
            return Err(LspWorkspaceRouteError::AmbiguousAdmittedRoots);
        }
        Ok(root)
    }

    pub fn admits_exact_root_hints(&self, requested: &[String]) -> bool {
        self.matches_multi_root_hints(requested)
    }

    pub fn is_single_root(&self) -> bool {
        self.roots.len() == 1
    }

    pub(crate) fn primary(&self) -> &AdmittedRoot {
        &self.roots[0]
    }

    fn matches_multi_root_hints(&self, requested: &[String]) -> bool {
        requested.len() == self.roots.len()
            && requested.iter().enumerate().all(|(index, uri)| {
                !requested[..index].iter().any(|prior| {
                    self.roots
                        .iter()
                        .any(|root| root.matches_root_uri(prior) && root.matches_root_uri(uri))
                }) && self
                    .roots
                    .iter()
                    .filter(|root| root.matches_root_uri(uri))
                    .count()
                    == 1
            })
    }
}

/// Session result authorized by the daemon admission boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizedLspSession {
    pub session_id: LspSessionId,
    pub credential: LspSessionCredential,
    pub workspace: AuthorizedLspWorkspace,
    pub expires_at_ms: u64,
}

/// Admission resolves one canonical project root before document content is
/// accepted.
pub trait LspSessionAdmissionPort {
    fn admit_lsp_session(
        &self,
        request: &LspSessionOpenRequest,
        now_ms: u64,
    ) -> Result<AuthorizedLspSession, LspEndpointError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LspEndpointError {
    InvalidSessionId,
    InvalidCredential,
    MultipleRootsUnsupported,
    AdmissionRejected,
    DuplicateSession,
    Saturated,
    AuthenticationFailed,
    SessionExpired,
    SessionUnavailable,
    Lifecycle(LifecycleError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionLifecycle {
    AwaitingInitialize,
    AwaitingInitialized,
    Ready,
    Detached,
    Shutdown,
    Exited,
    Expired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleError {
    InvalidTransition {
        from: SessionLifecycle,
        operation: &'static str,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingState {
    Active,
    Cancelled,
    ContentModified,
    TimedOut,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingRequest {
    document: Option<(String, i64)>,
    state: PendingState,
    deadline_at_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestAdmission {
    Accepted,
    DuplicateId,
    SessionUnavailable,
    Saturated { retrigger_request: bool },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancellationOutcome {
    Accepted,
    AlreadyCancelled,
    UnknownRequest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionDisposition {
    Publish,
    SuppressCancelled,
    SuppressContentModified,
    SuppressTimedOut,
    UnknownRequest,
}

impl CompletionDisposition {
    /// Maps a suppressed request completion to the standard JSON-RPC/LSP
    /// error that a protocol adapter must return instead of publishing a stale
    /// result.
    pub const fn failure(self) -> Option<LspRequestFailure> {
        match self {
            Self::Publish | Self::UnknownRequest => None,
            Self::SuppressCancelled => Some(LspRequestFailure::RequestCancelled),
            Self::SuppressContentModified => Some(LspRequestFailure::ContentModified),
            Self::SuppressTimedOut => Some(LspRequestFailure::ServerCancelled {
                retrigger_request: true,
            }),
        }
    }
}

/// Standard LSP request failure codes used by the protocol adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LspRequestFailure {
    RequestCancelled,
    ContentModified,
    ServerCancelled { retrigger_request: bool },
}

impl LspRequestFailure {
    pub const fn code(self) -> i64 {
        match self {
            Self::RequestCancelled => -32800,
            Self::ContentModified => -32801,
            Self::ServerCancelled { .. } => -32802,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationDelivery {
    Produced,
    Queued,
    BridgeAcknowledged,
    Superseded,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationState {
    pub document_version: i64,
    pub generation: u64,
    pub payload_bytes: usize,
    pub delivery: PublicationDelivery,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationAdmission {
    Accepted,
    Duplicate,
    Stale,
    TooLarge { size: usize, limit: usize },
    SessionUnavailable,
}

/// Mutable control state owned and serialized by one daemon session actor.
#[derive(Debug)]
pub struct LspSessionControl {
    lifecycle: SessionLifecycle,
    detached_from: Option<SessionLifecycle>,
    pending: BTreeMap<LspRequestId, PendingRequest>,
    publications: BTreeMap<String, PublicationState>,
    publication_payload_digests: BTreeMap<String, ContentDigest>,
    max_pending_requests: usize,
}

impl Default for LspSessionControl {
    fn default() -> Self {
        Self::new(MAX_PENDING_REQUESTS)
    }
}

impl LspSessionControl {
    pub fn new(max_pending_requests: usize) -> Self {
        Self {
            lifecycle: SessionLifecycle::AwaitingInitialize,
            detached_from: None,
            pending: BTreeMap::new(),
            publications: BTreeMap::new(),
            publication_payload_digests: BTreeMap::new(),
            max_pending_requests,
        }
    }

    pub fn lifecycle(&self) -> SessionLifecycle {
        self.lifecycle
    }

    pub fn begin_initialize(&mut self) -> Result<(), LifecycleError> {
        self.transition(
            SessionLifecycle::AwaitingInitialize,
            SessionLifecycle::AwaitingInitialized,
            "initialize",
        )
    }

    pub fn initialized(&mut self) -> Result<(), LifecycleError> {
        self.transition(
            SessionLifecycle::AwaitingInitialized,
            SessionLifecycle::Ready,
            "initialized",
        )
    }

    pub fn shutdown(&mut self) -> Result<(), LifecycleError> {
        self.transition(
            SessionLifecycle::Ready,
            SessionLifecycle::Shutdown,
            "shutdown",
        )
    }

    pub fn exit(&mut self) -> Result<(), LifecycleError> {
        self.transition(SessionLifecycle::Shutdown, SessionLifecycle::Exited, "exit")?;
        self.pending.clear();
        self.publications.clear();
        self.publication_payload_digests.clear();
        Ok(())
    }

    pub fn detach(&mut self) -> Result<(), LifecycleError> {
        if !matches!(
            self.lifecycle,
            SessionLifecycle::AwaitingInitialize
                | SessionLifecycle::AwaitingInitialized
                | SessionLifecycle::Ready
                | SessionLifecycle::Shutdown
        ) {
            return Err(LifecycleError::InvalidTransition {
                from: self.lifecycle,
                operation: "detach",
            });
        }
        self.detached_from = Some(self.lifecycle);
        self.lifecycle = SessionLifecycle::Detached;
        Ok(())
    }

    pub fn reconnect(&mut self) -> Result<(), LifecycleError> {
        if self.lifecycle != SessionLifecycle::Detached {
            return Err(LifecycleError::InvalidTransition {
                from: self.lifecycle,
                operation: "reconnect",
            });
        }
        self.lifecycle = self.detached_from.take().unwrap_or(SessionLifecycle::Ready);
        for publication in self.publications.values_mut() {
            if publication.delivery == PublicationDelivery::BridgeAcknowledged {
                publication.delivery = PublicationDelivery::Produced;
            } else if publication.delivery == PublicationDelivery::Queued {
                publication.delivery = PublicationDelivery::Unknown;
            }
        }
        Ok(())
    }

    /// Deterministic TTL expiry releases all session-only overlays and work.
    pub fn expire(&mut self) {
        self.lifecycle = SessionLifecycle::Expired;
        self.detached_from = None;
        self.pending.clear();
        self.publications.clear();
        self.publication_payload_digests.clear();
    }

    pub fn admit_request(
        &mut self,
        id: LspRequestId,
        document: Option<(String, i64)>,
    ) -> RequestAdmission {
        self.admit_request_with_deadline(id, document, None)
    }

    /// Admits a request with a daemon-supplied monotonic deadline. The session
    /// owns cancellation and response suppression even when an upstream
    /// analyzer cannot stop a request immediately.
    pub fn admit_request_with_deadline(
        &mut self,
        id: LspRequestId,
        document: Option<(String, i64)>,
        deadline_at_ms: Option<u64>,
    ) -> RequestAdmission {
        if self.lifecycle != SessionLifecycle::Ready {
            return RequestAdmission::SessionUnavailable;
        }
        if self.pending.contains_key(&id) {
            return RequestAdmission::DuplicateId;
        }
        if self.pending.len() >= self.max_pending_requests {
            return RequestAdmission::Saturated {
                retrigger_request: true,
            };
        }
        self.pending.insert(
            id,
            PendingRequest {
                document,
                state: PendingState::Active,
                deadline_at_ms,
            },
        );
        RequestAdmission::Accepted
    }

    pub fn cancel_request(&mut self, id: &LspRequestId) -> CancellationOutcome {
        let Some(request) = self.pending.get_mut(id) else {
            return CancellationOutcome::UnknownRequest;
        };
        if request.state == PendingState::Cancelled {
            return CancellationOutcome::AlreadyCancelled;
        }
        request.state = PendingState::Cancelled;
        CancellationOutcome::Accepted
    }

    pub fn supersede_document(&mut self, document_uri: &str, version: i64) {
        for request in self.pending.values_mut() {
            if let Some((uri, request_version)) = &request.document
                && uri == document_uri
                && *request_version < version
                && request.state == PendingState::Active
            {
                request.state = PendingState::ContentModified;
            }
        }
    }

    /// Marks active requests whose deadline passed. It returns the request ids
    /// so the daemon's protocol actor can send a standard `ServerCancelled`
    /// error when that request has a response id. No wall-clock source lives
    /// in this state machine; callers pass their monotonic timestamp.
    pub fn expire_deadlines(&mut self, now_ms: u64) -> Vec<LspRequestId> {
        let mut expired = Vec::new();
        for (id, request) in &mut self.pending {
            if request.state == PendingState::Active
                && request
                    .deadline_at_ms
                    .is_some_and(|deadline| deadline <= now_ms)
            {
                request.state = PendingState::TimedOut;
                expired.push(id.clone());
            }
        }
        expired
    }

    pub fn complete_request(&mut self, id: &LspRequestId) -> CompletionDisposition {
        match self.pending.remove(id).map(|request| request.state) {
            Some(PendingState::Active) => CompletionDisposition::Publish,
            Some(PendingState::Cancelled) => CompletionDisposition::SuppressCancelled,
            Some(PendingState::ContentModified) => CompletionDisposition::SuppressContentModified,
            Some(PendingState::TimedOut) => CompletionDisposition::SuppressTimedOut,
            None => CompletionDisposition::UnknownRequest,
        }
    }

    pub fn admit_publication(
        &mut self,
        document_uri: impl Into<String>,
        document_version: i64,
        generation: u64,
    ) -> PublicationAdmission {
        self.admit_sized_publication(document_uri, document_version, generation, 0)
    }

    /// Records an outbound publication only if it fits the session publication
    /// budget. This is deliberately independent of the outer LSP framing
    /// limit: a valid four-MiB request must never imply a four-MiB diagnostic
    /// notification is permitted.
    pub fn admit_sized_publication(
        &mut self,
        document_uri: impl Into<String>,
        document_version: i64,
        generation: u64,
        payload_bytes: usize,
    ) -> PublicationAdmission {
        self.admit_publication_identity(
            document_uri.into(),
            document_version,
            generation,
            payload_bytes,
            None,
        )
    }

    pub(crate) fn admit_payload_publication(
        &mut self,
        document_uri: impl Into<String>,
        document_version: i64,
        generation: u64,
        payload: &[u8],
    ) -> PublicationAdmission {
        self.admit_publication_identity(
            document_uri.into(),
            document_version,
            generation,
            payload.len(),
            Some(ContentDigest::of_bytes(payload)),
        )
    }

    fn admit_publication_identity(
        &mut self,
        document_uri: String,
        document_version: i64,
        generation: u64,
        payload_bytes: usize,
        payload_digest: Option<ContentDigest>,
    ) -> PublicationAdmission {
        if self.lifecycle != SessionLifecycle::Ready {
            return PublicationAdmission::SessionUnavailable;
        }
        if payload_bytes > MAX_PUBLICATION_BYTES {
            return PublicationAdmission::TooLarge {
                size: payload_bytes,
                limit: MAX_PUBLICATION_BYTES,
            };
        }
        if let Some(current) = self.publications.get(&document_uri) {
            let key = (document_version, generation);
            let current_key = (current.document_version, current.generation);
            if key < current_key {
                return PublicationAdmission::Stale;
            }
            if key == current_key {
                let duplicate = payload_digest.as_ref().is_none_or(|digest| {
                    self.publication_payload_digests.get(&document_uri) == Some(digest)
                });
                if duplicate {
                    return PublicationAdmission::Duplicate;
                }
            }
        }
        if let Some(payload_digest) = payload_digest {
            self.publication_payload_digests
                .insert(document_uri.clone(), payload_digest);
        } else {
            self.publication_payload_digests.remove(&document_uri);
        }
        self.publications.insert(
            document_uri,
            PublicationState {
                document_version,
                generation,
                payload_bytes,
                delivery: PublicationDelivery::Produced,
            },
        );
        PublicationAdmission::Accepted
    }

    pub fn mark_publication_queued(&mut self, document_uri: &str) -> bool {
        self.set_publication_delivery(document_uri, PublicationDelivery::Queued)
    }

    pub fn acknowledge_publication(&mut self, document_uri: &str) -> bool {
        self.set_publication_delivery(document_uri, PublicationDelivery::BridgeAcknowledged)
    }

    pub fn acknowledge_publication_version(
        &mut self,
        document_uri: &str,
        document_version: i64,
        generation: u64,
    ) -> bool {
        let Some(publication) = self.publications.get_mut(document_uri) else {
            return false;
        };
        if (publication.document_version, publication.generation) != (document_version, generation)
        {
            return false;
        }
        publication.delivery = PublicationDelivery::BridgeAcknowledged;
        true
    }

    pub fn publication(&self, document_uri: &str) -> Option<&PublicationState> {
        self.publications.get(document_uri)
    }

    pub fn remove_publication(&mut self, document_uri: &str) -> Option<PublicationState> {
        self.publication_payload_digests.remove(document_uri);
        self.publications.remove(document_uri)
    }

    fn transition(
        &mut self,
        expected: SessionLifecycle,
        next: SessionLifecycle,
        operation: &'static str,
    ) -> Result<(), LifecycleError> {
        if self.lifecycle != expected {
            return Err(LifecycleError::InvalidTransition {
                from: self.lifecycle,
                operation,
            });
        }
        self.lifecycle = next;
        Ok(())
    }

    fn set_publication_delivery(
        &mut self,
        document_uri: &str,
        delivery: PublicationDelivery,
    ) -> bool {
        let Some(publication) = self.publications.get_mut(document_uri) else {
            return false;
        };
        publication.delivery = delivery;
        true
    }
}

#[derive(Debug)]
struct RegisteredLspSession {
    credential: LspSessionCredential,
    workspace: AuthorizedLspWorkspace,
    expires_at_ms: u64,
    control: LspSessionControl,
}

/// Bounded in-memory registry for authenticated protocol sessions.
#[derive(Debug)]
pub struct LspSessionRegistry {
    sessions: BTreeMap<LspSessionId, RegisteredLspSession>,
    max_sessions: usize,
}

impl Default for LspSessionRegistry {
    fn default() -> Self {
        Self::new(MAX_LSP_SESSIONS)
    }
}

impl LspSessionRegistry {
    pub fn new(max_sessions: usize) -> Self {
        Self {
            sessions: BTreeMap::new(),
            max_sessions,
        }
    }

    pub fn register(
        &mut self,
        authorized: AuthorizedLspSession,
        now_ms: u64,
    ) -> Result<LspSessionAccess, LspEndpointError> {
        self.expire_at(now_ms);
        if authorized.expires_at_ms <= now_ms {
            return Err(LspEndpointError::SessionExpired);
        }
        if self.sessions.contains_key(&authorized.session_id) {
            return Err(LspEndpointError::DuplicateSession);
        }
        if self.sessions.len() >= self.max_sessions {
            return Err(LspEndpointError::Saturated);
        }
        let access =
            LspSessionAccess::new(authorized.session_id.clone(), authorized.credential.clone());
        self.sessions.insert(
            authorized.session_id,
            RegisteredLspSession {
                credential: authorized.credential,
                workspace: authorized.workspace,
                expires_at_ms: authorized.expires_at_ms,
                control: LspSessionControl::default(),
            },
        );
        Ok(access)
    }

    pub fn authenticate(
        &mut self,
        access: &LspSessionAccess,
        now_ms: u64,
    ) -> Result<&mut LspSessionControl, LspEndpointError> {
        let Some(session) = self.sessions.get(access.session_id()) else {
            return Err(LspEndpointError::AuthenticationFailed);
        };
        if !constant_time_eq(
            session.credential.as_bytes(),
            access.credential().as_bytes(),
        ) {
            return Err(LspEndpointError::AuthenticationFailed);
        }
        if session.expires_at_ms <= now_ms
            || session.control.lifecycle() == SessionLifecycle::Expired
        {
            if let Some(mut expired) = self.sessions.remove(access.session_id()) {
                expired.control.expire();
            }
            return Err(LspEndpointError::SessionExpired);
        }
        self.sessions
            .get_mut(access.session_id())
            .map(|session| &mut session.control)
            .ok_or(LspEndpointError::AuthenticationFailed)
    }

    pub fn root(
        &mut self,
        access: &LspSessionAccess,
        now_ms: u64,
    ) -> Result<&AdmittedRoot, LspEndpointError> {
        self.authenticate(access, now_ms)?;
        self.sessions
            .get(access.session_id())
            .map(|session| session.workspace.primary())
            .ok_or(LspEndpointError::AuthenticationFailed)
    }

    pub fn workspace(
        &mut self,
        access: &LspSessionAccess,
        now_ms: u64,
    ) -> Result<&AuthorizedLspWorkspace, LspEndpointError> {
        self.authenticate(access, now_ms)?;
        self.sessions
            .get(access.session_id())
            .map(|session| &session.workspace)
            .ok_or(LspEndpointError::AuthenticationFailed)
    }

    pub fn detach(
        &mut self,
        access: &LspSessionAccess,
        now_ms: u64,
    ) -> Result<(), LspEndpointError> {
        self.authenticate(access, now_ms)?
            .detach()
            .map_err(LspEndpointError::Lifecycle)
    }

    pub fn close(
        &mut self,
        access: &LspSessionAccess,
        now_ms: u64,
    ) -> Result<(), LspEndpointError> {
        self.authenticate(access, now_ms)?;
        let mut session = self
            .sessions
            .remove(access.session_id())
            .ok_or(LspEndpointError::AuthenticationFailed)?;
        session.control.expire();
        Ok(())
    }

    pub fn reconnect(
        &mut self,
        access: &LspSessionAccess,
        now_ms: u64,
    ) -> Result<(), LspEndpointError> {
        self.authenticate(access, now_ms)?
            .reconnect()
            .map_err(LspEndpointError::Lifecycle)
    }

    pub fn reconnect_with_credential(
        &mut self,
        access: &LspSessionAccess,
        credential: LspSessionCredential,
        now_ms: u64,
    ) -> Result<LspSessionAccess, LspEndpointError> {
        match self.authenticate(access, now_ms)?.lifecycle() {
            SessionLifecycle::Detached => self.reconnect(access, now_ms)?,
            SessionLifecycle::AwaitingInitialize
            | SessionLifecycle::AwaitingInitialized
            | SessionLifecycle::Ready
            | SessionLifecycle::Shutdown => {}
            SessionLifecycle::Exited | SessionLifecycle::Expired => {
                return Err(LspEndpointError::SessionUnavailable);
            }
        }
        let session = self
            .sessions
            .get_mut(access.session_id())
            .ok_or(LspEndpointError::AuthenticationFailed)?;
        session.credential = credential.clone();
        Ok(LspSessionAccess::new(
            access.session_id().clone(),
            credential,
        ))
    }

    pub fn expire_at(&mut self, now_ms: u64) -> usize {
        let expired: Vec<LspSessionId> = self
            .sessions
            .iter()
            .filter(|(_, session)| session.expires_at_ms <= now_ms)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            if let Some(session) = self.sessions.get_mut(id) {
                session.control.expire();
            }
        }
        self.sessions
            .retain(|_, session| session.expires_at_ms > now_ms);
        expired.len()
    }

    pub fn active_sessions(&self) -> usize {
        self.sessions.len()
    }
}

/// Typed single-project endpoint used by daemon startup.
#[derive(Debug)]
pub struct DaemonLspSessionEndpoint<A> {
    admission: A,
    registry: LspSessionRegistry,
}

impl<A> DaemonLspSessionEndpoint<A>
where
    A: LspSessionAdmissionPort,
{
    pub fn new(admission: A) -> Self {
        Self {
            admission,
            registry: LspSessionRegistry::default(),
        }
    }

    pub fn with_registry(admission: A, registry: LspSessionRegistry) -> Self {
        Self {
            admission,
            registry,
        }
    }

    pub fn open(
        &mut self,
        request: LspSessionOpenRequest,
        now_ms: u64,
    ) -> Result<LspSessionAccess, LspEndpointError> {
        // Client folder hints are never authority: admission resolves the
        // workspace independently. The only thing enforced here is the hard
        // root ceiling, so an oversized hint cannot cost an admission.
        if request.workspace_folders.len() > MAX_LSP_WORKSPACE_ROOTS {
            return Err(LspEndpointError::AdmissionRejected);
        }
        let authorized = self.admission.admit_lsp_session(&request, now_ms)?;
        self.registry.register(authorized, now_ms)
    }

    pub fn registry(&self) -> &LspSessionRegistry {
        &self.registry
    }

    pub fn registry_mut(&mut self) -> &mut LspSessionRegistry {
        &mut self.registry
    }

    pub fn into_registry(self) -> LspSessionRegistry {
        self.registry
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let max_len = left.len().max(right.len());
    let mut difference = left.len() ^ right.len();
    for index in 0..max_len {
        let left = left.get(index).copied().unwrap_or_default();
        let right = right.get(index).copied().unwrap_or_default();
        difference |= usize::from(left ^ right);
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::gateway::AdmittedRoot;

    fn ready(limit: usize) -> LspSessionControl {
        let mut session = LspSessionControl::new(limit);
        session.begin_initialize().unwrap();
        session.initialized().unwrap();
        session
    }

    #[test]
    fn lifecycle_fails_closed_and_expiry_releases_session_state() {
        let mut session = LspSessionControl::default();
        assert!(session.initialized().is_err());
        session.begin_initialize().unwrap();
        session.initialized().unwrap();
        assert_eq!(session.lifecycle(), SessionLifecycle::Ready);
        session.admit_request(LspRequestId::Number(1), None);
        session.expire();
        assert_eq!(session.lifecycle(), SessionLifecycle::Expired);
        assert_eq!(
            session.complete_request(&LspRequestId::Number(1)),
            CompletionDisposition::UnknownRequest
        );
    }

    #[test]
    fn authorized_workspace_routes_nested_documents_to_the_deepest_root() {
        let workspace = AuthorizedLspWorkspace::new(
            Some(ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap()),
            vec![
                AdmittedRoot::authorized(
                    "file:///repo",
                    ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
                ),
                AdmittedRoot::authorized(
                    "file:///repo/nested",
                    ManifestDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
                ),
            ],
        )
        .unwrap();

        assert_eq!(
            workspace
                .resolve_document("file:///repo/nested/src/lib.rs")
                .unwrap()
                .uri(),
            "file:///repo/nested"
        );
        assert_eq!(
            workspace.resolve_document("file:///outside/lib.rs"),
            Err(LspWorkspaceRouteError::OutsideAdmittedRoots)
        );
        assert!(
            !workspace
                .admits_exact_root_hints(&["file:///repo".to_owned(), "file:///repo".to_owned(),])
        );
    }

    #[cfg(unix)]
    #[test]
    fn authorized_workspace_rejects_documents_through_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), root.path().join("escape")).unwrap();
        let root_uri = url::Url::from_file_path(root.path().canonicalize().unwrap())
            .unwrap()
            .to_string();
        let workspace = AuthorizedLspWorkspace::new(
            Some(ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap()),
            vec![AdmittedRoot::authorized(
                root_uri,
                ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
            )],
        )
        .unwrap();
        let escaped_uri = url::Url::from_file_path(root.path().join("escape/lib.rs"))
            .unwrap()
            .to_string();

        assert_eq!(
            workspace.resolve_document(&escaped_uri),
            Err(LspWorkspaceRouteError::OutsideAdmittedRoots)
        );
    }

    #[test]
    fn cancellation_and_supersession_suppress_downstream_publication() {
        let mut session = ready(4);
        let cancelled = LspRequestId::Number(1);
        let stale = LspRequestId::String("stale".into());
        assert_eq!(
            session.admit_request(cancelled.clone(), None),
            RequestAdmission::Accepted
        );
        assert_eq!(
            session.admit_request(stale.clone(), Some(("file:///root/a.rs".into(), 3))),
            RequestAdmission::Accepted
        );
        assert_eq!(
            session.cancel_request(&cancelled),
            CancellationOutcome::Accepted
        );
        session.supersede_document("file:///root/a.rs", 4);
        assert_eq!(
            session.complete_request(&cancelled),
            CompletionDisposition::SuppressCancelled
        );
        assert_eq!(
            session.complete_request(&stale),
            CompletionDisposition::SuppressContentModified
        );
    }

    #[test]
    fn request_queue_is_bounded_and_retriggerable() {
        let mut session = ready(1);
        assert_eq!(
            session.admit_request(LspRequestId::Number(1), None),
            RequestAdmission::Accepted
        );
        assert_eq!(
            session.admit_request(LspRequestId::Number(2), None),
            RequestAdmission::Saturated {
                retrigger_request: true
            }
        );
    }

    #[test]
    fn publications_are_monotone_and_reconnect_does_not_claim_exactly_once() {
        let mut session = ready(1);
        let uri = "file:///root/a.rs";
        assert_eq!(
            session.admit_publication(uri, 2, 7),
            PublicationAdmission::Accepted
        );
        assert!(session.mark_publication_queued(uri));
        assert!(session.acknowledge_publication(uri));
        assert_eq!(
            session.admit_publication(uri, 1, 99),
            PublicationAdmission::Stale
        );
        session.detach().unwrap();
        session.reconnect().unwrap();
        assert_eq!(
            session.publication(uri).unwrap().delivery,
            PublicationDelivery::Produced
        );

        session.shutdown().unwrap();
        session.exit().unwrap();
        assert_eq!(session.lifecycle(), SessionLifecycle::Exited);
        assert!(session.publication(uri).is_none());
    }

    #[test]
    fn publication_identity_includes_bounded_payload_digest() {
        let mut session = ready(1);
        let uri = "file:///root/a.rs";
        assert_eq!(
            session.admit_payload_publication(uri, 2, 7, b"first"),
            PublicationAdmission::Accepted
        );
        assert_eq!(
            session.admit_payload_publication(uri, 2, 7, b"first"),
            PublicationAdmission::Duplicate
        );
        assert_eq!(
            session.admit_payload_publication(uri, 2, 7, b"changed"),
            PublicationAdmission::Accepted
        );
        assert_eq!(
            session.admit_payload_publication(uri, 1, 99, b"stale"),
            PublicationAdmission::Stale
        );
    }

    #[test]
    fn deadlines_suppress_late_responses_and_publications_are_size_bounded() {
        let mut session = ready(2);
        let id = LspRequestId::String("deadline".into());
        assert_eq!(
            session.admit_request_with_deadline(id.clone(), None, Some(100)),
            RequestAdmission::Accepted
        );
        assert_eq!(session.expire_deadlines(99), Vec::<LspRequestId>::new());
        assert_eq!(session.expire_deadlines(100), vec![id.clone()]);
        assert_eq!(
            session.complete_request(&id),
            CompletionDisposition::SuppressTimedOut
        );
        assert_eq!(
            CompletionDisposition::SuppressTimedOut.failure(),
            Some(LspRequestFailure::ServerCancelled {
                retrigger_request: true
            })
        );
        assert_eq!(
            session.admit_sized_publication("file:///root/a.rs", 1, 1, MAX_PUBLICATION_BYTES + 1,),
            PublicationAdmission::TooLarge {
                size: MAX_PUBLICATION_BYTES + 1,
                limit: MAX_PUBLICATION_BYTES,
            }
        );
    }

    #[derive(Clone)]
    struct Admission;

    impl LspSessionAdmissionPort for Admission {
        fn admit_lsp_session(
            &self,
            _request: &LspSessionOpenRequest,
            now_ms: u64,
        ) -> Result<AuthorizedLspSession, LspEndpointError> {
            Ok(AuthorizedLspSession {
                session_id: LspSessionId::new("daemon-session-1").unwrap(),
                credential: LspSessionCredential::new(vec![7; 16]).unwrap(),
                workspace: AuthorizedLspWorkspace::single(AdmittedRoot::new("file:///admitted")),
                expires_at_ms: now_ms + LSP_SESSION_TTL_MS,
            })
        }
    }

    #[derive(Clone)]
    struct RecordingAdmission {
        calls: Arc<AtomicUsize>,
    }

    impl LspSessionAdmissionPort for RecordingAdmission {
        fn admit_lsp_session(
            &self,
            _request: &LspSessionOpenRequest,
            now_ms: u64,
        ) -> Result<AuthorizedLspSession, LspEndpointError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(AuthorizedLspSession {
                session_id: LspSessionId::new("recorded-session").unwrap(),
                credential: LspSessionCredential::new(vec![9; 16]).unwrap(),
                workspace: AuthorizedLspWorkspace::single(AdmittedRoot::new("file:///admitted")),
                expires_at_ms: now_ms + LSP_SESSION_TTL_MS,
            })
        }
    }

    #[test]
    fn endpoint_defers_folder_hints_to_admission_and_caps_oversized_hints() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut endpoint = DaemonLspSessionEndpoint::new(RecordingAdmission {
            calls: Arc::clone(&calls),
        });
        let oversized = endpoint
            .open(
                LspSessionOpenRequest {
                    workspace_folders: (0..=MAX_LSP_WORKSPACE_ROOTS)
                        .map(|index| format!("file:///folder-{index}"))
                        .collect(),
                    ..LspSessionOpenRequest::default()
                },
                10,
            )
            .expect_err("an oversized folder hint must not cost an admission");
        assert_eq!(oversized, LspEndpointError::AdmissionRejected);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(endpoint.registry().active_sessions(), 0);

        // The admitted workspace, not the hint, decides the session's roots.
        let multi = endpoint
            .open(
                LspSessionOpenRequest {
                    workspace_folders: vec!["file:///one".into(), "file:///two".into()],
                    ..LspSessionOpenRequest::default()
                },
                10,
            )
            .expect("multi-folder hints defer to admission");
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            endpoint.registry_mut().root(&multi, 11).unwrap().uri(),
            "file:///admitted"
        );
        endpoint.registry_mut().expire_at(u64::MAX);
        calls.store(0, Ordering::Relaxed);

        let access = endpoint
            .open(
                LspSessionOpenRequest {
                    requested_root_uri: Some("file:///untrusted".into()),
                    ..LspSessionOpenRequest::default()
                },
                10,
            )
            .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            endpoint.registry_mut().root(&access, 11).unwrap().uri(),
            "file:///admitted"
        );
    }

    #[test]
    fn registry_authenticates_reconnects_and_reclaims_expired_capacity() {
        let mut endpoint = DaemonLspSessionEndpoint::new(Admission);
        let access = endpoint.open(LspSessionOpenRequest::default(), 0).unwrap();
        let forged = LspSessionAccess::new(
            access.session_id().clone(),
            LspSessionCredential::new(vec![8; 16]).unwrap(),
        );
        assert_eq!(
            endpoint.registry_mut().authenticate(&forged, 1).err(),
            Some(LspEndpointError::AuthenticationFailed)
        );

        let control = endpoint.registry_mut().authenticate(&access, 1).unwrap();
        control.begin_initialize().unwrap();
        control.initialized().unwrap();
        endpoint.registry_mut().detach(&access, 2).unwrap();
        let reconnected = endpoint
            .registry_mut()
            .reconnect_with_credential(&access, LspSessionCredential::new(vec![9; 16]).unwrap(), 3)
            .unwrap();
        assert_eq!(endpoint.registry().active_sessions(), 1);
        assert_eq!(
            endpoint.registry_mut().authenticate(&access, 4).err(),
            Some(LspEndpointError::AuthenticationFailed)
        );
        endpoint.registry_mut().close(&reconnected, 4).unwrap();
        assert_eq!(endpoint.registry().active_sessions(), 0);

        let mut registry = LspSessionRegistry::new(1);
        registry
            .register(
                AuthorizedLspSession {
                    session_id: LspSessionId::new("expired").unwrap(),
                    credential: LspSessionCredential::new(vec![1; 16]).unwrap(),
                    workspace: AuthorizedLspWorkspace::single(AdmittedRoot::new(
                        "file:///admitted",
                    )),
                    expires_at_ms: 10,
                },
                0,
            )
            .unwrap();
        assert!(
            registry
                .register(
                    AuthorizedLspSession {
                        session_id: LspSessionId::new("replacement").unwrap(),
                        credential: LspSessionCredential::new(vec![2; 16]).unwrap(),
                        workspace: AuthorizedLspWorkspace::single(AdmittedRoot::new(
                            "file:///admitted",
                        )),
                        expires_at_ms: 20,
                    },
                    10,
                )
                .is_ok()
        );
    }

    #[test]
    fn credentials_remain_redacted() {
        let credential = LspSessionCredential::new(b"never-render-this-secret".to_vec()).unwrap();
        assert_eq!(
            format!("{credential:?}"),
            "LspSessionCredential([redacted])"
        );
    }
}
