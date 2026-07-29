//! Production adapters between daemon-owned LSP ports and PR12 authorities.
//!
//! These adapters deliberately own only bounded in-flight correlation. The
//! graph/analyzer broker, diagnostic broker, feedback cycle, canonical
//! feedback reads, and projection truth remain supplied authorities.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::runtime::Handle;
use tokio::sync::oneshot::error::TryRecvError;
use tokio::sync::{Mutex as AsyncMutex, Semaphore, oneshot};
use tokio::task::AbortHandle;

use tracedecay_lsp::{
    AdmittedRoot, AnalyzerCancellationPort, CallHierarchyItem, ContextExpansionOutcome,
    ContextExpansionRequest, ContextProjectionChange, ContextProjectionKind,
    ContextProjectionOutcome, ContextProjectionPort, ContextProjectionRegistration,
    ContextProjectionRequest, DiagnosticRefreshAdmission, DiagnosticRefreshIdentity,
    DiagnosticSeverity, DiagnosticSnapshotOutcome, DiagnosticSnapshotPort, DiagnosticSource,
    DocumentSymbol, FeedbackCyclePort, FeedbackCycleRequest, FeedbackCycleResponse,
    GatewayDiagnostic, GenerationDiagnostics, Hover, IncomingCall, LspLocation, LspPosition,
    LspRange, LspRequestId, MAX_PENDING_REQUESTS, OutgoingCall, OverlaySnapshot,
    ProcessLocalRequestSequence, RenameCandidateResult, RenameCandidateUnavailableReason,
    SemanticProviderOutcome, SemanticProviderPort, SemanticRequest, SemanticResponse,
    SignatureHelp, TRACEDECAY_CONTEXT_REVISION, TypeHierarchyItem, WorkspaceSymbol,
};

use crate::diagnostics::lsp::broker::{
    CodeDiagnostic, DiagnosticBroker, DiagnosticSeverity as BrokerDiagnosticSeverity,
};
use crate::diagnostics::lsp::client::{LspDocument, LspSemanticRequest};

/// A Send future returned by an application-owned PR12 runtime authority.
pub type LspRuntimeFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

const MAX_RUNTIME_FAILURE_CLASS_BYTES: usize = 96;
const MAX_PR12_DIAGNOSTIC_OPERATIONS: usize = 128;
/// The Plan 35 engine queue bound applies to independently triggered cycles.
pub const MAX_PR12_FEEDBACK_CYCLES: usize = 128;
const MAX_PR12_CONTEXT_OPERATIONS: usize = MAX_PENDING_REQUESTS * 2;
const MAX_PR12_SEMANTIC_OPERATIONS: usize = MAX_PENDING_REQUESTS * 2;

/// A bounded, protocol-safe failure class supplied by an application owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LspRuntimeFailure {
    class: String,
}

impl LspRuntimeFailure {
    pub fn new(class: impl Into<String>) -> Self {
        let mut bounded = String::new();
        for character in class.into().chars() {
            if bounded.len().saturating_add(character.len_utf8()) > MAX_RUNTIME_FAILURE_CLASS_BYTES
            {
                break;
            }
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                bounded.push(character);
            }
        }
        if bounded.is_empty() {
            bounded.push_str("runtime-failure");
        }
        Self { class: bounded }
    }

    pub fn class(&self) -> &str {
        &self.class
    }
}

/// Exact input passed to canonical diagnostic refresh work. Overlay text stays
/// ephemeral and is never written by this adapter.
#[derive(Clone, Debug)]
pub struct CanonicalDiagnosticRefreshRequest {
    pub root: AdmittedRoot,
    pub document_uri: String,
    pub overlay: Option<OverlaySnapshot>,
    pub source_generation: Option<u64>,
}

/// Reads an admitted document through the existing source/overlay authority.
///
/// This avoids a gateway-local filesystem fallback for clean documents.
pub trait LspDiagnosticDocumentPort: Send + Sync {
    fn load_document(
        &self,
        request: CanonicalDiagnosticRefreshRequest,
    ) -> LspRuntimeFuture<Result<LspDocument, LspRuntimeFailure>>;
}

/// Current canonical managed diagnostics created by the feedback-cycle owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedDiagnosticSnapshot {
    pub generation: u64,
    pub diagnostics: Vec<GatewayDiagnostic>,
}

/// Reads managed diagnostics from the canonical feedback/diagnostic authority.
pub trait ManagedDiagnosticSnapshotPort: Send + Sync {
    fn snapshot(
        &self,
        request: CanonicalDiagnosticRefreshRequest,
    ) -> LspRuntimeFuture<Result<ManagedDiagnosticSnapshot, LspRuntimeFailure>>;
}

/// Non-blocking application boundary for a complete LSP diagnostic snapshot.
pub trait CanonicalDiagnosticSnapshotAuthority: Send + Sync {
    fn refresh(
        &self,
        request: CanonicalDiagnosticRefreshRequest,
    ) -> LspRuntimeFuture<Result<GenerationDiagnostics, LspRuntimeFailure>>;
}

/// Concrete diagnostic authority that reuses the existing diagnostics LSP
/// broker for upstream findings and the injected canonical feedback reader for
/// managed findings. It has no gateway-private diagnostic store.
pub struct BrokerDiagnosticSnapshotAuthority<S, M> {
    broker: Arc<AsyncMutex<DiagnosticBroker>>,
    documents: Arc<S>,
    managed: Arc<M>,
    diagnostics_quiet_window: Duration,
}

impl<S, M> BrokerDiagnosticSnapshotAuthority<S, M> {
    pub fn new(
        broker: Arc<AsyncMutex<DiagnosticBroker>>,
        documents: Arc<S>,
        managed: Arc<M>,
        diagnostics_quiet_window: Duration,
    ) -> Self {
        Self {
            broker,
            documents,
            managed,
            diagnostics_quiet_window,
        }
    }
}

impl<S, M> CanonicalDiagnosticSnapshotAuthority for BrokerDiagnosticSnapshotAuthority<S, M>
where
    S: LspDiagnosticDocumentPort + 'static,
    M: ManagedDiagnosticSnapshotPort + 'static,
{
    fn refresh(
        &self,
        request: CanonicalDiagnosticRefreshRequest,
    ) -> LspRuntimeFuture<Result<GenerationDiagnostics, LspRuntimeFailure>> {
        let broker = Arc::clone(&self.broker);
        let documents = Arc::clone(&self.documents);
        let managed = Arc::clone(&self.managed);
        let diagnostics_quiet_window = self.diagnostics_quiet_window;
        Box::pin(async move {
            let document = documents.load_document(request.clone()).await?;
            let language = document.language.clone();
            let relative_path = document.relative_path.clone();
            let prepared = {
                let mut broker = broker.lock().await;
                broker
                    .prepare_refresh(&language, vec![document])
                    .map_err(|_| LspRuntimeFailure::new("diagnostic-broker-preparation-failed"))?
            };

            if let Some(prepared) = prepared {
                let completed = prepared.collect_diagnostics(diagnostics_quiet_window).await;
                broker
                    .lock()
                    .await
                    .finish_refresh(completed)
                    .map_err(|_| LspRuntimeFailure::new("diagnostic-broker-refresh-failed"))?;
            }

            let upstream = broker
                .lock()
                .await
                .snapshot()
                .diagnostics
                .into_iter()
                .filter(|diagnostic| diagnostic.file == relative_path)
                .map(|diagnostic| broker_diagnostic(request.document_uri.as_str(), diagnostic))
                .collect();
            let managed = managed.snapshot(request).await?;
            Ok(GenerationDiagnostics {
                generation: managed.generation,
                upstream,
                tracedecay: managed.diagnostics,
            })
        })
    }
}

