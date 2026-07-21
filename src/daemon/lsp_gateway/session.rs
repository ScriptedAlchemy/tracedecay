//! Daemon-owned LSP session lifecycle, request admission, and publication
//! ordering. The stdio bridge has no copy of this state.

use std::collections::BTreeMap;

pub const MAX_PENDING_REQUESTS: usize = 64;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LspRequestId {
    Number(i64),
    String(String),
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
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingRequest {
    document: Option<(String, i64)>,
    state: PendingState,
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
    UnknownRequest,
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
    pub delivery: PublicationDelivery,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationAdmission {
    Accepted,
    Duplicate,
    Stale,
    SessionUnavailable,
}

/// Mutable control state owned and serialized by one daemon session actor.
pub struct LspSessionControl {
    lifecycle: SessionLifecycle,
    detached_from: Option<SessionLifecycle>,
    pending: BTreeMap<LspRequestId, PendingRequest>,
    publications: BTreeMap<String, PublicationState>,
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
        Ok(())
    }

    pub fn detach(&mut self) -> Result<(), LifecycleError> {
        if !matches!(
            self.lifecycle,
            SessionLifecycle::Ready | SessionLifecycle::Shutdown
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
    }

    pub fn admit_request(
        &mut self,
        id: LspRequestId,
        document: Option<(String, i64)>,
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

    pub fn complete_request(&mut self, id: &LspRequestId) -> CompletionDisposition {
        match self.pending.remove(id).map(|request| request.state) {
            Some(PendingState::Active) => CompletionDisposition::Publish,
            Some(PendingState::Cancelled) => CompletionDisposition::SuppressCancelled,
            Some(PendingState::ContentModified) => CompletionDisposition::SuppressContentModified,
            None => CompletionDisposition::UnknownRequest,
        }
    }

    pub fn admit_publication(
        &mut self,
        document_uri: impl Into<String>,
        document_version: i64,
        generation: u64,
    ) -> PublicationAdmission {
        if self.lifecycle != SessionLifecycle::Ready {
            return PublicationAdmission::SessionUnavailable;
        }
        let document_uri = document_uri.into();
        if let Some(current) = self.publications.get(&document_uri) {
            let key = (document_version, generation);
            let current_key = (current.document_version, current.generation);
            if key < current_key {
                return PublicationAdmission::Stale;
            }
            if key == current_key {
                return PublicationAdmission::Duplicate;
            }
        }
        self.publications.insert(
            document_uri,
            PublicationState {
                document_version,
                generation,
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

    pub fn publication(&self, document_uri: &str) -> Option<&PublicationState> {
        self.publications.get(document_uri)
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
