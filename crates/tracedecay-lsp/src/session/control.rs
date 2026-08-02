use std::collections::BTreeMap;

use tracedecay_domain::ContentDigest;

use super::{
    LifecycleError, LspRequestId, MAX_PENDING_REQUESTS, MAX_PUBLICATION_BYTES, SessionLifecycle,
};

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