fn broker_diagnostic(document_uri: &str, diagnostic: CodeDiagnostic) -> GatewayDiagnostic {
    let start_line = diagnostic.line_start.saturating_sub(1);
    let end_line = diagnostic
        .line_end
        .max(diagnostic.line_start)
        .saturating_sub(1);
    let start_character = diagnostic.character_start.unwrap_or_default();
    let end_character = diagnostic.character_end.unwrap_or(start_character);
    GatewayDiagnostic {
        uri: document_uri.to_owned(),
        range: LspRange {
            start: LspPosition {
                line: start_line,
                character: start_character,
            },
            end: LspPosition {
                line: end_line,
                character: if end_line == start_line {
                    end_character.max(start_character)
                } else {
                    end_character
                },
            },
        },
        severity: Some(match diagnostic.severity {
            BrokerDiagnosticSeverity::Error => DiagnosticSeverity::Error,
            BrokerDiagnosticSeverity::Warning => DiagnosticSeverity::Warning,
            BrokerDiagnosticSeverity::Information => DiagnosticSeverity::Information,
            BrokerDiagnosticSeverity::Hint => DiagnosticSeverity::Hint,
        }),
        code: diagnostic.code,
        code_description_uri: None,
        message: diagnostic.message,
        source: DiagnosticSource::Upstream,
        related_information: Vec::new(),
        data: None,
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DiagnosticOperationKey {
    root_uri: String,
    document_uri: String,
    overlay_version: i64,
    overlay_digest: Option<[u8; 32]>,
}

struct PendingDiagnosticOperation {
    identity: DiagnosticRefreshIdentity,
    receiver: oneshot::Receiver<DiagnosticSnapshotOutcome>,
    abort: AbortHandle,
}

/// Non-blocking `DiagnosticSnapshotPort` over the canonical diagnostics
/// authority. Completed results remain only in a bounded one-shot mailbox
/// until the protocol session consumes them; existing client publications are
/// intentionally left in place while a refresh runs.
pub struct Pr12DiagnosticSnapshotAdapter {
    runtime: Handle,
    authority: Arc<dyn CanonicalDiagnosticSnapshotAuthority>,
    next_operation: ProcessLocalRequestSequence,
    in_flight: Mutex<BTreeMap<DiagnosticOperationKey, PendingDiagnosticOperation>>,
}

impl Pr12DiagnosticSnapshotAdapter {
    pub fn new(runtime: Handle, authority: Arc<dyn CanonicalDiagnosticSnapshotAuthority>) -> Self {
        Self {
            runtime,
            authority,
            next_operation: ProcessLocalRequestSequence::starting_at(1),
            in_flight: Mutex::new(BTreeMap::new()),
        }
    }

    fn key(
        root: &AdmittedRoot,
        document_uri: &str,
        overlay: Option<&OverlaySnapshot>,
    ) -> DiagnosticOperationKey {
        DiagnosticOperationKey {
            root_uri: root.uri().to_owned(),
            document_uri: document_uri.to_owned(),
            overlay_version: overlay.map_or(0, |overlay| overlay.version),
            overlay_digest: overlay.map(|overlay| {
                let mut hasher = Sha256::new();
                hasher.update(overlay.language_id.as_bytes());
                hasher.update([0]);
                hasher.update(overlay.text.as_bytes());
                hasher.finalize().into()
            }),
        }
    }
}

impl DiagnosticSnapshotPort for Pr12DiagnosticSnapshotAdapter {
    fn document_diagnostics(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        overlay: Option<&OverlaySnapshot>,
    ) -> DiagnosticSnapshotOutcome {
        let key = Self::key(root, document_uri, overlay);
        let Ok(mut in_flight) = self.in_flight.try_lock() else {
            return DiagnosticSnapshotOutcome::Partial {
                source_generation: None,
                coverage: "runtime-busy".to_owned(),
            };
        };
        match in_flight
            .get_mut(&key)
            .map(|pending| (pending.identity.clone(), pending.receiver.try_recv()))
        {
            Some((_, Ok(outcome))) => {
                in_flight.remove(&key);
                outcome
            }
            Some((identity, Err(TryRecvError::Empty))) => {
                DiagnosticSnapshotOutcome::Refreshing(identity)
            }
            Some((_, Err(TryRecvError::Closed))) => {
                in_flight.remove(&key);
                DiagnosticSnapshotOutcome::Failed {
                    source_generation: None,
                    failure_class: "diagnostic-operation-dropped".to_owned(),
                }
            }
            None => DiagnosticSnapshotOutcome::Partial {
                source_generation: None,
                coverage: "refresh-required".to_owned(),
            },
        }
    }

    fn request_document_refresh(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        overlay: Option<&OverlaySnapshot>,
        source_generation: Option<u64>,
    ) -> DiagnosticRefreshAdmission {
        let key = Self::key(root, document_uri, overlay);
        let Ok(mut in_flight) = self.in_flight.try_lock() else {
            return DiagnosticRefreshAdmission::Rejected {
                failure_class: "runtime-busy".to_owned(),
            };
        };
        if let Some(pending) = in_flight.get(&key) {
            return DiagnosticRefreshAdmission::AlreadyRunning(pending.identity.clone());
        }
        if in_flight.len() >= MAX_PR12_DIAGNOSTIC_OPERATIONS {
            return DiagnosticRefreshAdmission::Rejected {
                failure_class: "diagnostic-capacity".to_owned(),
            };
        }

        let Ok(operation_id) = self.next_operation.next_string("lsp-diagnostic-") else {
            return DiagnosticRefreshAdmission::Rejected {
                failure_class: "diagnostic-identity-exhausted".to_owned(),
            };
        };
        let identity = DiagnosticRefreshIdentity {
            operation_id: operation_id.clone(),
            source_generation,
            target_generation: None,
        };
        let request = CanonicalDiagnosticRefreshRequest {
            root: root.clone(),
            document_uri: document_uri.to_owned(),
            overlay: overlay.cloned(),
            source_generation,
        };
        let authority = Arc::clone(&self.authority);
        let (sender, receiver) = oneshot::channel();
        let completed_operation_id = operation_id;
        let abort = self
            .runtime
            .spawn(async move {
                let outcome = match authority.refresh(request).await {
                    Ok(diagnostics) => DiagnosticSnapshotOutcome::Ready {
                        diagnostics,
                        completed_operation_id: Some(completed_operation_id),
                    },
                    Err(error) => DiagnosticSnapshotOutcome::Failed {
                        source_generation,
                        failure_class: error.class().to_owned(),
                    },
                };
                let _ = sender.send(outcome);
            })
            .abort_handle();
        in_flight.insert(
            key,
            PendingDiagnosticOperation {
                identity: identity.clone(),
                receiver,
                abort,
            },
        );
        DiagnosticRefreshAdmission::Started(identity)
    }
}

impl Drop for Pr12DiagnosticSnapshotAdapter {
    fn drop(&mut self) {
        if let Ok(in_flight) = self.in_flight.get_mut() {
            for pending in in_flight.values() {
                pending.abort.abort();
            }
        }
    }
}

/// Enqueues the real Plan 09 feedback cycle from a typed LSP lifecycle event.
pub trait FeedbackCycleRuntimePort: Send + Sync {
    fn execute(
        &self,
        request: FeedbackCycleRequest,
    ) -> LspRuntimeFuture<Result<(), LspRuntimeFailure>>;
}

/// Non-blocking feedback-cycle trigger. A returned `Accepted` means the
/// canonical operation was admitted to the bounded daemon task set; it never
/// claims a completed finding result before the authoritative cycle publishes.
pub struct Pr12FeedbackCycleAdapter {
    runtime: Handle,
    authority: Arc<dyn FeedbackCycleRuntimePort>,
    capacity: Arc<Semaphore>,
}

impl Pr12FeedbackCycleAdapter {
    pub fn new(runtime: Handle, authority: Arc<dyn FeedbackCycleRuntimePort>) -> Self {
        Self {
            runtime,
            authority,
            capacity: Arc::new(Semaphore::new(MAX_PR12_FEEDBACK_CYCLES)),
        }
    }
}

impl FeedbackCyclePort for Pr12FeedbackCycleAdapter {
    fn request_feedback_cycle(&self, request: FeedbackCycleRequest) -> FeedbackCycleResponse {
        let Ok(permit) = Arc::clone(&self.capacity).try_acquire_owned() else {
            return FeedbackCycleResponse::Deferred {
                reason: "feedback-cycle-capacity".to_owned(),
            };
        };
        let authority = Arc::clone(&self.authority);
        let _task = self.runtime.spawn(async move {
            let _permit = permit;
            if let Err(error) = authority.execute(request).await {
                eprintln!(
                    "[tracedecay] event=lsp_feedback_cycle_failed failure_class={}",
                    error.class()
                );
            }
        });
        FeedbackCycleResponse::Accepted
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LspSemanticOperationOutcome {
    Complete(Value),
    Partial {
        value: Value,
        /// Short stable identifier callers match on (e.g.
        /// `analyzer-start-failed`); never carries free-form text.
        coverage: String,
        /// Allowlisted static failure template surfaced to LSP callers in the
        /// JSON-RPC error `data`; never carries analyzer or graph error text.
        detail: Option<&'static str>,
    },
    RenameCandidate(RenameCandidateResult),
    Unavailable,
}

impl LspSemanticOperationOutcome {
    pub const ANALYZER_START_FAILED_DETAIL: &'static str = "Analyzer failed to start.";
    pub const ANALYZER_CANCELLED_DETAIL: &'static str = "Analyzer request was cancelled.";
    pub const ANALYZER_TIMEOUT_DETAIL: &'static str = "Analyzer request timed out.";
    pub const ANALYZER_REMOTE_ERROR_DETAIL: &'static str =
        "Analyzer request failed with a remote error.";
    pub const ANALYZER_TRANSPORT_FAILED_DETAIL: &'static str = "Analyzer transport failed.";
    pub const ANALYZER_INVALID_RESPONSE_DETAIL: &'static str =
        "Analyzer returned an invalid response.";
    pub const GRAPH_READ_FAILED_DETAIL: &'static str = "Graph semantic read failed.";
}

/// Canonical asynchronous owner for one retained standard analyzer request.
/// Implementations own process/client correlation and application
/// cancellation; this adapter owns only bounded protocol polling state.
pub trait LspSemanticRequestAuthority: Send + Sync {
    fn start(
        &self,
        root: AdmittedRoot,
        request_id: LspRequestId,
        request: LspSemanticRequest,
    ) -> LspRuntimeFuture<LspSemanticOperationOutcome>;

    fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool;
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SemanticRequestKey {
    root_uri: String,
    request_id: LspRequestId,
}

struct PendingSemanticOperation {
    method: &'static str,
    receiver: oneshot::Receiver<LspSemanticOperationOutcome>,
    abort: AbortHandle,
}

/// Non-blocking semantic provider over an asynchronous analyzer authority.
/// Repeated calls with the same admitted-root/request id poll one retained
/// operation; no Tokio runtime thread is synchronously blocked.
pub struct Pr12SemanticProviderAdapter {
    runtime: Handle,
    authority: Arc<dyn LspSemanticRequestAuthority>,
    in_flight: Mutex<BTreeMap<SemanticRequestKey, PendingSemanticOperation>>,
}

impl Pr12SemanticProviderAdapter {
    pub fn new(runtime: Handle, authority: Arc<dyn LspSemanticRequestAuthority>) -> Self {
        Self {
            runtime,
            authority,
            in_flight: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn shared(runtime: Handle, authority: Arc<dyn LspSemanticRequestAuthority>) -> Arc<Self> {
        Arc::new(Self::new(runtime, authority))
    }

    fn key(root: &AdmittedRoot, request_id: &LspRequestId) -> SemanticRequestKey {
        SemanticRequestKey {
            root_uri: root.uri().to_owned(),
            request_id: request_id.clone(),
        }
    }

    pub fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        let key = Self::key(root, request_id);
        let authority_cancelled = self.authority.cancel_request(root, request_id);
        let pending = self
            .in_flight
            .try_lock()
            .ok()
            .and_then(|mut in_flight| in_flight.remove(&key));
        if let Some(pending) = pending.as_ref()
            && !authority_cancelled
        {
            pending.abort.abort();
        }
        authority_cancelled || pending.is_some()
    }

    fn request(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
        request: &SemanticRequest,
    ) -> SemanticProviderOutcome<SemanticResponse> {
        let wire_request = match lsp_semantic_request(request) {
            Ok(request) => request,
            Err(coverage) => {
                return SemanticProviderOutcome::Partial {
                    value: empty_semantic_response(request),
                    coverage,
                    detail: None,
                };
            }
        };
        let method = wire_request.method();
        let key = Self::key(root, request_id);
        let Ok(mut in_flight) = self.in_flight.try_lock() else {
            return SemanticProviderOutcome::Partial {
                value: empty_semantic_response(request),
                coverage: "semantic-runtime-busy".to_owned(),
                detail: None,
            };
        };
        if let Some(pending) = in_flight.get_mut(&key) {
            if pending.method != method {
                return SemanticProviderOutcome::Partial {
                    value: empty_semantic_response(request),
                    coverage: "semantic-request-correlation-mismatch".to_owned(),
                    detail: None,
                };
            }
            return match pending.receiver.try_recv() {
                Ok(outcome) => {
                    in_flight.remove(&key);
                    project_semantic_outcome(root, request, outcome)
                }
                Err(TryRecvError::Empty) => SemanticProviderOutcome::Pending,
                Err(TryRecvError::Closed) => {
                    in_flight.remove(&key);
                    SemanticProviderOutcome::Partial {
                        value: empty_semantic_response(request),
                        coverage: "semantic-operation-dropped".to_owned(),
                        detail: None,
                    }
                }
            };
        }
        if in_flight.len() >= MAX_PR12_SEMANTIC_OPERATIONS {
            return SemanticProviderOutcome::Partial {
                value: empty_semantic_response(request),
                coverage: "semantic-operation-capacity".to_owned(),
                detail: None,
            };
        }

        let authority = Arc::clone(&self.authority);
        let root = root.clone();
        let request_id = request_id.clone();
        let (sender, receiver) = oneshot::channel();
        let abort = self
            .runtime
            .spawn(async move {
                let _ = sender.send(authority.start(root, request_id, wire_request).await);
            })
            .abort_handle();
        in_flight.insert(
            key,
            PendingSemanticOperation {
                method,
                receiver,
                abort,
            },
        );
        SemanticProviderOutcome::Pending
    }
}

impl SemanticProviderPort for Pr12SemanticProviderAdapter {
    fn request(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
        request: &SemanticRequest,
    ) -> SemanticProviderOutcome<SemanticResponse> {
        Pr12SemanticProviderAdapter::request(self, root, request_id, request)
    }
}

impl LspAnalyzerCancellationAuthority for Pr12SemanticProviderAdapter {
    fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        Pr12SemanticProviderAdapter::cancel_request(self, root, request_id)
    }
}

impl Drop for Pr12SemanticProviderAdapter {
    fn drop(&mut self) {
        if let Ok(in_flight) = self.in_flight.get_mut() {
            for pending in in_flight.values() {
                pending.abort.abort();
            }
        }
    }
}

fn lsp_semantic_request(request: &SemanticRequest) -> Result<LspSemanticRequest, String> {
    let position_params = |document_uri: &str, position: LspPosition| {
        json!({
            "textDocument": { "uri": document_uri },
            "position": position_value(position),
        })
    };
    match request {
        SemanticRequest::Declaration {
            document_uri,
            position,
        } => decode_lsp(position_params(document_uri, *position))
            .map(LspSemanticRequest::Declaration),
        SemanticRequest::Definition {
            document_uri,
            position,
        } => {
            decode_lsp(position_params(document_uri, *position)).map(LspSemanticRequest::Definition)
        }
        SemanticRequest::TypeDefinition {
            document_uri,
            position,
        } => decode_lsp(position_params(document_uri, *position))
            .map(LspSemanticRequest::TypeDefinition),
        SemanticRequest::Implementation {
            document_uri,
            position,
        } => decode_lsp(position_params(document_uri, *position))
            .map(LspSemanticRequest::Implementation),
        SemanticRequest::References {
            document_uri,
            position,
        } => {
            let mut params = position_params(document_uri, *position);
            params["context"] = json!({ "includeDeclaration": true });
            decode_lsp(params).map(LspSemanticRequest::References)
        }
        SemanticRequest::Hover {
            document_uri,
            position,
        } => decode_lsp(position_params(document_uri, *position)).map(LspSemanticRequest::Hover),
        SemanticRequest::DocumentSymbols { document_uri } => {
            decode_lsp(json!({ "textDocument": { "uri": document_uri } }))
                .map(LspSemanticRequest::DocumentSymbols)
        }
        SemanticRequest::WorkspaceSymbols { query } => {
            decode_lsp(json!({ "query": query })).map(LspSemanticRequest::WorkspaceSymbols)
        }
        SemanticRequest::PrepareCallHierarchy {
            document_uri,
            position,
        } => decode_lsp(position_params(document_uri, *position))
            .map(LspSemanticRequest::PrepareCallHierarchy),
        SemanticRequest::IncomingCalls { item } => {
            decode_lsp(json!({ "item": call_item_value(item) }))
                .map(LspSemanticRequest::IncomingCalls)
        }
        SemanticRequest::OutgoingCalls { item } => {
            decode_lsp(json!({ "item": call_item_value(item) }))
                .map(LspSemanticRequest::OutgoingCalls)
        }
        SemanticRequest::SignatureHelp {
            document_uri,
            position,
        } => decode_lsp(position_params(document_uri, *position))
            .map(LspSemanticRequest::SignatureHelp),
        SemanticRequest::PrepareTypeHierarchy {
            document_uri,
            position,
        } => decode_lsp(position_params(document_uri, *position))
            .map(LspSemanticRequest::PrepareTypeHierarchy),
        SemanticRequest::TypeHierarchySupertypes { item } => {
            decode_lsp(json!({ "item": type_item_value(item) }))
                .map(LspSemanticRequest::TypeHierarchySupertypes)
        }
        SemanticRequest::TypeHierarchySubtypes { item } => {
            decode_lsp(json!({ "item": type_item_value(item) }))
                .map(LspSemanticRequest::TypeHierarchySubtypes)
        }
        SemanticRequest::RenameCandidate {
            document_uri,
            position,
        } => decode_lsp(position_params(document_uri, *position))
            .map(LspSemanticRequest::PrepareRename),
    }
}

fn decode_lsp<T: DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|_| "semantic-request-invalid".to_owned())
}

fn project_semantic_outcome(
    root: &AdmittedRoot,
    request: &SemanticRequest,
    outcome: LspSemanticOperationOutcome,
) -> SemanticProviderOutcome<SemanticResponse> {
    match outcome {
        LspSemanticOperationOutcome::Complete(value) => {
            match parse_semantic_response(request, value) {
                Ok((value, coverage)) => {
                    let (value, outside_root) = confine_semantic_response(root, value);
                    match coverage.or_else(|| {
                        outside_root.then(|| "semantic-result-outside-admitted-root".to_owned())
                    }) {
                        Some(coverage) => SemanticProviderOutcome::Partial {
                            value,
                            coverage,
                            detail: None,
                        },
                        None => SemanticProviderOutcome::Complete(value),
                    }
                }
                Err(coverage) => SemanticProviderOutcome::Partial {
                    value: empty_semantic_response(request),
                    coverage,
                    detail: None,
                },
            }
        }
        LspSemanticOperationOutcome::Partial {
            value,
            coverage,
            detail,
        } => {
            let value = parse_semantic_response(request, value)
                .map_or_else(|_| empty_semantic_response(request), |(value, _)| value);
            let (value, _) = confine_semantic_response(root, value);
            SemanticProviderOutcome::Partial {
                value,
                coverage,
                detail: detail.map(str::to_owned),
            }
        }
        LspSemanticOperationOutcome::RenameCandidate(value) => {
            SemanticProviderOutcome::Complete(SemanticResponse::RenameCandidate(value))
        }
        LspSemanticOperationOutcome::Unavailable => SemanticProviderOutcome::Unavailable,
    }
}

fn confine_semantic_response(
    root: &AdmittedRoot,
    response: SemanticResponse,
) -> (SemanticResponse, bool) {
    match response {
        SemanticResponse::Locations(mut values) => {
            let before = values.len();
            values.retain(|value| root.contains_document(&value.uri));
            let omitted = before != values.len();
            (SemanticResponse::Locations(values), omitted)
        }
        SemanticResponse::WorkspaceSymbols(mut values) => {
            let before = values.len();
            values.retain(|value| root.contains_document(&value.location.uri));
            let omitted = before != values.len();
            (SemanticResponse::WorkspaceSymbols(values), omitted)
        }
        SemanticResponse::CallHierarchyItems(mut values) => {
            let before = values.len();
            values.retain(|value| root.contains_document(&value.uri));
            let omitted = before != values.len();
            (SemanticResponse::CallHierarchyItems(values), omitted)
        }
        SemanticResponse::IncomingCalls(mut values) => {
            let before = values.len();
            values.retain(|value| root.contains_document(&value.from.uri));
            let omitted = before != values.len();
            (SemanticResponse::IncomingCalls(values), omitted)
        }
        SemanticResponse::OutgoingCalls(mut values) => {
            let before = values.len();
            values.retain(|value| root.contains_document(&value.to.uri));
            let omitted = before != values.len();
            (SemanticResponse::OutgoingCalls(values), omitted)
        }
        SemanticResponse::TypeHierarchyItems(mut values) => {
            let before = values.len();
            values.retain(|value| root.contains_document(&value.uri));
            let omitted = before != values.len();
            (SemanticResponse::TypeHierarchyItems(values), omitted)
        }
        SemanticResponse::RenameCandidate(RenameCandidateResult::Available(candidate))
            if !root.contains_document(&candidate.document_uri) =>
        {
            (
                SemanticResponse::RenameCandidate(RenameCandidateResult::Unavailable {
                    reason: RenameCandidateUnavailableReason::AmbiguousEvidence,
                }),
                true,
            )
        }
        response => (response, false),
    }
}

fn parse_semantic_response(
    request: &SemanticRequest,
    value: Value,
) -> Result<(SemanticResponse, Option<String>), String> {
    match request {
        SemanticRequest::Declaration { .. }
        | SemanticRequest::Definition { .. }
        | SemanticRequest::TypeDefinition { .. }
        | SemanticRequest::Implementation { .. }
        | SemanticRequest::References { .. } => {
            Ok((SemanticResponse::Locations(parse_locations(value)?), None))
        }
        SemanticRequest::Hover { .. } => Ok((SemanticResponse::Hover(parse_hover(value)?), None)),
        SemanticRequest::DocumentSymbols { .. } => {
            let (symbols, partial) = parse_document_symbols(value)?;
            Ok((
                SemanticResponse::DocumentSymbols(symbols),
                partial.then(|| "document-symbols-unprojectable-items".to_owned()),
            ))
        }
        SemanticRequest::WorkspaceSymbols { .. } => {
            let (symbols, partial) = parse_workspace_symbols(value)?;
            Ok((
                SemanticResponse::WorkspaceSymbols(symbols),
                partial.then(|| "workspace-symbols-unresolved-locations".to_owned()),
            ))
        }
        SemanticRequest::PrepareCallHierarchy { .. } => Ok((
            SemanticResponse::CallHierarchyItems(parse_call_items(value)?),
            None,
        )),
        SemanticRequest::IncomingCalls { .. } => Ok((
            SemanticResponse::IncomingCalls(parse_incoming_calls(value)?),
            None,
        )),
        SemanticRequest::OutgoingCalls { .. } => Ok((
            SemanticResponse::OutgoingCalls(parse_outgoing_calls(value)?),
            None,
        )),
        SemanticRequest::SignatureHelp { .. } => Ok((
            SemanticResponse::SignatureHelp(parse_signature_help(value)?),
            None,
        )),
        SemanticRequest::PrepareTypeHierarchy { .. }
        | SemanticRequest::TypeHierarchySupertypes { .. }
        | SemanticRequest::TypeHierarchySubtypes { .. } => Ok((
            SemanticResponse::TypeHierarchyItems(parse_type_items(value)?),
            None,
        )),
        SemanticRequest::RenameCandidate { .. } => Err("rename-candidate-unmerged".to_owned()),
    }
}

fn empty_semantic_response(request: &SemanticRequest) -> SemanticResponse {
    match request {
        SemanticRequest::Declaration { .. }
        | SemanticRequest::Definition { .. }
        | SemanticRequest::TypeDefinition { .. }
        | SemanticRequest::Implementation { .. }
        | SemanticRequest::References { .. } => SemanticResponse::Locations(Vec::new()),
        SemanticRequest::Hover { .. } => SemanticResponse::Hover(None),
        SemanticRequest::DocumentSymbols { .. } => SemanticResponse::DocumentSymbols(Vec::new()),
        SemanticRequest::WorkspaceSymbols { .. } => SemanticResponse::WorkspaceSymbols(Vec::new()),
        SemanticRequest::PrepareCallHierarchy { .. } => {
            SemanticResponse::CallHierarchyItems(Vec::new())
        }
        SemanticRequest::IncomingCalls { .. } => SemanticResponse::IncomingCalls(Vec::new()),
        SemanticRequest::OutgoingCalls { .. } => SemanticResponse::OutgoingCalls(Vec::new()),
        SemanticRequest::SignatureHelp { .. } => SemanticResponse::SignatureHelp(None),
        SemanticRequest::PrepareTypeHierarchy { .. }
        | SemanticRequest::TypeHierarchySupertypes { .. }
        | SemanticRequest::TypeHierarchySubtypes { .. } => {
            SemanticResponse::TypeHierarchyItems(Vec::new())
        }
        SemanticRequest::RenameCandidate { .. } => {
            SemanticResponse::RenameCandidate(RenameCandidateResult::Unavailable {
                reason: RenameCandidateUnavailableReason::EvidenceAbsent,
            })
        }
    }
}

fn parse_locations(value: Value) -> Result<Vec<LspLocation>, String> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    let values = match value {
        Value::Array(values) => values,
        value @ Value::Object(_) => vec![value],
        _ => return Err("semantic-location-response-invalid".to_owned()),
    };
    values
        .into_iter()
        .map(|value| {
            let uri = value
                .get("uri")
                .or_else(|| value.get("targetUri"))
                .and_then(Value::as_str)
                .ok_or_else(|| "semantic-location-uri-invalid".to_owned())?;
            let range = value
                .get("range")
                .or_else(|| value.get("targetRange"))
                .ok_or_else(|| "semantic-location-range-invalid".to_owned())?;
            Ok(LspLocation {
                uri: uri.to_owned(),
                range: parse_range(range)?,
            })
        })
        .collect()
}

fn parse_hover(value: Value) -> Result<Option<Hover>, String> {
    if value.is_null() {
        return Ok(None);
    }
    let contents = value
        .get("contents")
        .map(hover_contents)
        .ok_or_else(|| "semantic-hover-contents-invalid".to_owned())?;
    let range = value.get("range").map(parse_range).transpose()?;
    Ok(Some(Hover { contents, range }))
}

fn hover_contents(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Array(values) => values
            .iter()
            .map(hover_contents)
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        Value::Object(object) => object
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        _ => String::new(),
    }
}

fn parse_document_symbols(value: Value) -> Result<(Vec<DocumentSymbol>, bool), String> {
    if value.is_null() {
        return Ok((Vec::new(), false));
    }
    let values = value
        .as_array()
        .ok_or_else(|| "semantic-document-symbols-invalid".to_owned())?;
    let mut partial = false;
    let symbols = values
        .iter()
        .filter_map(|value| {
            if let Ok(symbol) = parse_document_symbol(value) {
                Some(symbol)
            } else {
                partial = true;
                None
            }
        })
        .collect();
    Ok((symbols, partial))
}

fn parse_document_symbol(value: &Value) -> Result<DocumentSymbol, String> {
    let range_value = value
        .get("range")
        .or_else(|| {
            value
                .get("location")
                .and_then(|location| location.get("range"))
        })
        .ok_or_else(|| "semantic-document-symbol-range-invalid".to_owned())?;
    let range = parse_range(range_value)?;
    let children = value
        .get("children")
        .and_then(Value::as_array)
        .map(|children| {
            children
                .iter()
                .map(parse_document_symbol)
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    Ok(DocumentSymbol {
        name: required_string(value, "name")?,
        kind: required_u32(value, "kind")?,
        range,
        selection_range: value
            .get("selectionRange")
            .map(parse_range)
            .transpose()?
            .unwrap_or(range),
        children,
    })
}

fn parse_workspace_symbols(value: Value) -> Result<(Vec<WorkspaceSymbol>, bool), String> {
    if value.is_null() {
        return Ok((Vec::new(), false));
    }
    let values = value
        .as_array()
        .ok_or_else(|| "semantic-workspace-symbols-invalid".to_owned())?;
    let mut partial = false;
    let symbols = values
        .iter()
        .filter_map(|value| {
            if let Ok(symbol) = parse_workspace_symbol(value) {
                Some(symbol)
            } else {
                partial = true;
                None
            }
        })
        .collect();
    Ok((symbols, partial))
}

fn parse_workspace_symbol(value: &Value) -> Result<WorkspaceSymbol, String> {
    let location = value
        .get("location")
        .ok_or_else(|| "semantic-workspace-symbol-location-invalid".to_owned())?;
    Ok(WorkspaceSymbol {
        name: required_string(value, "name")?,
        kind: required_u32(value, "kind")?,
        location: LspLocation {
            uri: required_string(location, "uri")?,
            range: parse_range(
                location
                    .get("range")
                    .ok_or_else(|| "semantic-workspace-symbol-range-unresolved".to_owned())?,
            )?,
        },
    })
}

fn parse_call_items(value: Value) -> Result<Vec<CallHierarchyItem>, String> {
    nullable_array(value, parse_call_item)
}

fn parse_call_item(value: &Value) -> Result<CallHierarchyItem, String> {
    Ok(CallHierarchyItem {
        name: required_string(value, "name")?,
        kind: required_u32(value, "kind")?,
        uri: required_string(value, "uri")?,
        range: parse_required_range(value, "range")?,
        selection_range: parse_required_range(value, "selectionRange")?,
    })
}

fn parse_incoming_calls(value: Value) -> Result<Vec<IncomingCall>, String> {
    nullable_array(value, |value| {
        Ok(IncomingCall {
            from: parse_call_item(
                value
                    .get("from")
                    .ok_or_else(|| "semantic-incoming-call-from-invalid".to_owned())?,
            )?,
            from_ranges: parse_ranges(value, "fromRanges")?,
        })
    })
}

fn parse_outgoing_calls(value: Value) -> Result<Vec<OutgoingCall>, String> {
    nullable_array(value, |value| {
        Ok(OutgoingCall {
            to: parse_call_item(
                value
                    .get("to")
                    .ok_or_else(|| "semantic-outgoing-call-to-invalid".to_owned())?,
            )?,
            from_ranges: parse_ranges(value, "fromRanges")?,
        })
    })
}

fn parse_signature_help(value: Value) -> Result<Option<SignatureHelp>, String> {
    if value.is_null() {
        return Ok(None);
    }
    let signatures = value
        .get("signatures")
        .and_then(Value::as_array)
        .ok_or_else(|| "semantic-signature-help-invalid".to_owned())?
        .iter()
        .map(|signature| required_string(signature, "label"))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(SignatureHelp {
        signatures,
        active_signature: optional_u32(&value, "activeSignature")?,
        active_parameter: optional_u32(&value, "activeParameter")?,
    }))
}

fn parse_type_items(value: Value) -> Result<Vec<TypeHierarchyItem>, String> {
    nullable_array(value, |value| {
        Ok(TypeHierarchyItem {
            name: required_string(value, "name")?,
            kind: required_u32(value, "kind")?,
            uri: required_string(value, "uri")?,
            range: parse_required_range(value, "range")?,
            selection_range: parse_required_range(value, "selectionRange")?,
        })
    })
}

fn nullable_array<T>(
    value: Value,
    parse: impl Fn(&Value) -> Result<T, String>,
) -> Result<Vec<T>, String> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    value
        .as_array()
        .ok_or_else(|| "semantic-array-response-invalid".to_owned())?
        .iter()
        .map(parse)
        .collect()
}

fn parse_ranges(value: &Value, field: &str) -> Result<Vec<LspRange>, String> {
    value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| "semantic-ranges-invalid".to_owned())?
        .iter()
        .map(parse_range)
        .collect()
}

