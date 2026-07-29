//! Authenticated daemon LSP session endpoint and single-root registry.
//!
//! This module is intentionally an in-memory coordination boundary. The
//! injected admission port is the retained daemon authority that authenticates
//! a bridge, resolves the canonical root, and supplies an opaque session
//! credential. The registry never opens a database, a socket, or an analyzer.

use std::collections::BTreeMap;
use std::fmt;

use tracedecay_lsp::{AdmittedRoot, LifecycleError, LspSessionControl, SessionLifecycle};

/// PR12 permits one admitted root and a bounded number of live bridge
/// sessions in one daemon process.
pub const MAX_LSP_SESSIONS: usize = 64;
/// Detached session state, including unsaved overlays owned by the session
/// actor, is deterministically discarded after this TTL.
pub const LSP_SESSION_TTL_MS: u64 = 15 * 60 * 1_000;

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

/// Opaque credential minted by the daemon admission authority. It is not a
/// project, database, or filesystem capability.
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

    /// Returns credential material only to the daemon's authenticated wire
    /// adapter. Presentation code must never log or render this value.
    pub(crate) fn as_bytes(&self) -> &[u8] {
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

    /// Returns the opaque credential only to the retained daemon invocation
    /// service so it can encode a bridge capability over an authenticated
    /// local transport.
    pub(crate) fn credential(&self) -> &LspSessionCredential {
        &self.credential
    }
}

/// The bridge's request to begin one typed daemon session. The requested root
/// is a presentation hint only; admission owns canonical resolution.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LspSessionOpenRequest {
    pub requested_root_uri: Option<String>,
    pub workspace_folders: Vec<String>,
    pub client_revision: String,
}

/// A daemon-authorized session result. Only the retained daemon admission
/// implementation can construct this with a validated root and credential.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizedLspSession {
    pub session_id: LspSessionId,
    pub credential: LspSessionCredential,
    pub root: AdmittedRoot,
    pub expires_at_ms: u64,
}

/// Daemon-owned admission port. Implementations authenticate the bridge and
/// resolve one canonical root before document content is accepted.
pub trait LspSessionAdmissionPort {
    fn admit_lsp_session(
        &self,
        request: &LspSessionOpenRequest,
        now_ms: u64,
    ) -> Result<AuthorizedLspSession, LspEndpointError>;
}

/// Typed failure state for endpoint/session operation. These values are later
/// mapped to one startup error or JSON-RPC error by the daemon protocol actor;
/// they never trigger a client-side private daemon or analyzer fallback.
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

#[derive(Debug)]
struct RegisteredLspSession {
    credential: LspSessionCredential,
    root: AdmittedRoot,
    expires_at_ms: u64,
    control: LspSessionControl,
}

/// Retained daemon registry for the protocol actors serving bridge sessions.
/// It stores only ephemeral control state and credentials; gateway business
/// operations are injected into each actor rather than accepted as raw bytes.
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
        // Admission cannot be permanently saturated by entries whose bounded
        // lifetime has already elapsed, even if no independent sweeper ran.
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
                root: authorized.root,
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
        let Some(session) = self.sessions.get(&access.session_id) else {
            // Do not distinguish an unknown id from an invalid secret.
            return Err(LspEndpointError::AuthenticationFailed);
        };
        if !constant_time_eq(&session.credential.0, &access.credential.0) {
            return Err(LspEndpointError::AuthenticationFailed);
        }
        if session.expires_at_ms <= now_ms
            || session.control.lifecycle() == SessionLifecycle::Expired
        {
            if let Some(mut expired) = self.sessions.remove(&access.session_id) {
                expired.control.expire();
            }
            return Err(LspEndpointError::SessionExpired);
        }
        self.sessions
            .get_mut(&access.session_id)
            .map(|session| &mut session.control)
            .ok_or(LspEndpointError::AuthenticationFailed)
    }

    pub fn root(
        &mut self,
        access: &LspSessionAccess,
        now_ms: u64,
    ) -> Result<&AdmittedRoot, LspEndpointError> {
        // Authenticate first, then borrow the root independently.
        self.authenticate(access, now_ms)?;
        self.sessions
            .get(&access.session_id)
            .map(|session| &session.root)
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
            .remove(&access.session_id)
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
            .get_mut(&access.session_id)
            .ok_or(LspEndpointError::AuthenticationFailed)?;
        session.credential = credential.clone();
        Ok(LspSessionAccess::new(access.session_id.clone(), credential))
    }

    /// Expiry releases the registry's ephemeral control state. A caller that
    /// holds an old access capability cannot reconstruct a session.
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

/// A typed endpoint wrapper used by daemon startup code. It rejects a PR15
/// multi-root shape before delegation and otherwise relies solely on the
/// injected daemon admission authority.
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
        if request.workspace_folders.len() > 1 {
            return Err(LspEndpointError::MultipleRootsUnsupported);
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

    /// Returns the retained registry after a short-lived admission wrapper
    /// used by the daemon invocation service. The registry remains the sole
    /// credential and expiry authority.
    pub(crate) fn into_registry(self) -> LspSessionRegistry {
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
    use super::*;

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
                root: AdmittedRoot::new("file:///admitted"),
                expires_at_ms: now_ms + LSP_SESSION_TTL_MS,
            })
        }
    }

    #[test]
    fn endpoint_admits_one_root_only_and_never_uses_the_requested_root_as_authority() {
        let mut endpoint = DaemonLspSessionEndpoint::new(Admission);
        assert_eq!(
            endpoint.open(
                LspSessionOpenRequest {
                    workspace_folders: vec!["file:///one".into(), "file:///two".into()],
                    ..LspSessionOpenRequest::default()
                },
                10,
            ),
            Err(LspEndpointError::MultipleRootsUnsupported)
        );
        let access = endpoint
            .open(
                LspSessionOpenRequest {
                    requested_root_uri: Some("file:///untrusted".into()),
                    ..LspSessionOpenRequest::default()
                },
                10,
            )
            .unwrap();
        assert_eq!(
            endpoint.registry_mut().root(&access, 11).unwrap().uri(),
            "file:///admitted"
        );
    }

    #[test]
    fn registry_authenticates_detach_reconnect_and_close() {
        let mut endpoint = DaemonLspSessionEndpoint::new(Admission);
        let access = endpoint.open(LspSessionOpenRequest::default(), 0).unwrap();
        let forged = LspSessionAccess::new(
            access.session_id.clone(),
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
        assert_eq!(
            endpoint.registry_mut().reconnect(&reconnected, 5).err(),
            Some(LspEndpointError::AuthenticationFailed)
        );
    }

    #[test]
    fn registration_reclaims_expired_capacity_before_saturation_check() {
        let mut registry = LspSessionRegistry::new(1);
        registry
            .register(
                AuthorizedLspSession {
                    session_id: LspSessionId::new("expired").unwrap(),
                    credential: LspSessionCredential::new(vec![1; 16]).unwrap(),
                    root: AdmittedRoot::new("file:///admitted"),
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
                        root: AdmittedRoot::new("file:///admitted"),
                        expires_at_ms: 20,
                    },
                    10,
                )
                .is_ok()
        );
        assert_eq!(registry.active_sessions(), 1);
    }
}