fn parse_required_range(value: &Value, field: &str) -> Result<LspRange, String> {
    parse_range(
        value
            .get(field)
            .ok_or_else(|| "semantic-range-invalid".to_owned())?,
    )
}

fn parse_range(value: &Value) -> Result<LspRange, String> {
    Ok(LspRange {
        start: parse_position(
            value
                .get("start")
                .ok_or_else(|| "semantic-range-start-invalid".to_owned())?,
        )?,
        end: parse_position(
            value
                .get("end")
                .ok_or_else(|| "semantic-range-end-invalid".to_owned())?,
        )?,
    })
}

fn parse_position(value: &Value) -> Result<LspPosition, String> {
    Ok(LspPosition {
        line: required_u32(value, "line")?,
        character: required_u32(value, "character")?,
    })
}

fn required_string(value: &Value, field: &str) -> Result<String, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("semantic-{field}-invalid"))
}

fn required_u32(value: &Value, field: &str) -> Result<u32, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| format!("semantic-{field}-invalid"))
}

fn optional_u32(value: &Value, field: &str) -> Result<Option<u32>, String> {
    value
        .get(field)
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| format!("semantic-{field}-invalid"))
        })
        .transpose()
}

fn position_value(position: LspPosition) -> Value {
    json!({ "line": position.line, "character": position.character })
}

fn range_value(range: LspRange) -> Value {
    json!({
        "start": position_value(range.start),
        "end": position_value(range.end),
    })
}

fn call_item_value(item: &CallHierarchyItem) -> Value {
    json!({
        "name": item.name,
        "kind": item.kind,
        "uri": item.uri,
        "range": range_value(item.range),
        "selectionRange": range_value(item.selection_range),
    })
}

fn type_item_value(item: &TypeHierarchyItem) -> Value {
    json!({
        "name": item.name,
        "kind": item.kind,
        "uri": item.uri,
        "range": range_value(item.range),
        "selectionRange": range_value(item.selection_range),
    })
}

/// Cancellation authority supplied by the actual graph/analyzer/application
/// operation owner. The admitted root keeps shared provider bundles from
/// cancelling a same-id request in another root.
pub trait LspAnalyzerCancellationAuthority: Send + Sync {
    fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool;
}

impl<T> LspAnalyzerCancellationAuthority for Arc<T>
where
    T: LspAnalyzerCancellationAuthority + ?Sized,
{
    fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        (**self).cancel_request(root, request_id)
    }
}

pub struct Pr12AnalyzerCancellationAdapter {
    authority: Arc<dyn LspAnalyzerCancellationAuthority>,
}

impl Pr12AnalyzerCancellationAdapter {
    pub fn new(authority: Arc<dyn LspAnalyzerCancellationAuthority>) -> Self {
        Self { authority }
    }
}

impl AnalyzerCancellationPort for Pr12AnalyzerCancellationAdapter {
    fn cancel_upstream(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        self.authority.cancel_request(root, request_id)
    }
}

/// Canonical application boundary for the four versioned PR12 projections.
pub trait CanonicalContextProjectionAuthority: Send + Sync {
    fn registrations(&self) -> Vec<ContextProjectionRegistration>;

    fn snapshot(
        &self,
        root: AdmittedRoot,
        request_id: LspRequestId,
        request: ContextProjectionRequest,
    ) -> LspRuntimeFuture<ContextProjectionOutcome>;

    fn expand(
        &self,
        _root: AdmittedRoot,
        _request_id: LspRequestId,
        _request: ContextExpansionRequest,
    ) -> LspRuntimeFuture<ContextExpansionOutcome> {
        Box::pin(async { ContextExpansionOutcome::Denied })
    }

    fn cancel_request(&self, _root: &AdmittedRoot, _request_id: &LspRequestId) -> bool {
        false
    }

    fn poll_changes(
        &self,
        _root: &AdmittedRoot,
        _subscriptions: &std::collections::BTreeSet<ContextProjectionRegistration>,
    ) -> Vec<ContextProjectionChange> {
        Vec::new()
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ContextRequestKey {
    root_uri: String,
    request_id: LspRequestId,
}

struct PendingContextOperation {
    receiver: oneshot::Receiver<ContextProjectionOutcome>,
    abort: AbortHandle,
}

struct PendingContextExpansion {
    receiver: oneshot::Receiver<ContextExpansionOutcome>,
    abort: AbortHandle,
}

/// Async-capable `ContextProjectionPort` without a blocking runtime bridge.
/// Its in-flight map is bounded by session capacity and contains only one-shot
/// response correlation, never feedback or evidence truth.
pub struct Pr12ContextProjectionAdapter {
    runtime: Handle,
    authority: Arc<dyn CanonicalContextProjectionAuthority>,
    in_flight: Mutex<BTreeMap<ContextRequestKey, PendingContextOperation>>,
    expansions: Mutex<BTreeMap<ContextRequestKey, PendingContextExpansion>>,
    delivered_changes:
        Mutex<BTreeMap<(String, Option<String>, ContextProjectionKind), ContextProjectionChange>>,
}

impl Pr12ContextProjectionAdapter {
    pub fn new(runtime: Handle, authority: Arc<dyn CanonicalContextProjectionAuthority>) -> Self {
        Self {
            runtime,
            authority,
            in_flight: Mutex::new(BTreeMap::new()),
            expansions: Mutex::new(BTreeMap::new()),
            delivered_changes: Mutex::new(BTreeMap::new()),
        }
    }

    fn key(root: &AdmittedRoot, request_id: &LspRequestId) -> ContextRequestKey {
        ContextRequestKey {
            root_uri: root.uri().to_owned(),
            request_id: request_id.clone(),
        }
    }
}

impl ContextProjectionPort for Pr12ContextProjectionAdapter {
    fn registrations(&self) -> Vec<ContextProjectionRegistration> {
        self.authority.registrations()
    }

    fn snapshot(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
        request: &ContextProjectionRequest,
    ) -> ContextProjectionOutcome {
        let key = Self::key(root, request_id);
        let Ok(mut in_flight) = self.in_flight.try_lock() else {
            return ContextProjectionOutcome::Deferred {
                reason: "runtime-busy".to_owned(),
            };
        };
        if in_flight.contains_key(&key) {
            return ContextProjectionOutcome::Pending;
        }
        let expansion_count = self
            .expansions
            .try_lock()
            .map_or(MAX_PR12_CONTEXT_OPERATIONS, |expansions| expansions.len());
        if in_flight.len().saturating_add(expansion_count) >= MAX_PR12_CONTEXT_OPERATIONS {
            return ContextProjectionOutcome::Deferred {
                reason: "context-projection-capacity".to_owned(),
            };
        }
        let authority = Arc::clone(&self.authority);
        let (sender, receiver) = oneshot::channel();
        let root = root.clone();
        let request_id = request_id.clone();
        let request = request.clone();
        let abort = self
            .runtime
            .spawn(async move {
                let _ = sender.send(authority.snapshot(root, request_id, request).await);
            })
            .abort_handle();
        in_flight.insert(key, PendingContextOperation { receiver, abort });
        ContextProjectionOutcome::Pending
    }

    fn poll_snapshot(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
    ) -> Option<ContextProjectionOutcome> {
        let key = Self::key(root, request_id);
        let Ok(mut in_flight) = self.in_flight.try_lock() else {
            return None;
        };
        match in_flight
            .get_mut(&key)
            .map(|pending| pending.receiver.try_recv())
        {
            Some(Ok(outcome)) => {
                in_flight.remove(&key);
                Some(outcome)
            }
            Some(Err(TryRecvError::Empty)) | None => None,
            Some(Err(TryRecvError::Closed)) => {
                in_flight.remove(&key);
                Some(ContextProjectionOutcome::Failed {
                    reason: "context-operation-dropped".to_owned(),
                })
            }
        }
    }

    fn expand(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
        request: &ContextExpansionRequest,
    ) -> ContextExpansionOutcome {
        let key = Self::key(root, request_id);
        let Ok(mut expansions) = self.expansions.try_lock() else {
            return ContextExpansionOutcome::Failed {
                reason: "runtime-busy".to_owned(),
            };
        };
        if expansions.contains_key(&key) {
            return ContextExpansionOutcome::Pending;
        }
        let projection_count = self
            .in_flight
            .try_lock()
            .map_or(MAX_PR12_CONTEXT_OPERATIONS, |in_flight| in_flight.len());
        if projection_count.saturating_add(expansions.len()) >= MAX_PR12_CONTEXT_OPERATIONS {
            return ContextExpansionOutcome::Failed {
                reason: "context-expansion-capacity".to_owned(),
            };
        }
        let authority = Arc::clone(&self.authority);
        let (sender, receiver) = oneshot::channel();
        let root = root.clone();
        let request_id = request_id.clone();
        let request = request.clone();
        let abort = self
            .runtime
            .spawn(async move {
                let _ = sender.send(authority.expand(root, request_id, request).await);
            })
            .abort_handle();
        expansions.insert(key, PendingContextExpansion { receiver, abort });
        ContextExpansionOutcome::Pending
    }

    fn poll_expansion(
        &self,
        root: &AdmittedRoot,
        request_id: &LspRequestId,
    ) -> Option<ContextExpansionOutcome> {
        let key = Self::key(root, request_id);
        let Ok(mut expansions) = self.expansions.try_lock() else {
            return None;
        };
        match expansions
            .get_mut(&key)
            .map(|pending| pending.receiver.try_recv())
        {
            Some(Ok(outcome)) => {
                expansions.remove(&key);
                Some(outcome)
            }
            Some(Err(TryRecvError::Empty)) | None => None,
            Some(Err(TryRecvError::Closed)) => {
                expansions.remove(&key);
                Some(ContextExpansionOutcome::Failed {
                    reason: "context-expansion-operation-dropped".to_owned(),
                })
            }
        }
    }

    fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        let key = Self::key(root, request_id);
        let cancelled_projection = self
            .in_flight
            .try_lock()
            .ok()
            .and_then(|mut in_flight| in_flight.remove(&key))
            .is_some_and(|pending| {
                pending.abort.abort();
                true
            });
        let cancelled_expansion = self
            .expansions
            .try_lock()
            .ok()
            .and_then(|mut expansions| expansions.remove(&key))
            .is_some_and(|pending| {
                pending.abort.abort();
                true
            });
        self.authority.cancel_request(root, request_id)
            || cancelled_projection
            || cancelled_expansion
    }

    fn poll_changes(
        &self,
        root: &AdmittedRoot,
        subscriptions: &std::collections::BTreeSet<ContextProjectionRegistration>,
    ) -> Vec<ContextProjectionChange> {
        let changes = self.authority.poll_changes(root, subscriptions);
        let Ok(mut delivered) = self.delivered_changes.try_lock() else {
            return Vec::new();
        };
        changes
            .into_iter()
            .filter(|change| {
                let key = (
                    change.root_uri.clone(),
                    change.document_uri.clone(),
                    change.kind.clone(),
                );
                !matches!(delivered.insert(key, change.clone()), Some(previous) if previous == *change)
            })
            .collect()
    }

    fn update_subscriptions(
        &self,
        root: &AdmittedRoot,
        subscriptions: &std::collections::BTreeSet<ContextProjectionRegistration>,
    ) {
        if let Ok(mut delivered) = self.delivered_changes.try_lock() {
            delivered.retain(|(root_uri, _, kind), _| {
                root_uri != root.uri()
                    || subscriptions.contains(&ContextProjectionRegistration {
                        kind: kind.clone(),
                        revision: TRACEDECAY_CONTEXT_REVISION,
                    })
            });
        }
    }
}

impl Drop for Pr12ContextProjectionAdapter {
    fn drop(&mut self) {
        if let Ok(in_flight) = self.in_flight.get_mut() {
            for pending in in_flight.values() {
                pending.abort.abort();
            }
        }
        if let Ok(expansions) = self.expansions.get_mut() {
            for pending in expansions.values() {
                pending.abort.abort();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;
    use std::collections::BTreeSet;
    use std::sync::atomic::AtomicBool;
    use tracedecay_lsp::{
        ContextCoverage, ContextExpansionEnvelope, ContextExpansionScope, ContextFreshness,
        ContextProducerState, ContextProjectionEnvelope, ContextProjectionIdentity,
    };

    /// `tracedecay-lsp` serializes whatever detail this adapter supplies
    /// verbatim, so this allowlist is the only thing keeping analyzer, graph,
    /// and caller text off the wire. Every template must stay a fixed
    /// operator-readable sentence with no identity, path, or transport text.
    #[test]
    fn semantic_failure_detail_templates_are_static_and_leak_nothing() {
        let templates = [
            LspSemanticOperationOutcome::ANALYZER_START_FAILED_DETAIL,
            LspSemanticOperationOutcome::ANALYZER_CANCELLED_DETAIL,
            LspSemanticOperationOutcome::ANALYZER_TIMEOUT_DETAIL,
            LspSemanticOperationOutcome::ANALYZER_REMOTE_ERROR_DETAIL,
            LspSemanticOperationOutcome::ANALYZER_TRANSPORT_FAILED_DETAIL,
            LspSemanticOperationOutcome::ANALYZER_INVALID_RESPONSE_DETAIL,
            LspSemanticOperationOutcome::GRAPH_READ_FAILED_DETAIL,
        ];
        assert_eq!(
            templates.iter().collect::<BTreeSet<_>>().len(),
            templates.len(),
            "each failure class needs its own distinguishable template"
        );
        for detail in templates {
            let serialized = serde_json::to_string(&json!({ "detail": detail }))
                .expect("serialized error data");
            for forbidden in [
                "bearer-secret",
                "YWxpY2U6c2VjcmV0",
                "alice:hunter2",
                "bob:password",
                "file://",
                "/home/alice",
                r"C:\Users\alice",
                "密碼",
                "🔐",
                "\\n",
            ] {
                assert!(
                    !serialized.contains(forbidden),
                    "failure detail template leaked {forbidden}"
                );
            }
            assert!(
                !detail.is_empty()
                    && detail.len() <= 96
                    && detail.is_ascii()
                    && !detail.chars().any(char::is_control),
                "failure detail template must stay a short static sentence: {detail}"
            );
        }
    }

    struct Diagnostics;

    impl CanonicalDiagnosticSnapshotAuthority for Diagnostics {
        fn refresh(
            &self,
            _request: CanonicalDiagnosticRefreshRequest,
        ) -> LspRuntimeFuture<Result<GenerationDiagnostics, LspRuntimeFailure>> {
            Box::pin(async {
                Ok(GenerationDiagnostics {
                    generation: 7,
                    upstream: Vec::new(),
                    tracedecay: Vec::new(),
                })
            })
        }
    }

    struct Context;

    fn fixture_projection_identity(document_scoped: bool) -> ContextProjectionIdentity {
        ContextProjectionIdentity {
            head_commit_id: "0123456789abcdef".to_owned(),
            code_generation_id: "generation:7".to_owned(),
            snapshot_digest: format!("sha256:{}", "a".repeat(64)),
            invalidation_digest: format!("sha256:{}", "b".repeat(64)),
            snapshot_content_digest: format!("sha256:{}", "c".repeat(64)),
            document_content_digest: document_scoped.then(|| format!("sha256:{}", "d".repeat(64))),
        }
    }

    impl CanonicalContextProjectionAuthority for Context {
        fn registrations(&self) -> Vec<ContextProjectionRegistration> {
            vec![ContextProjectionRegistration {
                kind: ContextProjectionKind::diagnostics(),
                revision: 1,
            }]
        }

        fn snapshot(
            &self,
            root: AdmittedRoot,
            _request_id: LspRequestId,
            request: ContextProjectionRequest,
        ) -> LspRuntimeFuture<ContextProjectionOutcome> {
            Box::pin(async move {
                let identity = fixture_projection_identity(request.document_uri.is_some());
                ContextProjectionOutcome::Ready(ContextProjectionEnvelope {
                    root_uri: root.uri().to_owned(),
                    document_uri: request.document_uri,
                    kind: request.kind,
                    generation: 7,
                    identity,
                    freshness: ContextFreshness::Current,
                    producer_state: ContextProducerState::Complete,
                    coverage: ContextCoverage::Complete,
                    revision: 1,
                    items: Vec::new(),
                    omitted_count: 0,
                    omission_reasons: Vec::new(),
                    retrieval_handle: None,
                })
            })
        }

        fn expand(
            &self,
            root: AdmittedRoot,
            _request_id: LspRequestId,
            _request: ContextExpansionRequest,
        ) -> LspRuntimeFuture<ContextExpansionOutcome> {
            Box::pin(async move {
                ContextExpansionOutcome::Ready(ContextExpansionEnvelope {
                    root_uri: root.uri().to_owned(),
                    document_uri: None,
                    kind: ContextProjectionKind::diagnostics(),
                    stable_id: "finding.1".to_owned(),
                    generation: 7,
                    scope: ContextExpansionScope {
                        scope_digest: "sha256:scope".to_owned(),
                        identity: fixture_projection_identity(false),
                    },
                    expires_at: 10_000,
                    coverage: ContextCoverage::Complete,
                    revision: 1,
                    evidence: Some(json!({ "canonical": true })),
                    omission_reason: None,
                    next_retrieval_handle: None,
                })
            })
        }

        fn poll_changes(
            &self,
            root: &AdmittedRoot,
            subscriptions: &std::collections::BTreeSet<ContextProjectionRegistration>,
        ) -> Vec<ContextProjectionChange> {
            let registration = ContextProjectionRegistration {
                kind: ContextProjectionKind::diagnostics(),
                revision: TRACEDECAY_CONTEXT_REVISION,
            };
            subscriptions
                .contains(&registration)
                .then(|| ContextProjectionChange {
                    root_uri: root.uri().to_owned(),
                    document_uri: Some("file:///root/a.rs".to_owned()),
                    kind: ContextProjectionKind::diagnostics(),
                    generation: 7,
                    identity: fixture_projection_identity(true),
                    freshness: ContextFreshness::Current,
                    producer_state: ContextProducerState::Complete,
                    coverage: ContextCoverage::Complete,
                    revision: TRACEDECAY_CONTEXT_REVISION,
                    retrieval_handle: None,
                })
                .into_iter()
                .collect()
        }
    }

    struct Semantic {
        cancelled: Arc<AtomicBool>,
    }

    impl LspSemanticRequestAuthority for Semantic {
        fn start(
            &self,
            _root: AdmittedRoot,
            _request_id: LspRequestId,
            _request: LspSemanticRequest,
        ) -> LspRuntimeFuture<LspSemanticOperationOutcome> {
            Box::pin(async {
                LspSemanticOperationOutcome::Complete(json!([{
                    "uri": "file:///root/lib.rs",
                    "range": {
                        "start": { "line": 3, "character": 1 },
                        "end": { "line": 3, "character": 4 },
                    },
                }, {
                    "uri": "file:///outside/lib.rs",
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": { "line": 0, "character": 1 },
                    },
                }]))
            })
        }

        fn cancel_request(&self, _root: &AdmittedRoot, _request_id: &LspRequestId) -> bool {
            self.cancelled
                .store(true, std::sync::atomic::Ordering::Release);
            true
        }
    }

    fn root() -> AdmittedRoot {
        AdmittedRoot::new("file:///root")
    }

    #[tokio::test]
    async fn diagnostic_refresh_is_polled_without_blocking_the_session() {
        let adapter = Pr12DiagnosticSnapshotAdapter::new(Handle::current(), Arc::new(Diagnostics));
        let root = root();
        let expected = DiagnosticRefreshAdmission::Started(DiagnosticRefreshIdentity {
            operation_id: "lsp-diagnostic-1".to_owned(),
            source_generation: None,
            target_generation: None,
        });
        assert_eq!(
            adapter.request_document_refresh(&root, "file:///root/a.rs", None, None),
            expected
        );
        assert!(matches!(
            adapter.document_diagnostics(&root, "file:///root/a.rs", None),
            DiagnosticSnapshotOutcome::Refreshing(_)
        ));
        for _ in 0..4 {
            tokio::task::yield_now().await;
            if let DiagnosticSnapshotOutcome::Ready {
                diagnostics,
                completed_operation_id,
            } = adapter.document_diagnostics(&root, "file:///root/a.rs", None)
            {
                assert_eq!(diagnostics.generation, 7);
                assert_eq!(completed_operation_id.as_deref(), Some("lsp-diagnostic-1"));
                return;
            }
        }
        panic!("diagnostic operation did not complete");
    }

    #[tokio::test]
    async fn context_projection_is_correlated_by_admitted_root_and_request() {
        let adapter = Pr12ContextProjectionAdapter::new(Handle::current(), Arc::new(Context));
        let root = root();
        let request_id = LspRequestId::Number(4);
        let request = ContextProjectionRequest::new(
            ContextProjectionKind::diagnostics(),
            Some("file:///root/a.rs".to_owned()),
        );
        assert_eq!(
            adapter.snapshot(&root, &request_id, &request),
            ContextProjectionOutcome::Pending
        );
        for _ in 0..4 {
            tokio::task::yield_now().await;
            if let Some(ContextProjectionOutcome::Ready(envelope)) =
                adapter.poll_snapshot(&root, &request_id)
            {
                assert_eq!(envelope.generation, 7);
                return;
            }
        }
        panic!("context operation did not complete");
    }

    #[tokio::test]
    async fn context_change_delivery_is_isolated_coalesced_and_reset_on_unsubscribe() {
        let authority = Arc::new(Context);
        let first = Pr12ContextProjectionAdapter::new(Handle::current(), authority.clone());
        let second = Pr12ContextProjectionAdapter::new(Handle::current(), authority);
        let root = root();
        let subscriptions = [ContextProjectionRegistration {
            kind: ContextProjectionKind::diagnostics(),
            revision: TRACEDECAY_CONTEXT_REVISION,
        }]
        .into_iter()
        .collect();

        first.update_subscriptions(&root, &subscriptions);
        second.update_subscriptions(&root, &subscriptions);
        assert_eq!(first.poll_changes(&root, &subscriptions).len(), 1);
        assert_eq!(second.poll_changes(&root, &subscriptions).len(), 1);
        assert!(first.poll_changes(&root, &subscriptions).is_empty());
        assert!(second.poll_changes(&root, &subscriptions).is_empty());

        first.update_subscriptions(&root, &Default::default());
        first.update_subscriptions(&root, &subscriptions);
        assert_eq!(first.poll_changes(&root, &subscriptions).len(), 1);
        assert!(second.poll_changes(&root, &subscriptions).is_empty());
    }

    #[tokio::test]
    async fn context_expansion_is_correlated_without_storing_evidence_in_the_adapter() {
        let adapter = Pr12ContextProjectionAdapter::new(Handle::current(), Arc::new(Context));
        let root = root();
        let request_id = LspRequestId::Number(5);
        let request = ContextExpansionRequest {
            retrieval_handle: "rh_0123456789abcdef01234567".to_owned(),
        };
        assert_eq!(
            adapter.expand(&root, &request_id, &request),
            ContextExpansionOutcome::Pending
        );
        for _ in 0..4 {
            tokio::task::yield_now().await;
            if let Some(ContextExpansionOutcome::Ready(envelope)) =
                adapter.poll_expansion(&root, &request_id)
            {
                assert_eq!(envelope.generation, 7);
                assert_eq!(envelope.evidence, Some(json!({ "canonical": true })));
                return;
            }
        }
        panic!("context expansion did not complete");
    }

    #[tokio::test]
    async fn semantic_requests_are_polled_without_blocking_and_cancelled_by_correlation() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let adapter = Pr12SemanticProviderAdapter::new(
            Handle::current(),
            Arc::new(Semantic {
                cancelled: Arc::clone(&cancelled),
            }),
        );
        let root = root();
        let request_id = LspRequestId::Number(8);
        let position = LspPosition {
            line: 3,
            character: 1,
        };
        let request = SemanticRequest::Definition {
            document_uri: "file:///root/lib.rs".to_owned(),
            position,
        };

        assert_eq!(
            SemanticProviderPort::request(&adapter, &root, &request_id, &request),
            SemanticProviderOutcome::Pending
        );
        assert!(adapter.cancel_request(&root, &request_id));
        assert!(cancelled.load(std::sync::atomic::Ordering::Acquire));

        let completion_id = LspRequestId::Number(9);
        assert_eq!(
            SemanticProviderPort::request(&adapter, &root, &completion_id, &request),
            SemanticProviderOutcome::Pending
        );
        for _ in 0..4 {
            tokio::task::yield_now().await;
            if let SemanticProviderOutcome::Partial {
                value: SemanticResponse::Locations(locations),
                coverage,
                ..
            } = SemanticProviderPort::request(&adapter, &root, &completion_id, &request)
            {
                assert_eq!(locations.len(), 1);
                assert_eq!(locations[0].uri, "file:///root/lib.rs");
                assert_eq!(coverage, "semantic-result-outside-admitted-root");
                return;
            }
        }
        panic!("semantic operation did not complete");
    }
}
