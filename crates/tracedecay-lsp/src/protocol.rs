//! Typed JSON-RPC 2.0 / LSP 3.17 session actor.
//!
//! The actor accepts already-authenticated, already-framed payloads from the
//! bridge. It is intentionally not a raw socket tunnel: every accepted method
//! is parsed, lifecycle-gated, root-gated, bounded, and dispatched through a
//! typed gateway/provider port.
#![allow(dead_code)] // Plan 35 daemon LSP gateway — session actor not yet serving

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::convert::Infallible;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Map, Value, json};
use tracedecay_domain::{CodeGenerationId, CommitId, ContentDigest, ManifestDigest};

use crate::bridge::{
    DaemonLspSessionTransport, FramePoll, FrameSend, LspFrame, MAX_LSP_FRAME_BYTES,
};
use crate::capabilities::{
    CapabilityAvailability, CapabilityParseError, ClientCapabilities, EffectiveCapabilities,
    GatewayCapabilities, UpstreamCapabilities, negotiate_capabilities,
};
use crate::context::{
    ContextCoverage, ContextExpansionEnvelope, ContextExpansionOutcome, ContextExpansionRequest,
    ContextFreshness, ContextProducerState, ContextProjectionChange, ContextProjectionEnvelope,
    ContextProjectionIdentity, ContextProjectionKind, ContextProjectionOutcome,
    ContextProjectionPort, ContextProjectionRegistration, ContextProjectionRequest,
    ContextSubscribeRequest, MAX_CONTEXT_CHANGES_PER_POLL, MAX_CONTEXT_PROJECTION_BYTES,
    MAX_CONTEXT_PROJECTION_ITEMS, MAX_CONTEXT_PROJECTION_KINDS, MAX_CONTEXT_RETRIEVAL_HANDLE_BYTES,
    MAX_CONTEXT_SUMMARY_BYTES, TRACEDECAY_CONTEXT_CHANGED_METHOD, TRACEDECAY_CONTEXT_EXPAND_METHOD,
    TRACEDECAY_CONTEXT_METHOD, TRACEDECAY_SUBSCRIBE_METHOD,
};
use crate::diagnostics::{
    DiagnosticMerge, DiagnosticSeverity, DiagnosticSource, DocumentDiagnosticReport,
    GatewayDiagnostic, LspPosition, LspRange, MAX_DOCUMENT_DIAGNOSTICS,
};
use crate::dispatch::{dispatch_incoming, parse_incoming};
use crate::gateway::{
    AdmittedRoot, DaemonLspGateway, FeedbackCyclePort, FeedbackCycleResponse, GatewayMethod,
    GatewayResponse, MethodUnavailableReason, SemanticProviderPort, SemanticRequest,
};
use crate::overlay::{
    DebouncedDiagnosticKind, OverlayDiagnosticDebouncer, OverlayError, OverlayStore,
};
use crate::provider::{
    AnalyzerCancellationPort, DiagnosticRefreshAdmission, DiagnosticRefreshIdentity,
    DiagnosticSnapshotOutcome, DiagnosticSnapshotPort, MAX_DIAGNOSTIC_OPERATION_ID_BYTES,
    UnavailableDiagnosticSnapshotProvider,
};
use crate::request_sequence::{ConnectionLocalRequestSequence, ProcessLocalRequestSequence};
use crate::rpc::{
    DiagnosticSerializationCapabilities, RpcFailure, diagnostic_result_id, diagnostic_value,
    document_diagnostic_report_value, error_response, initialized_root_uri, overlay_failure,
    parse_overlay_change, partial_failure_data, request_id, request_id_value, required_i64,
    required_nonempty_string, required_string, response_value, semantic_response_value,
    success_response, text_document,
};
use crate::session::{
    CancellationOutcome, CompletionDisposition, LifecycleError, LspRequestFailure, LspRequestId,
    LspSessionControl, MAX_PUBLICATION_BYTES, PublicationAdmission, SessionLifecycle,
};

/// A protocol actor allows bounded synchronous work before returning a typed
/// cancellation response. Long-running adapters receive the same deadline via
/// their daemon-owned runtime contracts.
pub const DEFAULT_LSP_REQUEST_DEADLINE_MS: u64 = 5_000;
/// Session-local queued outbound bytes. The bridge retains one additional
/// frame per direction while its peer is backpressured.
pub const MAX_QUEUED_OUTBOUND_BYTES: usize = 1024 * 1024;
pub const MAX_QUEUED_OUTBOUND_MESSAGES: usize = 64;

fn valid_context_projection_identity(identity: &ContextProjectionIdentity) -> bool {
    CommitId::new(identity.head_commit_id.clone()).is_ok()
        && CodeGenerationId::new(identity.code_generation_id.clone()).is_ok()
        && ManifestDigest::new(identity.snapshot_digest.clone()).is_ok()
        && ManifestDigest::new(identity.invalidation_digest.clone()).is_ok()
        && ContentDigest::new(identity.snapshot_content_digest.clone()).is_ok()
        && identity
            .document_content_digest
            .as_ref()
            .is_none_or(|digest| ContentDigest::new(digest.clone()).is_ok())
}
const MIN_CLIENT_FRAME_OUTBOUND_RESERVE: usize = MAX_PUBLICATION_BYTES;
pub(crate) const TRACEDECAY_NATIVE_DIAGNOSTICS_METHOD: &str = "tracedecay/nativeDiagnostics";
const MAX_NATIVE_DIAGNOSTIC_URI_BYTES: usize = 4 * 1024;
const MAX_NATIVE_DIAGNOSTIC_METADATA_BYTES: usize = 256;
static NEXT_CONTEXT_OPERATION_ID: ProcessLocalRequestSequence =
    ProcessLocalRequestSequence::starting_at(1);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProtocolDispatch {
    pub queued_messages: usize,
    pub backpressured: bool,
    pub closed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PublicationTag {
    uri: String,
    version: i64,
    generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct QueuedFrame {
    payload: LspFrame,
    publication: Option<PublicationTag>,
    server_request: Option<LspRequestId>,
}

#[derive(Clone, Debug)]
struct PublishedDiagnostic {
    version: i64,
    generation: u64,
}

#[derive(Clone)]
struct PendingDiagnosticRefresh {
    identity: DiagnosticRefreshIdentity,
    overlay_version: i64,
}

#[derive(Clone)]
struct PendingContextRequest {
    response_id: Value,
    operation_id: LspRequestId,
    request: ContextProjectionRequest,
}

#[derive(Clone)]
struct PendingContextExpansion {
    response_id: Value,
    operation_id: LspRequestId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ContextProjectionCurrentness {
    generation: u64,
    identity: ContextProjectionIdentity,
}

#[derive(Clone)]
struct PendingSemanticRequest {
    response_id: Value,
    request: SemanticRequest,
}

#[derive(Clone, Eq, PartialEq)]
struct NativeDiagnosticSnapshot {
    version: i64,
    diagnostics: Vec<GatewayDiagnostic>,
}

fn bind_context_document_digest(request: &mut ContextProjectionRequest, overlays: &OverlayStore) {
    request.document_content_digest = request
        .document_uri
        .as_deref()
        .and_then(|uri| overlays.snapshot(uri))
        .map(|snapshot| ContentDigest::of_bytes(snapshot.text.as_bytes()));
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeDiagnosticsNotification {
    uri: String,
    version: i64,
    diagnostics: Vec<NativeDiagnostic>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeDiagnostic {
    range: NativeRange,
    severity: Option<u8>,
    code: Option<Value>,
    source: String,
    message: String,
    data: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeRange {
    start: NativePosition,
    end: NativePosition,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativePosition {
    line: u32,
    character: u32,
}

impl NativeDiagnosticsNotification {
    fn into_snapshot(self) -> Option<(String, NativeDiagnosticSnapshot)> {
        if !valid_native_string(&self.uri, MAX_NATIVE_DIAGNOSTIC_URI_BYTES)
            || self.version < 0
            || self.diagnostics.len() > MAX_DOCUMENT_DIAGNOSTICS
        {
            return None;
        }
        let uri = self.uri;
        let diagnostics = self
            .diagnostics
            .into_iter()
            .filter(|diagnostic| !native_source_is_tracedecay(&diagnostic.source))
            .map(|diagnostic| diagnostic.into_gateway_diagnostic(&uri))
            .collect::<Option<Vec<_>>>()?;
        Some((
            uri,
            NativeDiagnosticSnapshot {
                version: self.version,
                diagnostics,
            },
        ))
    }
}

fn native_source_is_tracedecay(source: &str) -> bool {
    source
        .get(.."tracedecay".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("tracedecay"))
}

impl NativeDiagnostic {
    fn into_gateway_diagnostic(self, uri: &str) -> Option<GatewayDiagnostic> {
        if !valid_native_string(&self.source, MAX_NATIVE_DIAGNOSTIC_METADATA_BYTES)
            || !valid_native_string(
                &self.message,
                super::diagnostics::MAX_DIAGNOSTIC_MESSAGE_BYTES,
            )
            || !valid_native_diagnostic_data(self.data.as_ref())
        {
            return None;
        }
        let severity = match self.severity {
            None => None,
            Some(1) => Some(DiagnosticSeverity::Error),
            Some(2) => Some(DiagnosticSeverity::Warning),
            Some(3) => Some(DiagnosticSeverity::Information),
            Some(4) => Some(DiagnosticSeverity::Hint),
            Some(_) => return None,
        };
        let code = match self.code {
            None | Some(Value::Null) => None,
            Some(Value::String(code))
                if valid_native_string(&code, MAX_NATIVE_DIAGNOSTIC_METADATA_BYTES) =>
            {
                Some(code)
            }
            Some(Value::Number(code)) => {
                let code = code.to_string();
                valid_native_string(&code, MAX_NATIVE_DIAGNOSTIC_METADATA_BYTES).then_some(code)
            }
            Some(_) => return None,
        };
        let range = LspRange {
            start: LspPosition {
                line: self.range.start.line,
                character: self.range.start.character,
            },
            end: LspPosition {
                line: self.range.end.line,
                character: self.range.end.character,
            },
        };
        (range.start <= range.end).then_some(GatewayDiagnostic {
            uri: uri.to_owned(),
            range,
            severity,
            code,
            code_description_uri: None,
            message: self.message,
            source: DiagnosticSource::Upstream,
            related_information: Vec::new(),
            data: None,
        })
    }
}

/// One authenticated daemon LSP session. It owns no durable state and is
/// dropped alongside its registry entry after TTL expiry.
pub struct DaemonLspProtocolSession<P, S, D>
where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    pub(crate) gateway: DaemonLspGateway<P, S>,
    control: LspSessionControl,
    gateway_capabilities: GatewayCapabilities,
    upstream_capabilities: UpstreamCapabilities,
    overlays: OverlayStore,
    debounce: OverlayDiagnosticDebouncer,
    diagnostics: D,
    outbound: VecDeque<QueuedFrame>,
    outbound_in_flight: bool,
    queued_outbound_bytes: usize,
    published: BTreeMap<String, PublishedDiagnostic>,
    native_upstream: BTreeMap<String, NativeDiagnosticSnapshot>,
    cursor_native_mode: bool,
    request_deadline_ms: u64,
    next_server_request_id: ConnectionLocalRequestSequence,
    diagnostic_refresh_request: Option<LspRequestId>,
    diagnostic_refresh_needed: bool,
    active_diagnostic_refreshes: BTreeMap<String, PendingDiagnosticRefresh>,
    context: Option<Arc<dyn ContextProjectionPort + Send + Sync>>,
    context_subscriptions: BTreeSet<ContextProjectionRegistration>,
    context_currentness:
        BTreeMap<(ContextProjectionKind, Option<String>), ContextProjectionCurrentness>,
    pending_context_requests: BTreeMap<LspRequestId, PendingContextRequest>,
    pending_context_expansions: BTreeMap<LspRequestId, PendingContextExpansion>,
    pending_semantic_requests: BTreeMap<LspRequestId, PendingSemanticRequest>,
    cancellation: Option<Arc<dyn AnalyzerCancellationPort + Send + Sync>>,
}

/// Concrete bridge-facing adapter for one typed daemon session actor. It
/// parses each client payload through [`DaemonLspProtocolSession`] and exposes
/// only queued LSP frames back to the bridge; it cannot become a raw daemon
/// socket tunnel.
pub struct DaemonLspProtocolTransport<P, S, D>
where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    session: DaemonLspProtocolSession<P, S, D>,
    now_ms: u64,
}

impl<P, S, D> DaemonLspProtocolTransport<P, S, D>
where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    pub fn new(session: DaemonLspProtocolSession<P, S, D>) -> Self {
        Self { session, now_ms: 0 }
    }

    pub fn set_now_ms(&mut self, now_ms: u64) {
        self.now_ms = now_ms;
    }

    pub fn session(&self) -> &DaemonLspProtocolSession<P, S, D> {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut DaemonLspProtocolSession<P, S, D> {
        &mut self.session
    }

    pub fn into_inner(self) -> DaemonLspProtocolSession<P, S, D> {
        self.session
    }
}

impl<P, S, D> DaemonLspSessionTransport for DaemonLspProtocolTransport<P, S, D>
where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    type Error = Infallible;

    fn try_send_client_frame(&mut self, frame: &[u8]) -> Result<FrameSend, Self::Error> {
        if matches!(
            self.session.lifecycle(),
            SessionLifecycle::Exited | SessionLifecycle::Expired
        ) {
            return Ok(FrameSend::Closed);
        }
        // Do not consume a frame when the typed session cannot reserve any
        // response capacity. The bridge retains exactly one frame and retries
        // once the daemon-to-client direction makes progress.
        if !self
            .session
            .has_outbound_capacity(MIN_CLIENT_FRAME_OUTBOUND_RESERVE)
        {
            return Ok(FrameSend::Backpressured);
        }
        let dispatch = self.session.handle_payload(frame, self.now_ms);
        Ok(if dispatch.closed {
            FrameSend::Closed
        } else {
            FrameSend::Sent
        })
    }

    fn poll_daemon_frame(&mut self) -> Result<FramePoll, Self::Error> {
        if let Some(frame) = self.session.poll_outbound() {
            return Ok(FramePoll::Frame(frame.to_vec()));
        }
        if matches!(
            self.session.lifecycle(),
            SessionLifecycle::Exited | SessionLifecycle::Expired
        ) {
            Ok(FramePoll::Closed)
        } else {
            Ok(FramePoll::Pending)
        }
    }

    fn acknowledge_daemon_frame(&mut self) -> Result<(), Self::Error> {
        self.session.acknowledge_outbound();
        Ok(())
    }
}

impl<P, S> DaemonLspProtocolSession<P, S, UnavailableDiagnosticSnapshotProvider>
where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
{
    pub fn without_diagnostic_provider(
        gateway: DaemonLspGateway<P, S>,
        gateway_capabilities: GatewayCapabilities,
        upstream_capabilities: UpstreamCapabilities,
    ) -> Self {
        Self::new(
            gateway,
            gateway_capabilities,
            upstream_capabilities,
            UnavailableDiagnosticSnapshotProvider,
        )
    }
}

impl<P, S, D> DaemonLspProtocolSession<P, S, D>
where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    /// Creates a complete typed session from daemon-owned application ports.
    /// This is the central invocation integration point: no semantic or
    /// diagnostic provider is selected implicitly.
    pub fn from_ports(
        root: AdmittedRoot,
        initial_capabilities: EffectiveCapabilities,
        gateway_capabilities: GatewayCapabilities,
        upstream_capabilities: UpstreamCapabilities,
        feedback_cycle: P,
        semantic_provider: S,
        diagnostics: D,
    ) -> Self {
        Self::new(
            DaemonLspGateway::new(
                root,
                initial_capabilities,
                feedback_cycle,
                semantic_provider,
            ),
            gateway_capabilities,
            upstream_capabilities,
            diagnostics,
        )
    }

    pub fn new(
        gateway: DaemonLspGateway<P, S>,
        gateway_capabilities: GatewayCapabilities,
        upstream_capabilities: UpstreamCapabilities,
        diagnostics: D,
    ) -> Self {
        Self {
            gateway,
            control: LspSessionControl::default(),
            gateway_capabilities,
            upstream_capabilities,
            overlays: OverlayStore::default(),
            debounce: OverlayDiagnosticDebouncer::default(),
            diagnostics,
            outbound: VecDeque::new(),
            outbound_in_flight: false,
            queued_outbound_bytes: 0,
            published: BTreeMap::new(),
            native_upstream: BTreeMap::new(),
            cursor_native_mode: false,
            request_deadline_ms: DEFAULT_LSP_REQUEST_DEADLINE_MS,
            next_server_request_id: ConnectionLocalRequestSequence::starting_at(1),
            diagnostic_refresh_request: None,
            diagnostic_refresh_needed: false,
            active_diagnostic_refreshes: BTreeMap::new(),
            context: None,
            context_subscriptions: BTreeSet::new(),
            context_currentness: BTreeMap::new(),
            pending_context_requests: BTreeMap::new(),
            pending_context_expansions: BTreeMap::new(),
            pending_semantic_requests: BTreeMap::new(),
            cancellation: None,
        }
    }

    /// Mounts the daemon-owned analyzer cancellation adapter. Session
    /// cancellation remains authoritative even when the provider reports that
    /// its upstream work could not be interrupted.
    #[must_use]
    pub fn with_cancellation_port<C>(mut self, cancellation: C) -> Self
    where
        C: AnalyzerCancellationPort + Send + Sync + 'static,
    {
        self.cancellation = Some(Arc::new(cancellation));
        self
    }

    #[must_use]
    pub fn with_context_projection_port<C>(mut self, context: C) -> Self
    where
        C: ContextProjectionPort + Send + Sync + 'static,
    {
        self.context = Some(Arc::new(context));
        self
    }

    pub fn root(&self) -> &AdmittedRoot {
        self.gateway.root()
    }

    pub fn lifecycle(&self) -> SessionLifecycle {
        self.control.lifecycle()
    }

    pub fn overlays(&self) -> &OverlayStore {
        &self.overlays
    }

    pub fn set_request_deadline_ms(&mut self, deadline_ms: u64) {
        self.request_deadline_ms = deadline_ms;
    }

    pub fn cancel_request(&mut self, id: &LspRequestId) -> CancellationOutcome {
        self.cancel_request_and_upstream(id)
    }

    /// Preserves session-only state while a bridge reconnects. Publications
    /// may be redelivered after this transition; exact-once delivery is never
    /// claimed across a transport interruption.
    pub fn detach(&mut self) -> Result<(), LifecycleError> {
        self.control.detach()?;
        // A bridge-local copy may have been lost before acknowledgement. The
        // retained queue remains authoritative and is eligible for redelivery.
        self.outbound_in_flight = false;
        Ok(())
    }

    pub fn reconnect(&mut self) -> Result<(), LifecycleError> {
        self.control.reconnect()?;
        if let Some(request_id) = self.diagnostic_refresh_request.as_ref()
            && !self
                .outbound
                .iter()
                .any(|frame| frame.server_request.as_ref() == Some(request_id))
        {
            // A bridge acknowledgement may have raced the disconnect before
            // the client response arrived. Reissue one coalesced refresh; the
            // old response remains harmless and is ignored as unknown.
            self.diagnostic_refresh_request = None;
            self.queue_diagnostic_refresh();
        }
        Ok(())
    }

    /// Decodes and routes one opaque JSON-RPC payload. Responses and server
    /// notifications remain queued until a typed daemon-session transport
    /// acknowledges delivery to the bridge.
    pub fn handle_payload(&mut self, payload: &[u8], now_ms: u64) -> ProtocolDispatch {
        self.expire_requests(now_ms);
        let before = self.outbound.len();
        let backpressured_before = self.queued_outbound_bytes >= MAX_QUEUED_OUTBOUND_BYTES;
        if payload.len() > MAX_LSP_FRAME_BYTES {
            self.enqueue_value(error_response(
                Value::Null,
                RpcFailure {
                    code: -32600,
                    message: "Invalid Request",
                    data: json!({ "detail": "frame exceeds LSP limit" }),
                },
            ));
            return ProtocolDispatch {
                queued_messages: self.outbound.len().saturating_sub(before),
                backpressured: true,
                closed: false,
            };
        }
        let Ok(value) = serde_json::from_slice::<Value>(payload) else {
            self.enqueue_value(error_response(
                Value::Null,
                RpcFailure {
                    code: -32700,
                    message: "Parse error",
                    data: Value::Null,
                },
            ));
            self.flush_debounced_diagnostics(now_ms);
            return ProtocolDispatch {
                queued_messages: self.outbound.len().saturating_sub(before),
                backpressured: backpressured_before,
                closed: false,
            };
        };
        self.dispatch_value(value, now_ms);
        self.flush_debounced_diagnostics(now_ms);
        self.poll_context_requests();
        self.poll_context_expansions();
        self.poll_semantic_requests();
        self.flush_context_changes();
        ProtocolDispatch {
            queued_messages: self.outbound.len().saturating_sub(before),
            backpressured: backpressured_before
                || self.queued_outbound_bytes >= MAX_QUEUED_OUTBOUND_BYTES,
            closed: matches!(
                self.control.lifecycle(),
                SessionLifecycle::Exited | SessionLifecycle::Expired
            ),
        }
    }

    /// Runs only coalesced overlay work. A daemon scheduler can call this when
    /// no new frame arrives so a quiet editor still receives its refresh.
    pub fn flush_due(&mut self, now_ms: u64) -> ProtocolDispatch {
        let before = self.outbound.len();
        self.expire_requests(now_ms);
        self.flush_debounced_diagnostics(now_ms);
        self.poll_context_requests();
        self.poll_context_expansions();
        self.poll_semantic_requests();
        self.flush_context_changes();
        ProtocolDispatch {
            queued_messages: self.outbound.len().saturating_sub(before),
            backpressured: self.queued_outbound_bytes >= MAX_QUEUED_OUTBOUND_BYTES,
            closed: matches!(
                self.control.lifecycle(),
                SessionLifecycle::Exited | SessionLifecycle::Expired
            ),
        }
    }

    /// The daemon-side typed transport polls exactly one already-serialized
    /// frame. It cannot fetch arbitrary daemon socket data.
    pub fn poll_outbound(&mut self) -> Option<&[u8]> {
        let frame = self.outbound.front()?;
        self.outbound_in_flight = true;
        Some(frame.payload.as_slice())
    }

    /// Records that the bridge accepted the current outbound frame. Network
    /// delivery remains at-least-once across reconnects by design.
    pub fn acknowledge_outbound(&mut self) -> bool {
        if !self.outbound_in_flight {
            return false;
        }
        let Some(frame) = self.outbound.pop_front() else {
            self.outbound_in_flight = false;
            return false;
        };
        self.outbound_in_flight = false;
        self.queued_outbound_bytes = self
            .queued_outbound_bytes
            .saturating_sub(frame.payload.len());
        if let Some(publication) = frame.publication {
            self.control.acknowledge_publication_version(
                &publication.uri,
                publication.version,
                publication.generation,
            );
        }
        if self.diagnostic_refresh_needed {
            self.queue_diagnostic_refresh();
        }
        true
    }

    /// Test and adapter convenience. Production transports use
    /// [`Self::poll_outbound`] and [`Self::acknowledge_outbound`] so delivery
    /// state is preserved across temporary backpressure.
    pub fn drain_outbound(&mut self) -> Vec<LspFrame> {
        self.outbound_in_flight = false;
        let mut frames = Vec::with_capacity(self.outbound.len());
        while let Some(frame) = self.outbound.pop_front() {
            self.queued_outbound_bytes = self
                .queued_outbound_bytes
                .saturating_sub(frame.payload.len());
            if let Some(publication) = frame.publication {
                self.control.acknowledge_publication_version(
                    &publication.uri,
                    publication.version,
                    publication.generation,
                );
            }
            frames.push(frame.payload);
        }
        if self.diagnostic_refresh_needed {
            self.queue_diagnostic_refresh();
            while let Some(frame) = self.outbound.pop_front() {
                self.queued_outbound_bytes = self
                    .queued_outbound_bytes
                    .saturating_sub(frame.payload.len());
                frames.push(frame.payload);
            }
        }
        frames
    }

    /// Marks session-local state expired. The retained registry calls this on
    /// TTL expiry; no overlay or queued document text survives the call.
    pub fn expire(&mut self) {
        self.control.expire();
        self.clear_volatile_state();
    }

    fn clear_volatile_state(&mut self) {
        if let Some(cancellation) = &self.cancellation {
            for request_id in self.pending_semantic_requests.keys() {
                let _ = cancellation.cancel_upstream(self.gateway.root(), request_id);
            }
        }
        if let Some(context) = &self.context {
            for pending in self.pending_context_requests.values() {
                let _ = context.cancel_request(self.gateway.root(), &pending.operation_id);
            }
            for pending in self.pending_context_expansions.values() {
                let _ = context.cancel_request(self.gateway.root(), &pending.operation_id);
            }
        }
        self.overlays.clear();
        self.debounce.clear();
        self.outbound.clear();
        self.outbound_in_flight = false;
        self.queued_outbound_bytes = 0;
        self.published.clear();
        self.native_upstream.clear();
        self.cursor_native_mode = false;
        self.diagnostic_refresh_request = None;
        self.diagnostic_refresh_needed = false;
        self.active_diagnostic_refreshes.clear();
        self.context_subscriptions.clear();
        self.context_currentness.clear();
        self.pending_context_requests.clear();
        self.pending_context_expansions.clear();
        self.pending_semantic_requests.clear();
    }

    fn cancel_pending_operations(&mut self) {
        let semantic = std::mem::take(&mut self.pending_semantic_requests);
        for (request_id, pending) in semantic {
            let _ = self.control.cancel_request(&request_id);
            if let Some(cancellation) = &self.cancellation {
                let _ = cancellation.cancel_upstream(self.gateway.root(), &request_id);
            }
            self.complete_context_request(
                request_id,
                pending.response_id,
                Err(RpcFailure::request_failure(
                    LspRequestFailure::RequestCancelled,
                )),
            );
        }

        let snapshots = std::mem::take(&mut self.pending_context_requests);
        for (request_id, pending) in snapshots {
            let _ = self.control.cancel_request(&request_id);
            if let Some(context) = &self.context {
                let _ = context.cancel_request(self.gateway.root(), &pending.operation_id);
            }
            self.complete_context_request(
                request_id,
                pending.response_id,
                Err(RpcFailure::request_failure(
                    LspRequestFailure::RequestCancelled,
                )),
            );
        }

        let expansions = std::mem::take(&mut self.pending_context_expansions);
        for (request_id, pending) in expansions {
            let _ = self.control.cancel_request(&request_id);
            if let Some(context) = &self.context {
                let _ = context.cancel_request(self.gateway.root(), &pending.operation_id);
            }
            self.complete_context_request(
                request_id,
                pending.response_id,
                Err(RpcFailure::request_failure(
                    LspRequestFailure::RequestCancelled,
                )),
            );
        }
    }

    fn dispatch_value(&mut self, value: Value, now_ms: u64) {
        match parse_incoming(value) {
            Ok(incoming) => dispatch_incoming(self, incoming, now_ms),
            Err((response_id, failure)) => {
                self.enqueue_value(error_response(response_id, failure));
            }
        }
    }

    pub(crate) fn handle_initialized_notification(&mut self) {
        let _ = self.control.initialized();
    }

    pub(crate) fn handle_initialized_request(&mut self, response_id: Value) {
        let _ = self.enqueue_value(error_response(
            response_id,
            RpcFailure {
                code: -32600,
                message: "Invalid Request",
                data: json!({ "detail": "initialized must be a notification" }),
            },
        ));
    }

    pub(crate) fn handle_shutdown_request(&mut self, response_id: Value) {
        if self.control.lifecycle() != SessionLifecycle::Ready {
            let _ = self.enqueue_value(error_response(
                response_id,
                RpcFailure {
                    code: -32600,
                    message: "Invalid Request",
                    data: json!({ "detail": "shutdown is not valid in this lifecycle state" }),
                },
            ));
            return;
        }
        self.cancel_pending_operations();
        match self.control.shutdown() {
            Ok(()) => {
                let _ = self.enqueue_value(success_response(response_id, Value::Null));
            }
            Err(_) => {
                let _ = self.enqueue_value(error_response(
                    response_id,
                    RpcFailure {
                        code: -32600,
                        message: "Invalid Request",
                        data: json!({ "detail": "shutdown is not valid in this lifecycle state" }),
                    },
                ));
            }
        }
    }

    pub(crate) fn handle_exit_notification(&mut self) {
        if self.control.exit().is_err() {
            self.expire();
        } else {
            self.clear_volatile_state();
        }
    }

    pub(crate) fn handle_exit_request(&mut self, response_id: Value) {
        let _ = self.enqueue_value(error_response(
            response_id,
            RpcFailure {
                code: -32600,
                message: "Invalid Request",
                data: json!({ "detail": "exit must be a notification" }),
            },
        ));
    }

    pub(crate) fn handle_client_response(&mut self, id: LspRequestId) {
        if self.diagnostic_refresh_request.as_ref() == Some(&id) {
            self.diagnostic_refresh_request = None;
        }
        if self.diagnostic_refresh_needed {
            self.queue_diagnostic_refresh();
        }
    }

    pub(crate) fn document_version(&self, uri: &str) -> i64 {
        self.overlays.version(uri).unwrap_or_default()
    }

    pub(crate) fn handle_initialize(&mut self, id: Value, params: &Value) {
        if self.control.lifecycle() != SessionLifecycle::AwaitingInitialize {
            self.enqueue_value(error_response(
                id,
                RpcFailure {
                    code: -32600,
                    message: "Invalid Request",
                    data: json!({ "detail": "initialize is only valid once" }),
                },
            ));
            return;
        }
        let root_uri = match initialized_root_uri(params) {
            Ok(root_uri) => root_uri,
            Err(error) => {
                self.enqueue_value(error_response(id, error));
                return;
            }
        };
        if !self.gateway.root().matches_root_uri(&root_uri) {
            self.enqueue_value(error_response(
                id,
                RpcFailure {
                    code: -32602,
                    message: "Invalid params",
                    data: json!({ "detail": "root is not the daemon-admitted root" }),
                },
            ));
            return;
        }
        let cursor_native_mode = match cursor_native_initialize_mode(params) {
            Ok(cursor_native_mode) => cursor_native_mode,
            Err(error) => {
                self.enqueue_value(error_response(id, error));
                return;
            }
        };
        let empty = Value::Object(Map::new());
        let client = match ClientCapabilities::from_initialize_capabilities(
            params.get("capabilities").unwrap_or(&empty),
        ) {
            Ok(client) => client,
            Err(CapabilityParseError::ExpectedObject) => {
                self.enqueue_value(error_response(
                    id,
                    RpcFailure::invalid_params("capabilities must be an object"),
                ));
                return;
            }
            Err(CapabilityParseError::InvalidPositionEncodings) => {
                self.enqueue_value(error_response(
                    id,
                    RpcFailure::invalid_params("positionEncodings must be an array of strings"),
                ));
                return;
            }
            Err(CapabilityParseError::InvalidTraceDecayCapabilities) => {
                self.enqueue_value(error_response(
                    id,
                    RpcFailure::invalid_params(
                        "experimental.tracedecay projections must be bounded kind/revision pairs",
                    ),
                ));
                return;
            }
        };
        let mounted_context = self
            .context
            .as_ref()
            .map(|context| {
                context
                    .registrations()
                    .into_iter()
                    .filter(|registration| {
                        registration.kind.is_pr12_supported() && registration.revision > 0
                    })
                    .take(MAX_CONTEXT_PROJECTION_KINDS)
                    .map(|registration| (registration.kind, registration.revision))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let mut gateway_capabilities = self.gateway_capabilities.clone();
        gateway_capabilities
            .context_projections
            .retain(|kind, revision| mounted_context.get(kind) == Some(revision));
        let effective =
            negotiate_capabilities(&client, &gateway_capabilities, &self.upstream_capabilities);
        if let CapabilityAvailability::Unavailable(unavailable) =
            effective.initialization_availability()
        {
            self.enqueue_value(error_response(
                id,
                RpcFailure {
                    code: -32602,
                    message: "Invalid params",
                    data: json!({
                        "capability": unavailable.capability,
                        "reason": format!("{:?}", unavailable.reason),
                    }),
                },
            ));
            return;
        }
        let response = success_response(
            id.clone(),
            json!({
                "capabilities": effective.to_lsp_server_capabilities(),
                "serverInfo": {
                    "name": "tracedecay",
                    "version": effective.protocol_version,
                },
            }),
        );
        // Queue the success before committing lifecycle/capability state. If a
        // backpressured peer filled its outbound budget, a retry remains a
        // valid initialize rather than observing a poisoned half-transition.
        if !self.enqueue_value_exact(response) {
            return;
        }
        if self.control.begin_initialize().is_err() {
            self.enqueue_value(error_response(
                id,
                RpcFailure {
                    code: -32600,
                    message: "Invalid Request",
                    data: json!({ "detail": "initialize is only valid once" }),
                },
            ));
            return;
        }
        self.cursor_native_mode = cursor_native_mode;
        self.gateway
            .bind_initialized_capabilities(effective.clone());
    }

    pub(crate) fn handle_cancel(&mut self, params: &Value) {
        let Some(id) = params.get("id").and_then(request_id) else {
            return;
        };
        let _ = self.cancel_request_and_upstream(&id);
    }

    pub(crate) fn handle_native_diagnostics_notification(&mut self, params: &Value, now_ms: u64) {
        if !self.cursor_native_mode || self.require_ready().is_err() {
            return;
        }
        let Ok(notification) =
            serde_json::from_value::<NativeDiagnosticsNotification>(params.clone())
        else {
            return;
        };
        let Some((uri, snapshot)) = notification.into_snapshot() else {
            return;
        };
        if self.require_document_root(&uri).is_err() {
            return;
        }
        let Some(document_version) = self.overlays.version(&uri) else {
            return;
        };
        if document_version != snapshot.version {
            return;
        }
        if self.native_upstream.get(&uri) == Some(&snapshot) {
            return;
        }
        let version = snapshot.version;
        self.native_upstream.insert(uri.clone(), snapshot);
        if !self
            .debounce
            .schedule_immediate_refresh(uri.clone(), version, now_ms)
        {
            self.native_upstream.remove(&uri);
        }
    }

    pub(crate) fn handle_did_open(
        &mut self,
        params: &Value,
        now_ms: u64,
    ) -> Result<(), RpcFailure> {
        self.require_ready()?;
        let text_document = text_document(params)?;
        let uri = required_nonempty_string(text_document, "uri")?;
        self.require_document_root(&uri)?;
        let language_id = required_nonempty_string(text_document, "languageId")?;
        let version = required_i64(text_document, "version")?;
        let text = required_string(text_document, "text")?;
        let snapshot = self
            .overlays
            .open(uri.clone(), language_id, version, text)
            .map_err(|error| self.close_for_overlay_error(error))?;
        // A close followed by a reopen starts a new document incarnation; LSP
        // versions need not remain monotone across that boundary. Remove any
        // queued/acknowledged publication ordering state before publishing the
        // new incarnation.
        self.debounce.cancel(&uri);
        self.discard_document_publications(&uri);
        if self
            .native_upstream
            .get(&uri)
            .is_some_and(|native| native.version != snapshot.version)
        {
            self.native_upstream.remove(&uri);
        }
        let _ = self.publish_diagnostics(&uri, snapshot.version, 0, Vec::new());
        self.control.supersede_document(&uri, snapshot.version);
        if !self
            .debounce
            .schedule_refresh(uri, snapshot.version, now_ms)
        {
            return Err(self.close_for_debounce_overflow());
        }
        Ok(())
    }

    pub(crate) fn handle_did_change(
        &mut self,
        params: &Value,
        now_ms: u64,
    ) -> Result<(), RpcFailure> {
        self.require_ready()?;
        let text_document = text_document(params)?;
        let uri = required_nonempty_string(text_document, "uri")?;
        self.require_document_root(&uri)?;
        let version = required_i64(text_document, "version")?;
        let changes = params
            .get("contentChanges")
            .and_then(Value::as_array)
            .ok_or_else(|| RpcFailure::invalid_params("contentChanges must be an array"))?;
        if changes.is_empty() {
            return Err(RpcFailure::invalid_params(
                "contentChanges must not be empty",
            ));
        }
        let changes = changes
            .iter()
            .map(parse_overlay_change)
            .collect::<Result<Vec<_>, _>>()?;
        let snapshot = self
            .overlays
            .change(&uri, version, &changes)
            .map_err(|error| self.close_for_overlay_error(error))?;
        self.discard_document_context(&uri);
        self.native_upstream.remove(&uri);
        self.control.supersede_document(&uri, snapshot.version);
        if !self
            .debounce
            .schedule_refresh(uri, snapshot.version, now_ms)
        {
            return Err(self.close_for_debounce_overflow());
        }
        Ok(())
    }

    pub(crate) fn handle_did_close(
        &mut self,
        params: &Value,
        now_ms: u64,
    ) -> Result<(), RpcFailure> {
        self.require_ready()?;
        let uri = required_nonempty_string(text_document(params)?, "uri")?;
        self.require_document_root(&uri)?;
        let closed = self.overlays.close(&uri).map_err(overlay_failure)?;
        self.discard_document_context(&uri);
        self.native_upstream.remove(&uri);
        self.control
            .supersede_document(&uri, closed.version.saturating_add(1));
        if !self.debounce.schedule_clear(uri, closed.version, now_ms) {
            return Err(self.close_for_debounce_overflow());
        }
        Ok(())
    }

    pub(crate) fn handle_did_save(
        &mut self,
        params: &Value,
        now_ms: u64,
    ) -> Result<(), RpcFailure> {
        self.require_ready()?;
        let uri = required_nonempty_string(text_document(params)?, "uri")?;
        self.require_document_root(&uri)?;
        self.discard_document_context(&uri);
        if matches!(
            self.gateway.document_saved(uri.clone()),
            FeedbackCycleResponse::Accepted
        ) {
            let version = self.overlays.version(&uri).unwrap_or_default();
            if !self
                .debounce
                .schedule_immediate_refresh(uri, version, now_ms)
            {
                return Err(self.close_for_debounce_overflow());
            }
        }
        Ok(())
    }

    pub(crate) fn handle_context_request(&mut self, id: Value, params: &Value, now_ms: u64) {
        let request = serde_json::from_value::<ContextProjectionRequest>(params.clone())
            .map_err(|_| RpcFailure::invalid_params("invalid tracedecay/context parameters"));
        match request {
            Ok(mut request) if request.kind.is_valid() => {
                let Some(context_request_id) = request_id(&id) else {
                    let _ = self.enqueue_value(error_response(
                        Value::Null,
                        RpcFailure::invalid_params(
                            "tracedecay/context requires an integer or string request id",
                        ),
                    ));
                    return;
                };
                if let Some(uri) = request.document_uri.as_deref()
                    && let Err(error) = self.require_document_root(uri)
                {
                    let _ = self.enqueue_value(error_response(id, error));
                    return;
                }
                bind_context_document_digest(&mut request, &self.overlays);
                let document = request
                    .document_uri
                    .as_ref()
                    .map(|uri| (uri.clone(), self.overlays.version(uri).unwrap_or_default()));
                self.start_context_request(id, context_request_id, document, request, now_ms);
            }
            Ok(_) => {
                let _ = self.enqueue_value(error_response(
                    id,
                    RpcFailure::invalid_params("invalid TraceDecay projection kind"),
                ));
            }
            Err(error) => {
                let _ = self.enqueue_value(error_response(id, error));
            }
        }
    }

    pub(crate) fn handle_context_expand_request(&mut self, id: Value, params: &Value, now_ms: u64) {
        let request =
            serde_json::from_value::<ContextExpansionRequest>(params.clone()).map_err(|_| {
                RpcFailure::invalid_params("invalid tracedecay/context/expand parameters")
            });
        match request {
            Ok(request) if valid_retrieval_handle(Some(&request.retrieval_handle)) => {
                let Some(context_request_id) = request_id(&id) else {
                    let _ = self.enqueue_value(error_response(
                        Value::Null,
                        RpcFailure::invalid_params(
                            "tracedecay/context/expand requires an integer or string request id",
                        ),
                    ));
                    return;
                };
                self.start_context_expansion(id, context_request_id, request, now_ms);
            }
            Ok(_) => {
                let _ = self.enqueue_value(error_response(
                    id,
                    RpcFailure::invalid_params("invalid TraceDecay retrieval handle"),
                ));
            }
            Err(error) => {
                let _ = self.enqueue_value(error_response(id, error));
            }
        }
    }

    pub(crate) fn handle_context_subscribe(&mut self, id: Value, params: &Value, now_ms: u64) {
        let request = serde_json::from_value::<ContextSubscribeRequest>(params.clone())
            .map_err(|_| RpcFailure::invalid_params("invalid tracedecay/subscribe parameters"));
        match request {
            Ok(request) => {
                self.with_request(id, None, now_ms, move |session| {
                    session.context_subscription_value(request)
                });
            }
            Err(error) => {
                let _ = self.enqueue_value(error_response(id, error));
            }
        }
    }

    pub(crate) fn with_request(
        &mut self,
        id: Value,
        document: Option<(String, i64)>,
        now_ms: u64,
        route: impl FnOnce(&mut Self) -> Result<Value, RpcFailure>,
    ) {
        let Some(request_id) = request_id(&id) else {
            let _ = self.enqueue_value(error_response(
                Value::Null,
                RpcFailure {
                    code: -32600,
                    message: "Invalid Request",
                    data: json!({ "detail": "request id must be an integer or string" }),
                },
            ));
            return;
        };
        let deadline = now_ms.saturating_add(self.request_deadline_ms);
        match self
            .control
            .admit_request_with_deadline(request_id.clone(), document, Some(deadline))
        {
            super::session::RequestAdmission::Accepted => {
                let result = route(self);
                let completion = self.control.complete_request(&request_id);
                if let Some(failure) = completion.failure() {
                    let _ = self
                        .enqueue_value(error_response(id, RpcFailure::request_failure(failure)));
                } else if completion == CompletionDisposition::Publish {
                    match result {
                        Ok(value) => {
                            let _ = self.enqueue_value(success_response(id, value));
                        }
                        Err(error) => {
                            let _ = self.enqueue_value(error_response(id, error));
                        }
                    }
                }
            }
            super::session::RequestAdmission::DuplicateId => {
                let _ = self.enqueue_value(error_response(
                    id,
                    RpcFailure {
                        code: -32600,
                        message: "Invalid Request",
                        data: json!({ "detail": "duplicate request id" }),
                    },
                ));
            }
            super::session::RequestAdmission::SessionUnavailable => {
                let _ = self.enqueue_value(error_response(
                    id,
                    RpcFailure::request_failure(LspRequestFailure::ServerCancelled {
                        retrigger_request: true,
                    }),
                ));
            }
            super::session::RequestAdmission::Saturated { retrigger_request } => {
                let _ = self.enqueue_value(error_response(
                    id,
                    RpcFailure::request_failure(LspRequestFailure::ServerCancelled {
                        retrigger_request,
                    }),
                ));
            }
        }
    }

    pub(crate) fn start_semantic_request(
        &mut self,
        response_id: Value,
        document: Option<(String, i64)>,
        request: SemanticRequest,
        now_ms: u64,
    ) {
        let Some(request_id) = request_id(&response_id) else {
            let _ = self.enqueue_value(error_response(
                Value::Null,
                RpcFailure::invalid_params("semantic request id must be an integer or string"),
            ));
            return;
        };
        let deadline = now_ms.saturating_add(self.request_deadline_ms);
        match self
            .control
            .admit_request_with_deadline(request_id.clone(), document, Some(deadline))
        {
            super::session::RequestAdmission::Accepted => {
                match self.semantic_request_value(&request_id, &request) {
                    Ok(None) => {
                        self.pending_semantic_requests.insert(
                            request_id,
                            PendingSemanticRequest {
                                response_id,
                                request,
                            },
                        );
                    }
                    result => self.complete_semantic_request(request_id, response_id, result),
                }
            }
            super::session::RequestAdmission::DuplicateId => {
                let _ = self.enqueue_value(error_response(
                    response_id,
                    RpcFailure {
                        code: -32600,
                        message: "Invalid Request",
                        data: json!({ "detail": "duplicate request id" }),
                    },
                ));
            }
            super::session::RequestAdmission::SessionUnavailable => {
                let _ = self.enqueue_value(error_response(
                    response_id,
                    RpcFailure::request_failure(LspRequestFailure::ServerCancelled {
                        retrigger_request: true,
                    }),
                ));
            }
            super::session::RequestAdmission::Saturated { retrigger_request } => {
                let _ = self.enqueue_value(error_response(
                    response_id,
                    RpcFailure::request_failure(LspRequestFailure::ServerCancelled {
                        retrigger_request,
                    }),
                ));
            }
        }
    }

    fn semantic_request_value(
        &self,
        request_id: &LspRequestId,
        request: &SemanticRequest,
    ) -> Result<Option<Value>, RpcFailure> {
        match self.gateway.semantic_request(request_id, request) {
            GatewayResponse::Value(value) => Ok(Some(semantic_response_value(value))),
            GatewayResponse::Partial {
                coverage, detail, ..
            } => Err(RpcFailure {
                code: -32802,
                message: "Server cancelled request",
                data: partial_failure_data(coverage, detail),
            }),
            GatewayResponse::Pending => Ok(None),
            GatewayResponse::Unavailable(unavailable) => Err(RpcFailure::unavailable(
                unavailable.method.as_lsp_method(),
                unavailable.reason,
            )),
            GatewayResponse::RequestFailed(failure) => Err(RpcFailure::request_failure(failure)),
        }
    }

    fn complete_semantic_request(
        &mut self,
        request_id: LspRequestId,
        response_id: Value,
        result: Result<Option<Value>, RpcFailure>,
    ) {
        let completion = self.control.complete_request(&request_id);
        if let Some(failure) = completion.failure() {
            let _ = self.enqueue_value(error_response(
                response_id,
                RpcFailure::request_failure(failure),
            ));
        } else if completion == CompletionDisposition::Publish {
            match result {
                Ok(Some(value)) => {
                    let _ = self.enqueue_value(success_response(response_id, value));
                }
                Ok(None) => {}
                Err(error) => {
                    let _ = self.enqueue_value(error_response(response_id, error));
                }
            }
        }
    }

    fn poll_semantic_requests(&mut self) {
        let request_ids = self
            .pending_semantic_requests
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for request_id in request_ids {
            let Some(pending) = self.pending_semantic_requests.get(&request_id).cloned() else {
                continue;
            };
            let result = self.semantic_request_value(&request_id, &pending.request);
            if matches!(result, Ok(None)) {
                continue;
            }
            self.pending_semantic_requests.remove(&request_id);
            self.complete_semantic_request(request_id, pending.response_id, result);
        }
    }

    fn start_context_request(
        &mut self,
        response_id: Value,
        request_id: LspRequestId,
        document: Option<(String, i64)>,
        request: ContextProjectionRequest,
        now_ms: u64,
    ) {
        let deadline = now_ms.saturating_add(self.request_deadline_ms);
        match self
            .control
            .admit_request_with_deadline(request_id.clone(), document, Some(deadline))
        {
            super::session::RequestAdmission::Accepted => {
                let Ok(operation_id) =
                    NEXT_CONTEXT_OPERATION_ID.next_string("lsp-context-operation-")
                else {
                    self.complete_context_request(
                        request_id,
                        response_id,
                        Err(RpcFailure::request_failure(
                            LspRequestFailure::ServerCancelled {
                                retrigger_request: true,
                            },
                        )),
                    );
                    return;
                };
                let operation_id = LspRequestId::String(operation_id);
                match self.context_snapshot_value(&operation_id, &request) {
                    Ok(None) => {
                        self.pending_context_requests.insert(
                            request_id,
                            PendingContextRequest {
                                response_id,
                                operation_id,
                                request,
                            },
                        );
                    }
                    result => self.complete_context_request(request_id, response_id, result),
                }
            }
            super::session::RequestAdmission::DuplicateId => {
                let _ = self.enqueue_value(error_response(
                    response_id,
                    RpcFailure {
                        code: -32600,
                        message: "Invalid Request",
                        data: json!({ "detail": "duplicate request id" }),
                    },
                ));
            }
            super::session::RequestAdmission::SessionUnavailable => {
                let _ = self.enqueue_value(error_response(
                    response_id,
                    RpcFailure::request_failure(LspRequestFailure::ServerCancelled {
                        retrigger_request: true,
                    }),
                ));
            }
            super::session::RequestAdmission::Saturated { retrigger_request } => {
                let _ = self.enqueue_value(error_response(
                    response_id,
                    RpcFailure::request_failure(LspRequestFailure::ServerCancelled {
                        retrigger_request,
                    }),
                ));
            }
        }
    }

    fn start_context_expansion(
        &mut self,
        response_id: Value,
        request_id: LspRequestId,
        request: ContextExpansionRequest,
        now_ms: u64,
    ) {
        let deadline = now_ms.saturating_add(self.request_deadline_ms);
        match self
            .control
            .admit_request_with_deadline(request_id.clone(), None, Some(deadline))
        {
            super::session::RequestAdmission::Accepted => {
                let Ok(operation_id) =
                    NEXT_CONTEXT_OPERATION_ID.next_string("lsp-context-expansion-")
                else {
                    self.complete_context_request(
                        request_id,
                        response_id,
                        Err(RpcFailure::request_failure(
                            LspRequestFailure::ServerCancelled {
                                retrigger_request: true,
                            },
                        )),
                    );
                    return;
                };
                let operation_id = LspRequestId::String(operation_id);
                match self.context_expansion_value(&operation_id, &request) {
                    Ok(None) => {
                        self.pending_context_expansions.insert(
                            request_id,
                            PendingContextExpansion {
                                response_id,
                                operation_id,
                            },
                        );
                    }
                    result => self.complete_context_request(request_id, response_id, result),
                }
            }
            super::session::RequestAdmission::DuplicateId => {
                let _ = self.enqueue_value(error_response(
                    response_id,
                    RpcFailure {
                        code: -32600,
                        message: "Invalid Request",
                        data: json!({ "detail": "duplicate request id" }),
                    },
                ));
            }
            super::session::RequestAdmission::SessionUnavailable => {
                let _ = self.enqueue_value(error_response(
                    response_id,
                    RpcFailure::request_failure(LspRequestFailure::ServerCancelled {
                        retrigger_request: true,
                    }),
                ));
            }
            super::session::RequestAdmission::Saturated { retrigger_request } => {
                let _ = self.enqueue_value(error_response(
                    response_id,
                    RpcFailure::request_failure(LspRequestFailure::ServerCancelled {
                        retrigger_request,
                    }),
                ));
            }
        }
    }

    fn complete_context_request(
        &mut self,
        request_id: LspRequestId,
        response_id: Value,
        result: Result<Option<Value>, RpcFailure>,
    ) {
        let completion = self.control.complete_request(&request_id);
        if let Some(failure) = completion.failure() {
            let _ = self.enqueue_value(error_response(
                response_id,
                RpcFailure::request_failure(failure),
            ));
        } else if completion == CompletionDisposition::Publish {
            match result {
                Ok(Some(value)) => {
                    let _ = self.enqueue_value(success_response(response_id, value));
                }
                Ok(None) => {}
                Err(error) => {
                    let _ = self.enqueue_value(error_response(response_id, error));
                }
            }
        }
    }

    fn context_snapshot_value(
        &mut self,
        request_id: &LspRequestId,
        request: &ContextProjectionRequest,
    ) -> Result<Option<Value>, RpcFailure> {
        let Some(revision) = self
            .gateway
            .capabilities()
            .context_projections
            .get(&request.kind)
            .copied()
        else {
            return Err(RpcFailure::unavailable(
                TRACEDECAY_CONTEXT_METHOD,
                MethodUnavailableReason::CapabilityNotNegotiated,
            ));
        };
        let Some(context) = self.context.as_ref() else {
            return Err(RpcFailure::unavailable(
                TRACEDECAY_CONTEXT_METHOD,
                MethodUnavailableReason::CapabilityNotNegotiated,
            ));
        };
        let outcome = context.snapshot(self.gateway.root(), request_id, request);
        self.context_projection_value(request, revision, outcome)
    }

    fn context_projection_value(
        &mut self,
        request: &ContextProjectionRequest,
        revision: u32,
        outcome: ContextProjectionOutcome,
    ) -> Result<Option<Value>, RpcFailure> {
        let envelope = match outcome {
            ContextProjectionOutcome::Ready(envelope) => envelope,
            ContextProjectionOutcome::Pending => return Ok(None),
            ContextProjectionOutcome::Unsupported | ContextProjectionOutcome::Denied => {
                return Err(RpcFailure::unavailable(
                    TRACEDECAY_CONTEXT_METHOD,
                    MethodUnavailableReason::CapabilityNotNegotiated,
                ));
            }
            ContextProjectionOutcome::Deferred { reason } => {
                return Err(refresh_pending_failure(
                    None,
                    None,
                    Some(bounded_context_text(reason, MAX_CONTEXT_SUMMARY_BYTES)),
                    None,
                ));
            }
            ContextProjectionOutcome::Failed { reason } => {
                return Err(RpcFailure {
                    code: -32603,
                    message: "Internal error",
                    data: json!({
                        "failureClass": bounded_context_text(reason, MAX_CONTEXT_SUMMARY_BYTES),
                    }),
                });
            }
        };
        self.validate_context_envelope(request, revision, &envelope)?;
        let key = (envelope.kind.clone(), envelope.document_uri.clone());
        if self.context_currentness.get(&key).is_some_and(|current| {
            current.generation > envelope.generation
                || (current.generation == envelope.generation
                    && current.identity != envelope.identity)
        }) {
            return Err(refresh_pending_failure(
                None,
                None,
                None,
                Some("superseded-generation".to_owned()),
            ));
        }
        let value = serde_json::to_value(&envelope).map_err(|_| RpcFailure {
            code: -32603,
            message: "Internal error",
            data: Value::Null,
        })?;
        if serde_json::to_vec(&value)
            .map_or(true, |payload| payload.len() > MAX_CONTEXT_PROJECTION_BYTES)
        {
            return Err(refresh_pending_failure(
                None,
                None,
                Some("projection-payload-exceeded".to_owned()),
                None,
            ));
        }
        self.context_currentness.insert(
            key,
            ContextProjectionCurrentness {
                generation: envelope.generation,
                identity: envelope.identity,
            },
        );
        Ok(Some(value))
    }

    fn poll_context_requests(&mut self) {
        let Some(context) = self.context.clone() else {
            return;
        };
        let request_ids = self
            .pending_context_requests
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for request_id in request_ids {
            let Some(operation_id) = self
                .pending_context_requests
                .get(&request_id)
                .map(|pending| pending.operation_id.clone())
            else {
                continue;
            };
            let Some(outcome) = context.poll_snapshot(self.gateway.root(), &operation_id) else {
                continue;
            };
            if outcome == ContextProjectionOutcome::Pending {
                continue;
            }
            let Some(pending) = self.pending_context_requests.remove(&request_id) else {
                continue;
            };
            let Some(revision) = self
                .gateway
                .capabilities()
                .context_projections
                .get(&pending.request.kind)
                .copied()
            else {
                self.complete_context_request(
                    request_id,
                    pending.response_id,
                    Err(RpcFailure::unavailable(
                        TRACEDECAY_CONTEXT_METHOD,
                        MethodUnavailableReason::CapabilityNotNegotiated,
                    )),
                );
                continue;
            };
            let result = self.context_projection_value(&pending.request, revision, outcome);
            self.complete_context_request(request_id, pending.response_id, result);
        }
    }

    fn context_expansion_value(
        &self,
        request_id: &LspRequestId,
        request: &ContextExpansionRequest,
    ) -> Result<Option<Value>, RpcFailure> {
        if !self.gateway.capabilities().supports_context_expansion {
            return Err(RpcFailure::unavailable(
                TRACEDECAY_CONTEXT_EXPAND_METHOD,
                MethodUnavailableReason::CapabilityNotNegotiated,
            ));
        }
        let Some(context) = self.context.as_ref() else {
            return Err(RpcFailure::unavailable(
                TRACEDECAY_CONTEXT_EXPAND_METHOD,
                MethodUnavailableReason::CapabilityNotNegotiated,
            ));
        };
        self.context_expansion_outcome_value(context.expand(
            self.gateway.root(),
            request_id,
            request,
        ))
    }

    fn context_expansion_outcome_value(
        &self,
        outcome: ContextExpansionOutcome,
    ) -> Result<Option<Value>, RpcFailure> {
        match outcome {
            ContextExpansionOutcome::Ready(envelope) => {
                self.validate_context_expansion(&envelope)?;
                let value = serde_json::to_value(envelope).map_err(|_| RpcFailure {
                    code: -32603,
                    message: "Internal error",
                    data: Value::Null,
                })?;
                if serde_json::to_vec(&value)
                    .map_or(true, |payload| payload.len() > MAX_CONTEXT_PROJECTION_BYTES)
                {
                    return Err(refresh_pending_failure(
                        None,
                        None,
                        Some("context-expansion-payload-exceeded".to_owned()),
                        None,
                    ));
                }
                Ok(Some(value))
            }
            ContextExpansionOutcome::Denied => Err(RpcFailure::unavailable(
                TRACEDECAY_CONTEXT_EXPAND_METHOD,
                MethodUnavailableReason::CapabilityNotNegotiated,
            )),
            ContextExpansionOutcome::Pending => Ok(None),
            ContextExpansionOutcome::Failed { reason } => Err(RpcFailure {
                code: -32603,
                message: "Internal error",
                data: json!({
                    "failureClass": bounded_context_text(reason, MAX_CONTEXT_SUMMARY_BYTES),
                }),
            }),
        }
    }

    fn poll_context_expansions(&mut self) {
        let Some(context) = self.context.clone() else {
            return;
        };
        let request_ids = self
            .pending_context_expansions
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for request_id in request_ids {
            let Some(operation_id) = self
                .pending_context_expansions
                .get(&request_id)
                .map(|pending| pending.operation_id.clone())
            else {
                continue;
            };
            let Some(outcome) = context.poll_expansion(self.gateway.root(), &operation_id) else {
                continue;
            };
            if outcome == ContextExpansionOutcome::Pending {
                continue;
            }
            let Some(pending) = self.pending_context_expansions.remove(&request_id) else {
                continue;
            };
            let result = self.context_expansion_outcome_value(outcome);
            self.complete_context_request(request_id, pending.response_id, result);
        }
    }

    fn validate_context_expansion(
        &self,
        envelope: &ContextExpansionEnvelope,
    ) -> Result<(), RpcFailure> {
        let negotiated = self
            .gateway
            .capabilities()
            .context_projections
            .get(&envelope.kind)
            == Some(&envelope.revision);
        let valid_scope = envelope.root_uri == self.gateway.root().uri()
            && envelope.kind.is_pr12_supported()
            && envelope.generation > 0
            && envelope
                .document_uri
                .as_deref()
                .is_none_or(|uri| self.gateway.root().contains_document(uri))
            && !envelope.scope.scope_digest.is_empty()
            && valid_context_projection_identity(&envelope.scope.identity)
            && match (
                envelope.document_uri.is_some(),
                envelope.scope.identity.document_content_digest.as_deref(),
            ) {
                (true, Some(digest)) => !digest.is_empty(),
                (false, None) => true,
                _ => false,
            };
        let current_scope = envelope.coverage != ContextCoverage::Complete
            || self
                .context_currentness
                .get(&(envelope.kind.clone(), envelope.document_uri.clone()))
                .is_some_and(|current| {
                    current.generation == envelope.generation
                        && current.identity == envelope.scope.identity
                });
        let valid_payload = !envelope.stable_id.is_empty()
            && envelope.stable_id.len() <= MAX_CONTEXT_RETRIEVAL_HANDLE_BYTES
            && envelope.expires_at > 0
            && valid_retrieval_handle(envelope.next_retrieval_handle.as_deref())
            && match envelope.coverage {
                ContextCoverage::Complete => {
                    envelope.evidence.is_some()
                        && envelope.omission_reason.is_none()
                        && envelope.next_retrieval_handle.is_none()
                }
                ContextCoverage::Partial => envelope.omission_reason.is_some(),
                ContextCoverage::Unavailable | ContextCoverage::Failed => false,
            };
        if negotiated && valid_scope && current_scope && valid_payload {
            Ok(())
        } else {
            Err(RpcFailure {
                code: -32603,
                message: "Internal error",
                data: json!({ "failureClass": "invalid-context-expansion" }),
            })
        }
    }

    fn context_subscription_value(
        &mut self,
        request: ContextSubscribeRequest,
    ) -> Result<Value, RpcFailure> {
        if self.context.is_none() || request.projections.len() > MAX_CONTEXT_PROJECTION_KINDS {
            return Err(RpcFailure::invalid_params(
                "TraceDecay projection subscription is unavailable or too large",
            ));
        }
        let subscriptions = request.projections.into_iter().collect::<BTreeSet<_>>();
        if subscriptions.len() > MAX_CONTEXT_PROJECTION_KINDS
            || subscriptions.iter().any(|registration| {
                !registration.kind.is_valid()
                    || self
                        .gateway
                        .capabilities()
                        .context_projections
                        .get(&registration.kind)
                        != Some(&registration.revision)
            })
        {
            return Err(RpcFailure::unavailable(
                TRACEDECAY_SUBSCRIBE_METHOD,
                MethodUnavailableReason::CapabilityNotNegotiated,
            ));
        }
        self.context_subscriptions = subscriptions;
        if let Some(context) = &self.context {
            context.update_subscriptions(self.gateway.root(), &self.context_subscriptions);
        }
        Ok(json!({
            "projections": self.context_subscriptions.iter().collect::<Vec<_>>(),
        }))
    }

    fn validate_context_envelope(
        &self,
        request: &ContextProjectionRequest,
        revision: u32,
        envelope: &ContextProjectionEnvelope,
    ) -> Result<(), RpcFailure> {
        let valid_scope = envelope.root_uri == self.gateway.root().uri()
            && envelope.kind.is_pr12_supported()
            && envelope.generation > 0
            && envelope.document_uri == request.document_uri
            && envelope
                .document_uri
                .as_deref()
                .is_none_or(|uri| self.gateway.root().contains_document(uri))
            && valid_context_projection_identity(&envelope.identity)
            && match (
                envelope.document_uri.is_some(),
                envelope.identity.document_content_digest.as_deref(),
            ) {
                (true, Some(digest)) => !digest.is_empty(),
                (false, None) => true,
                _ => false,
            }
            && request
                .document_content_digest
                .as_ref()
                .is_none_or(|expected| {
                    envelope.identity.document_content_digest.as_deref() == Some(expected.as_str())
                });
        let valid_items = envelope.items.len() <= MAX_CONTEXT_PROJECTION_ITEMS
            && envelope.items.iter().all(|item| {
                !item.stable_id.is_empty()
                    && item.stable_id.len() <= MAX_CONTEXT_RETRIEVAL_HANDLE_BYTES
                    && item.summary.len() <= MAX_CONTEXT_SUMMARY_BYTES
                    && valid_retrieval_handle(item.retrieval_handle.as_deref())
            });
        if valid_scope
            && envelope.kind == request.kind
            && envelope.revision == revision
            && valid_items
            && match envelope.coverage {
                ContextCoverage::Complete => {
                    envelope.freshness == ContextFreshness::Current
                        && envelope.producer_state == ContextProducerState::Complete
                        && envelope.omitted_count == 0
                        && envelope.omission_reasons.is_empty()
                }
                ContextCoverage::Partial => {
                    !envelope.omission_reasons.is_empty()
                        && matches!(
                            envelope.producer_state,
                            ContextProducerState::Partial | ContextProducerState::Indexing
                        )
                }
                ContextCoverage::Unavailable => {
                    envelope.items.is_empty()
                        && !envelope.omission_reasons.is_empty()
                        && matches!(
                            envelope.producer_state,
                            ContextProducerState::Unavailable
                                | ContextProducerState::Cancelled
                                | ContextProducerState::TimedOut
                        )
                }
                ContextCoverage::Failed => {
                    envelope.items.is_empty()
                        && !envelope.omission_reasons.is_empty()
                        && envelope.producer_state == ContextProducerState::Failed
                }
            }
            && valid_retrieval_handle(envelope.retrieval_handle.as_deref())
        {
            Ok(())
        } else {
            Err(RpcFailure {
                code: -32603,
                message: "Internal error",
                data: json!({ "failureClass": "invalid-context-projection" }),
            })
        }
    }

    pub(crate) fn pull_diagnostics(
        &mut self,
        uri: &str,
        params: &Value,
    ) -> Result<Value, RpcFailure> {
        self.require_document_root(uri)?;
        if !self.gateway.capabilities().supports_document_diagnostics {
            return Err(RpcFailure::unavailable(
                GatewayMethod::TextDocumentDiagnostic.as_lsp_method(),
                MethodUnavailableReason::CapabilityNotNegotiated,
            ));
        }
        response_value(self.gateway.request_document_diagnostics(uri), |()| {
            Value::Null
        })?;
        self.poll_diagnostic_refresh(uri);
        let overlay = self.overlays.snapshot(uri);
        let outcome =
            self.diagnostics
                .document_diagnostics(self.gateway.root(), uri, overlay.as_ref());
        let version = overlay.as_ref().map_or(0, |overlay| overlay.version);
        let source_generation = diagnostic_source_generation(&outcome);
        let _refresh_failure =
            self.request_diagnostic_refresh(uri, version, overlay.as_ref(), source_generation);
        let diagnostics = match outcome {
            DiagnosticSnapshotOutcome::Ready { diagnostics, .. } => diagnostics,
            DiagnosticSnapshotOutcome::Refreshing(refresh) => {
                return Err(refresh_pending_failure(
                    Some(refresh.operation_id),
                    refresh.target_generation,
                    None,
                    None,
                ));
            }
            DiagnosticSnapshotOutcome::Partial {
                source_generation: _,
                coverage,
            } => {
                return Err(refresh_pending_failure(None, None, Some(coverage), None));
            }
            DiagnosticSnapshotOutcome::Failed {
                source_generation: _,
                failure_class,
            } => {
                return Err(refresh_pending_failure(
                    None,
                    None,
                    None,
                    Some(failure_class),
                ));
            }
        };
        let generation = diagnostics.generation;
        if self.published.get(uri).is_some_and(|published| {
            published.version == version && published.generation > generation
        }) {
            return Err(refresh_pending_failure(
                None,
                None,
                None,
                Some("superseded-generation".to_owned()),
            ));
        }
        let result_id = diagnostic_result_id(generation, version);
        let merged =
            self.merge_document_diagnostics(uri, diagnostics.upstream, diagnostics.tracedecay);
        let value = document_diagnostic_report_value(
            DocumentDiagnosticReport::full(
                result_id.clone(),
                self.visible_diagnostics(
                    merged.items,
                    self.gateway.capabilities().document_diagnostics_data,
                ),
            ),
            DiagnosticSerializationCapabilities::pull(self.gateway.capabilities()),
        );
        let previous = params.get("previousResultId").and_then(Value::as_str);
        if previous == Some(result_id.as_str()) {
            return Ok(document_diagnostic_report_value(
                DocumentDiagnosticReport::Unchanged { result_id },
                DiagnosticSerializationCapabilities::pull(self.gateway.capabilities()),
            ));
        }
        if overlay.is_some() {
            self.published.insert(
                uri.to_owned(),
                PublishedDiagnostic {
                    version,
                    generation,
                },
            );
        }
        Ok(value)
    }

    fn flush_debounced_diagnostics(&mut self, now_ms: u64) {
        if self.control.lifecycle() != SessionLifecycle::Ready {
            return;
        }
        self.poll_diagnostic_refreshes();
        while self.has_outbound_capacity(MAX_PUBLICATION_BYTES) {
            let Some(scheduled) = self.debounce.take_next_due(now_ms) else {
                break;
            };
            match scheduled.kind {
                DebouncedDiagnosticKind::Clear => {
                    let generation = self
                        .published
                        .get(&scheduled.uri)
                        .map_or(0, |published| published.generation);
                    self.discard_document_publications(&scheduled.uri);
                    if self.publish_diagnostics(
                        &scheduled.uri,
                        scheduled.version,
                        generation,
                        Vec::new(),
                    ) {
                        self.published.remove(&scheduled.uri);
                        self.queue_diagnostic_refresh();
                    }
                }
                DebouncedDiagnosticKind::Refresh => {
                    let overlay = self.overlays.snapshot(&scheduled.uri);
                    let outcome = self.diagnostics.document_diagnostics(
                        self.gateway.root(),
                        &scheduled.uri,
                        overlay.as_ref(),
                    );
                    let source_generation = diagnostic_source_generation(&outcome);
                    let _ = self.request_diagnostic_refresh(
                        &scheduled.uri,
                        scheduled.version,
                        overlay.as_ref(),
                        source_generation,
                    );
                    if let DiagnosticSnapshotOutcome::Ready { diagnostics, .. } = outcome {
                        let _ = self.publish_complete_snapshot(
                            &scheduled.uri,
                            scheduled.version,
                            diagnostics,
                        );
                    }
                }
            }
        }
    }

    fn request_diagnostic_refresh(
        &mut self,
        uri: &str,
        version: i64,
        overlay: Option<&super::overlay::OverlaySnapshot>,
        source_generation: Option<u64>,
    ) -> Option<String> {
        if self
            .active_diagnostic_refreshes
            .get(uri)
            .is_some_and(|pending| pending.overlay_version == version)
        {
            return None;
        }
        match self.diagnostics.request_document_refresh(
            self.gateway.root(),
            uri,
            overlay,
            source_generation,
        ) {
            DiagnosticRefreshAdmission::Started(identity)
            | DiagnosticRefreshAdmission::AlreadyRunning(identity) => {
                if !valid_refresh_identity(&identity, source_generation) {
                    return Some("invalid-refresh-identity".to_owned());
                }
                self.active_diagnostic_refreshes.insert(
                    uri.to_owned(),
                    PendingDiagnosticRefresh {
                        identity,
                        overlay_version: version,
                    },
                );
                None
            }
            DiagnosticRefreshAdmission::Rejected { failure_class } => Some(failure_class),
        }
    }

    fn poll_diagnostic_refreshes(&mut self) {
        let documents: Vec<_> = self.active_diagnostic_refreshes.keys().cloned().collect();
        for document in documents {
            if !self.has_outbound_capacity(MAX_PUBLICATION_BYTES) {
                break;
            }
            self.poll_diagnostic_refresh(&document);
        }
    }

    fn poll_diagnostic_refresh(&mut self, uri: &str) {
        let Some(pending) = self.active_diagnostic_refreshes.get(uri).cloned() else {
            return;
        };
        let version = self.overlays.version(uri).unwrap_or_default();
        if version != pending.overlay_version {
            self.active_diagnostic_refreshes.remove(uri);
            return;
        }
        let overlay = self.overlays.snapshot(uri);
        match self
            .diagnostics
            .document_diagnostics(self.gateway.root(), uri, overlay.as_ref())
        {
            DiagnosticSnapshotOutcome::Ready {
                diagnostics,
                completed_operation_id,
            } if completed_operation_id.as_deref()
                == Some(pending.identity.operation_id.as_str()) =>
            {
                let generation = diagnostics.generation;
                let target_matches = pending
                    .identity
                    .target_generation
                    .is_none_or(|target| target == generation);
                let source_not_superseded = pending
                    .identity
                    .source_generation
                    .is_none_or(|source| generation >= source);
                let publication_not_superseded = self.published.get(uri).is_none_or(|published| {
                    published.version != version || published.generation <= generation
                });
                if target_matches && source_not_superseded && publication_not_superseded {
                    if self.publish_complete_snapshot(uri, version, diagnostics) {
                        self.active_diagnostic_refreshes.remove(uri);
                    }
                } else {
                    self.active_diagnostic_refreshes.remove(uri);
                }
            }
            DiagnosticSnapshotOutcome::Partial { .. }
            | DiagnosticSnapshotOutcome::Failed { .. } => {
                self.active_diagnostic_refreshes.remove(uri);
                self.queue_diagnostic_refresh();
            }
            DiagnosticSnapshotOutcome::Ready { .. } | DiagnosticSnapshotOutcome::Refreshing(_) => {}
        }
    }

    fn publish_complete_snapshot(
        &mut self,
        uri: &str,
        version: i64,
        snapshot: super::provider::GenerationDiagnostics,
    ) -> bool {
        let generation = snapshot.generation;
        if self.published.get(uri).is_some_and(|published| {
            published.version == version && published.generation > generation
        }) {
            return false;
        }
        let merged = self.merge_document_diagnostics(uri, snapshot.upstream, snapshot.tracedecay);
        if self.gateway.capabilities().supports_publish_diagnostics
            && !self.publish_diagnostics(
                uri,
                version,
                generation,
                self.visible_diagnostics(
                    merged.items,
                    self.gateway.capabilities().publish_diagnostics_data,
                ),
            )
        {
            return false;
        }
        self.published.insert(
            uri.to_owned(),
            PublishedDiagnostic {
                version,
                generation,
            },
        );
        self.queue_diagnostic_refresh();
        true
    }

    fn merge_document_diagnostics(
        &self,
        uri: &str,
        mut upstream: Vec<GatewayDiagnostic>,
        tracedecay: Vec<GatewayDiagnostic>,
    ) -> DiagnosticMerge {
        if let Some(native) = self.native_upstream.get(uri) {
            upstream.extend(native.diagnostics.iter().cloned());
        }
        DiagnosticMerge::for_document(uri, upstream, tracedecay)
    }

    fn visible_diagnostics(
        &self,
        mut diagnostics: Vec<GatewayDiagnostic>,
        supports_diagnostic_data: bool,
    ) -> Vec<GatewayDiagnostic> {
        for diagnostic in &mut diagnostics {
            diagnostic
                .related_information
                .retain(|related| self.gateway.root().contains_document(&related.uri));
        }
        diagnostics
            .into_iter()
            .filter(|diagnostic| {
                (!self.cursor_native_mode || diagnostic.source.is_tracedecay())
                    && (supports_diagnostic_data || !diagnostic.source.is_tracedecay())
            })
            .collect()
    }

    fn publish_diagnostics(
        &mut self,
        uri: &str,
        version: i64,
        generation: u64,
        diagnostics: Vec<GatewayDiagnostic>,
    ) -> bool {
        if !self.gateway.capabilities().supports_publish_diagnostics {
            return false;
        }
        let capabilities = self.gateway.capabilities();
        let mut params = json!({
            "uri": uri,
            "diagnostics": diagnostics
                .into_iter()
                .map(|diagnostic| diagnostic_value(
                    diagnostic,
                    DiagnosticSerializationCapabilities::push(capabilities),
                ))
                .collect::<Vec<_>>(),
        });
        if capabilities.publish_diagnostics_version {
            params["version"] = Value::from(version);
        }
        let value = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": params,
        });
        self.enqueue_publication(
            value,
            PublicationTag {
                uri: uri.to_owned(),
                version,
                generation,
            },
        )
    }

    fn queue_diagnostic_refresh(&mut self) {
        if !self
            .gateway
            .capabilities()
            .supports_workspace_diagnostic_refresh
        {
            self.diagnostic_refresh_needed = false;
            return;
        }
        self.diagnostic_refresh_needed = true;
        if self.diagnostic_refresh_request.is_some() {
            return;
        }
        let Ok(id) = self
            .next_server_request_id
            .next_string("tracedecay-diagnostic-refresh-")
        else {
            return;
        };
        let id = LspRequestId::String(id);
        if self.enqueue_value(json!({
            "jsonrpc": "2.0",
            "id": request_id_value(id.clone()),
            "method": "workspace/diagnostic/refresh",
            "params": {},
        })) {
            self.diagnostic_refresh_request = Some(id);
            self.diagnostic_refresh_needed = false;
        }
    }

    fn flush_context_changes(&mut self) {
        if self.context_subscriptions.is_empty()
            || !self.has_outbound_capacity(MAX_CONTEXT_PROJECTION_BYTES)
        {
            return;
        }
        let Some(context) = self.context.clone() else {
            return;
        };
        let changes = context.poll_changes(self.gateway.root(), &self.context_subscriptions);
        for mut change in changes.into_iter().take(MAX_CONTEXT_CHANGES_PER_POLL) {
            if !self.valid_context_change(&change) {
                continue;
            }
            let key = (change.kind.clone(), change.document_uri.clone());
            let identity_drift_clear = match self.context_currentness.get(&key) {
                Some(current) if current.generation > change.generation => continue,
                Some(current) if current.generation == change.generation => {
                    if current.identity == change.identity {
                        false
                    } else {
                        change.identity = current.identity.clone();
                        change.freshness = ContextFreshness::Unknown;
                        change.producer_state = ContextProducerState::Unavailable;
                        change.coverage = ContextCoverage::Unavailable;
                        change.retrieval_handle = None;
                        true
                    }
                }
                _ => false,
            };
            if !self.valid_context_change(&change) {
                continue;
            }
            let Ok(params) = serde_json::to_value(&change) else {
                continue;
            };
            let notification = json!({
                "jsonrpc": "2.0",
                "method": TRACEDECAY_CONTEXT_CHANGED_METHOD,
                "params": params,
            });
            if serde_json::to_vec(&notification)
                .map_or(true, |payload| payload.len() > MAX_CONTEXT_PROJECTION_BYTES)
                || !self.enqueue_value(notification)
            {
                break;
            }
            if identity_drift_clear {
                self.context_currentness.remove(&key);
            } else {
                self.context_currentness.insert(
                    key,
                    ContextProjectionCurrentness {
                        generation: change.generation,
                        identity: change.identity,
                    },
                );
            }
        }
    }

    fn valid_context_change(&self, change: &ContextProjectionChange) -> bool {
        change.root_uri == self.gateway.root().uri()
            && change.kind.is_pr12_supported()
            && change.generation > 0
            && change
                .document_uri
                .as_deref()
                .is_none_or(|uri| self.gateway.root().contains_document(uri))
            && valid_context_projection_identity(&change.identity)
            && match (
                change.document_uri.is_some(),
                change.identity.document_content_digest.as_deref(),
            ) {
                (true, Some(digest)) => !digest.is_empty(),
                (false, None) => true,
                _ => false,
            }
            && self
                .context_subscriptions
                .contains(&ContextProjectionRegistration {
                    kind: change.kind.clone(),
                    revision: change.revision,
                })
            && match change.coverage {
                ContextCoverage::Complete => {
                    change.freshness == ContextFreshness::Current
                        && change.producer_state == ContextProducerState::Complete
                }
                ContextCoverage::Partial => matches!(
                    change.producer_state,
                    ContextProducerState::Partial | ContextProducerState::Indexing
                ),
                ContextCoverage::Unavailable => matches!(
                    change.producer_state,
                    ContextProducerState::Unavailable
                        | ContextProducerState::Cancelled
                        | ContextProducerState::TimedOut
                ),
                ContextCoverage::Failed => change.producer_state == ContextProducerState::Failed,
            }
            && valid_retrieval_handle(change.retrieval_handle.as_deref())
    }

    fn expire_requests(&mut self, now_ms: u64) {
        for id in self.control.expire_deadlines(now_ms) {
            if self.pending_semantic_requests.remove(&id).is_some()
                && let Some(cancellation) = &self.cancellation
            {
                let _ = cancellation.cancel_upstream(self.gateway.root(), &id);
            }
            if let Some(pending) = self.pending_context_requests.remove(&id)
                && let Some(context) = &self.context
            {
                let _ = context.cancel_request(self.gateway.root(), &pending.operation_id);
            }
            if let Some(pending) = self.pending_context_expansions.remove(&id)
                && let Some(context) = &self.context
            {
                let _ = context.cancel_request(self.gateway.root(), &pending.operation_id);
            }
            let disposition = self.control.complete_request(&id);
            if let Some(failure) = disposition.failure() {
                self.enqueue_value(error_response(
                    request_id_value(id),
                    RpcFailure::request_failure(failure),
                ));
            }
        }
    }

    fn cancel_request_and_upstream(&mut self, id: &LspRequestId) -> CancellationOutcome {
        let outcome = self.control.cancel_request(id);
        if outcome == CancellationOutcome::Accepted {
            let semantic = self.pending_semantic_requests.remove(id);
            let context_pending = self.pending_context_requests.remove(id);
            let expansion_pending = self.pending_context_expansions.remove(id);
            if let Some(cancellation) = &self.cancellation {
                let _ = cancellation.cancel_upstream(self.gateway.root(), id);
            }
            if let Some(context) = &self.context {
                if let Some(pending) = context_pending.as_ref() {
                    let _ = context.cancel_request(self.gateway.root(), &pending.operation_id);
                }
                if let Some(pending) = expansion_pending.as_ref() {
                    let _ = context.cancel_request(self.gateway.root(), &pending.operation_id);
                }
                if context_pending.is_none() && expansion_pending.is_none() {
                    let _ = context.cancel_request(self.gateway.root(), id);
                }
            }
            let response_id = semantic
                .map(|pending| pending.response_id)
                .or_else(|| context_pending.map(|pending| pending.response_id))
                .or_else(|| expansion_pending.map(|pending| pending.response_id));
            if let Some(response_id) = response_id {
                self.complete_context_request(
                    id.clone(),
                    response_id,
                    Err(RpcFailure::request_failure(
                        LspRequestFailure::RequestCancelled,
                    )),
                );
            }
        }
        outcome
    }

    pub(crate) fn enqueue_value(&mut self, value: Value) -> bool {
        let server_request = value
            .get("method")
            .and_then(Value::as_str)
            .and_then(|_| value.get("id"))
            .and_then(request_id);
        let response_id = value
            .get("id")
            .cloned()
            .filter(|_| value.get("method").is_none());
        let Ok(payload) = serde_json::to_vec(&value) else {
            return false;
        };
        if payload.len() <= MAX_LSP_FRAME_BYTES && self.enqueue_frame(payload, None, server_request)
        {
            return true;
        }
        let Some(id) = response_id else {
            return false;
        };
        let Ok(fallback) = serde_json::to_vec(&error_response(
            id,
            RpcFailure::request_failure(LspRequestFailure::ServerCancelled {
                retrigger_request: true,
            }),
        )) else {
            return false;
        };
        self.enqueue_frame(fallback, None, None)
    }

    fn enqueue_value_exact(&mut self, value: Value) -> bool {
        let server_request = value
            .get("method")
            .and_then(Value::as_str)
            .and_then(|_| value.get("id"))
            .and_then(request_id);
        let Ok(payload) = serde_json::to_vec(&value) else {
            return false;
        };
        payload.len() <= MAX_LSP_FRAME_BYTES && self.enqueue_frame(payload, None, server_request)
    }

    fn enqueue_publication(&mut self, value: Value, tag: PublicationTag) -> bool {
        let Ok(payload) = serde_json::to_vec(&value) else {
            return false;
        };
        if payload.len() > MAX_PUBLICATION_BYTES {
            return false;
        }
        let Ok(replacement) = self.publication_replacement(&payload, &tag) else {
            return false;
        };
        if replacement.is_some_and(|index| {
            self.outbound[index]
                .publication
                .as_ref()
                .is_some_and(|existing| {
                    (existing.version, existing.generation) == (tag.version, tag.generation)
                })
        }) {
            // The queued initial clear for a reopened document may be replaced
            // by diagnostics for the same version/generation before either is
            // delivered. Queue identity, not just tuple equality, determines
            // whether that is a duplicate.
            self.control.remove_publication(&tag.uri);
        }
        match self.control.admit_sized_publication(
            tag.uri.clone(),
            tag.version,
            tag.generation,
            payload.len(),
        ) {
            PublicationAdmission::Accepted => {}
            PublicationAdmission::Duplicate | PublicationAdmission::Stale => return false,
            PublicationAdmission::TooLarge { .. } | PublicationAdmission::SessionUnavailable => {
                return false;
            }
        }
        self.enqueue_frame(payload, Some(tag), None)
    }

    fn enqueue_frame(
        &mut self,
        payload: LspFrame,
        publication: Option<PublicationTag>,
        server_request: Option<LspRequestId>,
    ) -> bool {
        if payload.len() > MAX_LSP_FRAME_BYTES {
            return false;
        }
        let replacement = if let Some(tag) = &publication {
            let Ok(replacement) = self.publication_replacement(&payload, tag) else {
                return false;
            };
            replacement
        } else {
            if self.outbound.len() >= MAX_QUEUED_OUTBOUND_MESSAGES
                || self.queued_outbound_bytes.saturating_add(payload.len())
                    > MAX_QUEUED_OUTBOUND_BYTES
            {
                return false;
            }
            None
        };
        if let Some(index) = replacement {
            let Some(existing) = self.outbound.get(index) else {
                return false;
            };
            debug_assert!(existing.publication.is_some());
            let Some(replaced) = self.outbound.remove(index) else {
                return false;
            };
            self.queued_outbound_bytes = self
                .queued_outbound_bytes
                .saturating_sub(replaced.payload.len());
        }
        if let Some(tag) = &publication {
            self.control.mark_publication_queued(&tag.uri);
        }
        self.queued_outbound_bytes += payload.len();
        self.outbound.push_back(QueuedFrame {
            payload,
            publication,
            server_request,
        });
        true
    }

    fn publication_replacement(
        &self,
        payload: &[u8],
        tag: &PublicationTag,
    ) -> Result<Option<usize>, ()> {
        let replacement = self
            .outbound
            .iter()
            .enumerate()
            .find(|(index, frame)| {
                !(self.outbound_in_flight && *index == 0)
                    && frame
                        .publication
                        .as_ref()
                        .is_some_and(|existing| existing.uri == tag.uri)
            })
            .map(|(index, _)| index);
        let replaced_len = replacement
            .and_then(|index| self.outbound.get(index))
            .map_or(0, |frame| frame.payload.len());
        if let Some(index) = replacement {
            let existing = self.outbound[index].publication.as_ref().ok_or(())?;
            if (tag.version, tag.generation) < (existing.version, existing.generation)
                || ((tag.version, tag.generation) == (existing.version, existing.generation)
                    && self.outbound[index].payload == payload)
            {
                return Err(());
            }
        }
        let projected_messages = self.outbound.len() + usize::from(replacement.is_none());
        let projected_bytes = self
            .queued_outbound_bytes
            .saturating_sub(replaced_len)
            .saturating_add(payload.len());
        if projected_messages > MAX_QUEUED_OUTBOUND_MESSAGES
            || projected_bytes > MAX_QUEUED_OUTBOUND_BYTES
        {
            return Err(());
        }
        Ok(replacement)
    }

    fn discard_document_publications(&mut self, uri: &str) {
        self.discard_document_context(uri);
        self.active_diagnostic_refreshes.remove(uri);
        let mut retained = VecDeque::with_capacity(self.outbound.len());
        let mut index = 0_usize;
        while let Some(frame) = self.outbound.pop_front() {
            let is_in_flight = self.outbound_in_flight && index == 0;
            index += 1;
            if !is_in_flight
                && frame
                    .publication
                    .as_ref()
                    .is_some_and(|publication| publication.uri == uri)
            {
                self.queued_outbound_bytes = self
                    .queued_outbound_bytes
                    .saturating_sub(frame.payload.len());
            } else {
                retained.push_back(frame);
            }
        }
        self.outbound = retained;
        self.control.remove_publication(uri);
        self.published.remove(uri);
    }

    fn discard_document_context(&mut self, uri: &str) {
        self.context_currentness
            .retain(|(_, document_uri), _| document_uri.as_deref() != Some(uri));
    }

    fn has_outbound_capacity(&self, reserve_bytes: usize) -> bool {
        self.outbound.len() < MAX_QUEUED_OUTBOUND_MESSAGES
            && self.queued_outbound_bytes.saturating_add(reserve_bytes) <= MAX_QUEUED_OUTBOUND_BYTES
    }

    fn require_ready(&self) -> Result<(), RpcFailure> {
        (self.control.lifecycle() == SessionLifecycle::Ready)
            .then_some(())
            .ok_or_else(|| {
                RpcFailure::request_failure(LspRequestFailure::ServerCancelled {
                    retrigger_request: true,
                })
            })
    }

    fn require_document_root(&self, uri: &str) -> Result<(), RpcFailure> {
        self.gateway
            .root()
            .contains_document(uri)
            .then_some(())
            .ok_or_else(|| {
                RpcFailure::unavailable(
                    GatewayMethod::TextDocumentDiagnostic.as_lsp_method(),
                    MethodUnavailableReason::OutsideAdmittedRoot,
                )
            })
    }

    fn close_for_overlay_error(&mut self, error: OverlayError) -> RpcFailure {
        if matches!(
            error,
            OverlayError::TooLarge { .. } | OverlayError::TooManyDocuments { .. }
        ) {
            self.expire();
        }
        overlay_failure(error)
    }

    fn close_for_debounce_overflow(&mut self) -> RpcFailure {
        self.expire();
        RpcFailure::request_failure(LspRequestFailure::ServerCancelled {
            retrigger_request: true,
        })
    }
}

fn diagnostic_source_generation(outcome: &DiagnosticSnapshotOutcome) -> Option<u64> {
    match outcome {
        DiagnosticSnapshotOutcome::Ready { diagnostics, .. } => Some(diagnostics.generation),
        DiagnosticSnapshotOutcome::Refreshing(refresh) => refresh.source_generation,
        DiagnosticSnapshotOutcome::Partial {
            source_generation, ..
        }
        | DiagnosticSnapshotOutcome::Failed {
            source_generation, ..
        } => *source_generation,
    }
}

fn valid_refresh_identity(
    identity: &DiagnosticRefreshIdentity,
    source_generation: Option<u64>,
) -> bool {
    !identity.operation_id.is_empty()
        && identity.operation_id.len() <= MAX_DIAGNOSTIC_OPERATION_ID_BYTES
        && identity.source_generation == source_generation
        && match (identity.source_generation, identity.target_generation) {
            (Some(source), Some(target)) => target >= source,
            _ => true,
        }
}

fn refresh_pending_failure(
    operation_id: Option<String>,
    target_generation: Option<u64>,
    coverage: Option<String>,
    failure_class: Option<String>,
) -> RpcFailure {
    RpcFailure {
        code: -32802,
        message: "Server cancelled request",
        data: json!({
            "retriggerRequest": true,
            "operationId": operation_id,
            "targetGeneration": target_generation,
            "coverage": coverage,
            "failureClass": failure_class,
        }),
    }
}

fn cursor_native_initialize_mode(params: &Value) -> Result<bool, RpcFailure> {
    let Some(options) = params.get("initializationOptions") else {
        return Ok(false);
    };
    if options.is_null() {
        return Ok(false);
    }
    let options = options
        .as_object()
        .ok_or_else(|| RpcFailure::invalid_params("initializationOptions must be an object"))?;
    let Some(tracedecay) = options.get("tracedecay") else {
        return Ok(false);
    };
    let tracedecay = tracedecay.as_object().ok_or_else(|| {
        RpcFailure::invalid_params("initializationOptions.tracedecay must be an object")
    })?;
    if tracedecay.get("mode").and_then(Value::as_str) != Some("cursor-native") {
        return Ok(false);
    }
    (tracedecay.get("context").and_then(Value::as_bool) == Some(true))
        .then_some(true)
        .ok_or_else(|| {
            RpcFailure::invalid_params(
                "cursor-native initialization requires tracedecay context support",
            )
        })
}

fn valid_native_string(value: &str, maximum_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= maximum_bytes
}

fn valid_native_diagnostic_data(data: Option<&Value>) -> bool {
    let Some(data) = data else {
        return true;
    };
    if data.is_null() {
        return true;
    }
    let Some(object) = data.as_object() else {
        return false;
    };
    object.len() <= 5
        && object.iter().all(|(key, value)| {
            matches!(
                key.as_str(),
                "category" | "href" | "kind" | "ruleId" | "url"
            ) && match value {
                Value::Bool(_) | Value::Number(_) => true,
                Value::String(value) => {
                    valid_native_string(value, MAX_NATIVE_DIAGNOSTIC_METADATA_BYTES)
                }
                _ => false,
            }
        })
}

fn valid_retrieval_handle(handle: Option<&str>) -> bool {
    handle.is_none_or(|handle| {
        !handle.is_empty()
            && handle.len() <= MAX_CONTEXT_RETRIEVAL_HANDLE_BYTES
            && handle.bytes().all(|byte| byte.is_ascii_graphic())
    })
}

fn bounded_context_text(mut value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value;
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TRACEDECAY_CONTEXT_REVISION;
    use crate::bridge::{DaemonLspSessionTransport, FramePoll, FrameSend};
    use crate::capabilities::SemanticCapability;
    use crate::diagnostics::{DiagnosticSeverity, DiagnosticSource, LspPosition, LspRange};
    use crate::gateway::{FeedbackCycleRequest, LspLocation, SemanticProviderOutcome};
    use crate::overlay::{MAX_OVERLAY_BYTES, OverlaySnapshot};
    use crate::provider::GenerationDiagnostics;
    use std::cell::RefCell;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Feedback(RefCell<Vec<FeedbackCycleRequest>>);

    impl FeedbackCyclePort for Feedback {
        fn request_feedback_cycle(&self, request: FeedbackCycleRequest) -> FeedbackCycleResponse {
            self.0.borrow_mut().push(request);
            FeedbackCycleResponse::Accepted
        }
    }

    struct Semantics;

    impl SemanticProviderPort for Semantics {
        fn definition(
            &self,
            _root: &AdmittedRoot,
            uri: &str,
            _position: LspPosition,
        ) -> super::super::gateway::SemanticProviderOutcome<Vec<LspLocation>> {
            SemanticProviderOutcome::Complete(vec![LspLocation {
                uri: uri.into(),
                range: LspRange {
                    start: LspPosition {
                        line: 0,
                        character: 0,
                    },
                    end: LspPosition {
                        line: 0,
                        character: 0,
                    },
                },
            }])
        }
    }

    struct Diagnostics;

    impl DiagnosticSnapshotPort for Diagnostics {
        fn document_diagnostics(
            &self,
            _root: &AdmittedRoot,
            uri: &str,
            overlay: Option<&OverlaySnapshot>,
        ) -> DiagnosticSnapshotOutcome {
            assert!(overlay.is_none_or(|overlay| overlay.ephemeral));
            DiagnosticSnapshotOutcome::Ready {
                diagnostics: GenerationDiagnostics {
                    generation: 9,
                    upstream: vec![GatewayDiagnostic {
                        uri: uri.into(),
                        range: LspRange {
                            start: LspPosition {
                                line: 0,
                                character: 0,
                            },
                            end: LspPosition {
                                line: 0,
                                character: 1,
                            },
                        },
                        severity: Some(DiagnosticSeverity::Warning),
                        code: Some("warning".into()),
                        code_description_uri: None,
                        message: "bounded diagnostic".into(),
                        source: DiagnosticSource::Upstream,
                        related_information: Vec::new(),
                        data: None,
                    }],
                    tracedecay: Vec::new(),
                },
                completed_operation_id: None,
            }
        }
    }

    #[derive(Clone, Default)]
    struct CapturingContext {
        document_content_digest: Arc<Mutex<Option<ContentDigest>>>,
    }

    impl ContextProjectionPort for CapturingContext {
        fn registrations(&self) -> Vec<ContextProjectionRegistration> {
            vec![ContextProjectionRegistration {
                kind: ContextProjectionKind::test_run_results(),
                revision: TRACEDECAY_CONTEXT_REVISION,
            }]
        }

        fn snapshot(
            &self,
            _root: &AdmittedRoot,
            _request_id: &LspRequestId,
            request: &ContextProjectionRequest,
        ) -> ContextProjectionOutcome {
            *self
                .document_content_digest
                .lock()
                .expect("capture context request") = request.document_content_digest.clone();
            ContextProjectionOutcome::Deferred {
                reason: "captured".to_owned(),
            }
        }
    }

    fn session() -> DaemonLspProtocolSession<Feedback, Semantics, Diagnostics> {
        let capabilities = GatewayCapabilities::default();
        let upstream = UpstreamCapabilities {
            supports_diagnostics: true,
            semantic: SemanticCapability::ALL.into_iter().collect(),
        };
        let effective = negotiate_capabilities(
            &ClientCapabilities {
                supports_versioned_publish_diagnostics: true,
                publish_diagnostics_related_information: true,
                publish_diagnostics_code_description: true,
                publish_diagnostics_data: true,
                supports_document_diagnostics: true,
                workspace_diagnostic_refresh_support: true,
                semantic: SemanticCapability::ALL.into_iter().collect(),
                ..ClientCapabilities::default()
            },
            &capabilities,
            &upstream,
        );
        DaemonLspProtocolSession::new(
            DaemonLspGateway::with_semantic_provider(
                AdmittedRoot::new("file:///root"),
                effective,
                Feedback::default(),
                Semantics,
            ),
            capabilities,
            upstream,
            Diagnostics,
        )
    }

    fn initialize(session: &mut DaemonLspProtocolSession<Feedback, Semantics, Diagnostics>) {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "rootUri": "file:///root",
                "capabilities": {
                    "general": { "positionEncodings": ["utf-16"] },
                    "textDocument": {
                        "publishDiagnostics": {
                            "versionSupport": true,
                            "relatedInformation": true,
                            "codeDescriptionSupport": true,
                            "dataSupport": true
                        },
                        "diagnostic": {
                            "relatedInformation": true,
                            "codeDescriptionSupport": true,
                            "dataSupport": true
                        },
                        "definition": {},
                        "declaration": {},
                        "typeDefinition": {},
                        "implementation": {},
                        "references": {},
                        "hover": {},
                        "documentSymbol": {},
                        "signatureHelp": {},
                        "callHierarchy": {},
                        "typeHierarchy": {}
                    },
                    "workspace": {
                        "symbol": {},
                        "diagnostic": { "refreshSupport": true }
                    }
                }
            }
        });
        session.handle_payload(&serde_json::to_vec(&request).unwrap(), 0);
        let initial = session.drain_outbound();
        assert_eq!(initial.len(), 1);
        let response: Value = serde_json::from_slice(&initial[0]).unwrap();
        assert!(
            response["result"]["capabilities"]
                .get("renameProvider")
                .is_none()
        );
        session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
            1,
        );
        assert_eq!(session.lifecycle(), SessionLifecycle::Ready);
    }

    #[test]
    fn initialization_is_single_root_and_deferred_methods_are_typed_unavailable() {
        let mut session = session();
        initialize(&mut session);
        session.handle_payload(
            br#"{"jsonrpc":"2.0","id":2,"method":"textDocument/rename","params":{}}"#,
            2,
        );
        let output = session.drain_outbound();
        let response: Value = serde_json::from_slice(&output[0]).unwrap();
        assert_eq!(response["error"]["code"], -32601);
        assert_eq!(response["error"]["data"]["reason"], "explicitlyUnavailable");
    }

    #[test]
    fn failed_initialize_does_not_transition_or_admit_document_content() {
        let mut session = session();
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "rootUri": "file:///not-admitted",
                "capabilities": { "general": { "positionEncodings": ["utf-16"] } }
            }
        });
        session.handle_payload(&serde_json::to_vec(&request).unwrap(), 0);
        let response: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
        assert_eq!(response["error"]["code"], -32602);
        assert_eq!(session.lifecycle(), SessionLifecycle::AwaitingInitialize);

        session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
            1,
        );
        session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":1,"text":"secret"}}}"#,
            2,
        );
        assert_eq!(session.lifecycle(), SessionLifecycle::AwaitingInitialize);
        assert!(session.overlays().snapshot("file:///root/a.rs").is_none());

        initialize(&mut session);
    }

    #[test]
    fn context_request_binds_exact_session_overlay_digest() {
        let document_uri = "file:///root/a.rs";
        let mut overlays = OverlayStore::default();
        overlays
            .open(document_uri, "rust", 1, "fn dirty() {}")
            .expect("open overlay");
        let mut request = ContextProjectionRequest {
            kind: ContextProjectionKind::test_run_results(),
            document_uri: Some(document_uri.to_owned()),
            document_content_digest: None,
        };

        bind_context_document_digest(&mut request, &overlays);

        assert_eq!(
            request.document_content_digest,
            Some(ContentDigest::of_bytes(b"fn dirty() {}"))
        );
        assert!(
            serde_json::from_value::<ContextProjectionRequest>(json!({
                "kind": "testRunResults",
                "documentUri": document_uri,
                "documentContentDigest": format!("sha256:{}", "a".repeat(64)),
            }))
            .is_err(),
            "clients cannot supply the trusted overlay digest"
        );
    }

    #[test]
    fn document_change_invalidates_context_currentness_before_expansion() {
        let document_uri = "file:///root/a.rs";
        let mut session = session();
        initialize(&mut session);
        session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":1,"text":"fn a() {}"}}}"#,
            1,
        );
        session.context_currentness.insert(
            (
                ContextProjectionKind::test_run_results(),
                Some(document_uri.to_owned()),
            ),
            ContextProjectionCurrentness {
                generation: 7,
                identity: ContextProjectionIdentity {
                    head_commit_id: "0123456789abcdef".to_owned(),
                    code_generation_id: "generation:7".to_owned(),
                    snapshot_digest: format!("sha256:{}", "a".repeat(64)),
                    invalidation_digest: format!("sha256:{}", "b".repeat(64)),
                    snapshot_content_digest: format!("sha256:{}", "c".repeat(64)),
                    document_content_digest: Some(format!("sha256:{}", "d".repeat(64))),
                },
            },
        );

        session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didChange","params":{"textDocument":{"uri":"file:///root/a.rs","version":2},"contentChanges":[{"text":"fn b() {}"}]}}"#,
            2,
        );

        assert!(
            !session
                .context_currentness
                .keys()
                .any(|(_, uri)| uri.as_deref() == Some(document_uri))
        );
    }

    #[test]
    fn context_dispatch_supplies_session_overlay_digest_to_canonical_reader() {
        let captured = CapturingContext::default();
        let observed = Arc::clone(&captured.document_content_digest);
        let mut capabilities = GatewayCapabilities::default();
        capabilities.context_projections.insert(
            ContextProjectionKind::test_run_results(),
            TRACEDECAY_CONTEXT_REVISION,
        );
        let upstream = UpstreamCapabilities::default();
        let effective =
            negotiate_capabilities(&ClientCapabilities::default(), &capabilities, &upstream);
        let mut session = DaemonLspProtocolSession::new(
            DaemonLspGateway::with_semantic_provider(
                AdmittedRoot::new("file:///root"),
                effective,
                Feedback::default(),
                Semantics,
            ),
            capabilities,
            upstream,
            Diagnostics,
        )
        .with_context_projection_port(captured);
        session.handle_payload(
            &serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "rootUri": "file:///root",
                    "capabilities": {
                        "general": { "positionEncodings": ["utf-16"] },
                        "experimental": {
                            "tracedecay": {
                                "revision": TRACEDECAY_CONTEXT_REVISION,
                                "projections": [{
                                    "kind": "testRunResults",
                                    "revision": TRACEDECAY_CONTEXT_REVISION
                                }],
                                "opaqueExpansion": false
                            }
                        }
                    }
                }
            }))
            .expect("initialize request"),
            0,
        );
        session.drain_outbound();
        session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
            1,
        );
        session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":1,"text":"fn dirty() {}"}}}"#,
            2,
        );
        session.handle_payload(
            br#"{"jsonrpc":"2.0","id":2,"method":"tracedecay/context","params":{"kind":"testRunResults","documentUri":"file:///root/a.rs"}}"#,
            3,
        );

        assert_eq!(
            *observed.lock().expect("read captured context request"),
            Some(ContentDigest::of_bytes(b"fn dirty() {}"))
        );
    }

    #[test]
    fn malformed_position_encoding_initialize_is_retryable() {
        let mut session = session();
        session.handle_payload(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":"file:///root","capabilities":{"general":{"positionEncodings":"utf-16"}}}}"#,
            0,
        );
        let response: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
        assert_eq!(response["error"]["code"], -32602);
        assert_eq!(session.lifecycle(), SessionLifecycle::AwaitingInitialize);
        initialize(&mut session);
    }

    #[test]
    fn outbound_backpressure_cannot_half_commit_initialize() {
        let mut session = session();
        session.outbound.push_back(QueuedFrame {
            payload: vec![0; MAX_QUEUED_OUTBOUND_BYTES],
            publication: None,
            server_request: None,
        });
        session.queued_outbound_bytes = MAX_QUEUED_OUTBOUND_BYTES;
        session.handle_payload(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":"file:///root","capabilities":{"general":{"positionEncodings":["utf-16"]}}}}"#,
            0,
        );
        assert_eq!(session.lifecycle(), SessionLifecycle::AwaitingInitialize);
        session.drain_outbound();
        initialize(&mut session);
    }

    #[test]
    fn save_flushes_pending_overlay_diagnostics() {
        let mut session = session();
        initialize(&mut session);
        session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":1,"text":"fn a() {}"}}}"#,
            10,
        );
        session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didSave","params":{"textDocument":{"uri":"file:///root/a.rs"}}}"#,
            11,
        );

        let messages = session.drain_outbound();
        let publication: Value = messages
            .iter()
            .map(|message| serde_json::from_slice(message).unwrap())
            .find(|message: &Value| message["method"] == "textDocument/publishDiagnostics")
            .unwrap();
        assert_eq!(publication["params"]["version"], 1);
    }

    #[test]
    fn publish_diagnostics_omits_unnegotiated_optional_fields() {
        let mut session = session();
        session.handle_payload(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":"file:///root","capabilities":{"general":{"positionEncodings":["utf-16"]},"textDocument":{"publishDiagnostics":{"versionSupport":true}}}}}"#,
            0,
        );
        session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
            1,
        );
        session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":4,"text":"fn a() {}"}}}"#,
            2,
        );
        session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didSave","params":{"textDocument":{"uri":"file:///root/a.rs"}}}"#,
            3,
        );

        let publication: Value = session
            .drain_outbound()
            .iter()
            .map(|message| serde_json::from_slice(message).unwrap())
            .find(|message: &Value| message["method"] == "textDocument/publishDiagnostics")
            .expect("baseline publish diagnostics remains available");
        assert_eq!(publication["params"]["version"], 4);
        let diagnostic = &publication["params"]["diagnostics"][0];
        assert!(diagnostic.get("relatedInformation").is_none());
        assert!(diagnostic.get("codeDescription").is_none());
        assert!(diagnostic.get("data").is_none());
    }

    #[test]
    fn related_locations_are_limited_to_the_admitted_root() {
        let session = session();
        let diagnostic = GatewayDiagnostic {
            uri: "file:///root/a.rs".to_owned(),
            range: LspRange {
                start: LspPosition {
                    line: 0,
                    character: 0,
                },
                end: LspPosition {
                    line: 0,
                    character: 1,
                },
            },
            severity: Some(DiagnosticSeverity::Information),
            code: Some("github-review".to_owned()),
            code_description_uri: None,
            message: "review".to_owned(),
            source: DiagnosticSource::TraceDecayGitHub,
            related_information: vec![
                super::super::diagnostics::GatewayDiagnosticRelatedInformation {
                    uri: "file:///root/caller.rs".to_owned(),
                    range: LspRange {
                        start: LspPosition {
                            line: 1,
                            character: 0,
                        },
                        end: LspPosition {
                            line: 1,
                            character: 1,
                        },
                    },
                    message: "authorized".to_owned(),
                },
                super::super::diagnostics::GatewayDiagnosticRelatedInformation {
                    uri: "file:///other/secret.rs".to_owned(),
                    range: LspRange {
                        start: LspPosition {
                            line: 1,
                            character: 0,
                        },
                        end: LspPosition {
                            line: 1,
                            character: 1,
                        },
                    },
                    message: "outside root".to_owned(),
                },
            ],
            data: None,
        };

        let visible = session.visible_diagnostics(vec![diagnostic], true);
        assert_eq!(visible[0].related_information.len(), 1);
        assert_eq!(
            visible[0].related_information[0].uri,
            "file:///root/caller.rs"
        );
    }

    #[test]
    fn managed_diagnostics_require_negotiated_data_identity() {
        let session = session();
        let diagnostic = |source| GatewayDiagnostic {
            uri: "file:///root/a.rs".to_owned(),
            range: LspRange {
                start: LspPosition {
                    line: 0,
                    character: 0,
                },
                end: LspPosition {
                    line: 0,
                    character: 1,
                },
            },
            severity: Some(DiagnosticSeverity::Information),
            code: Some("finding".to_owned()),
            code_description_uri: None,
            message: "finding".to_owned(),
            source,
            related_information: Vec::new(),
            data: None,
        };

        let visible = session.visible_diagnostics(
            vec![
                diagnostic(DiagnosticSource::Upstream),
                diagnostic(DiagnosticSource::TraceDecayGitHub),
            ],
            false,
        );

        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].source, DiagnosticSource::Upstream);
    }

    #[test]
    fn cursor_native_diagnostics_are_merged_but_not_republished() {
        let mut session = session();
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "rootUri": "file:///root",
                "initializationOptions": {
                    "tracedecay": {
                        "mode": "cursor-native",
                        "context": true
                    }
                },
                "capabilities": {
                    "general": { "positionEncodings": ["utf-16"] },
                    "textDocument": {
                        "publishDiagnostics": {
                            "versionSupport": true,
                            "relatedInformation": true,
                            "codeDescriptionSupport": true,
                            "dataSupport": true
                        },
                        "diagnostic": {
                            "relatedInformation": true,
                            "codeDescriptionSupport": true,
                            "dataSupport": true
                        }
                    },
                    "workspace": {
                        "diagnostic": { "refreshSupport": true }
                    }
                }
            }
        });
        session.handle_payload(&serde_json::to_vec(&request).unwrap(), 0);
        session.drain_outbound();
        session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
            1,
        );
        session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":7,"text":"fn a() {}"}}}"#,
            10,
        );
        session.drain_outbound();
        session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"tracedecay/nativeDiagnostics","params":{"uri":"file:///root/a.rs","version":7,"diagnostics":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"source":"tracedecay","message":"projected diagnostic"},{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"source":"TraceDecay-CI","message":"projected CI diagnostic"},{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"source":"tracedecay-proximity","message":"projected proximity diagnostic"},{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"source":"typescript","message":"native diagnostic","data":{"ruleId":"typescript"}}]}}"#,
            11,
        );
        assert_eq!(
            session
                .native_upstream
                .get("file:///root/a.rs")
                .unwrap()
                .diagnostics
                .len(),
            1
        );
        assert_eq!(
            session.native_upstream["file:///root/a.rs"].diagnostics[0].message,
            "native diagnostic",
            "TraceDecay-projected sources must never be inverted into native upstream evidence"
        );
        session.detach().unwrap();
        session.reconnect().unwrap();
        assert!(
            session.native_upstream.contains_key("file:///root/a.rs"),
            "reconnect preserves the current in-memory native diagnostic lane"
        );
        session.flush_due(61);

        let messages = session.drain_outbound();
        let publication: Value = messages
            .iter()
            .map(|message| serde_json::from_slice(message).unwrap())
            .find(|message: &Value| message["method"] == "textDocument/publishDiagnostics")
            .unwrap();
        assert!(
            publication["params"]["diagnostics"]
                .as_array()
                .unwrap()
                .is_empty(),
            "Cursor-native clients must not receive their own diagnostics back"
        );
        let projected_only = br#"{"jsonrpc":"2.0","method":"tracedecay/nativeDiagnostics","params":{"uri":"file:///root/a.rs","version":7,"diagnostics":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"source":"tracedecay-github","message":"projected diagnostic"}]}}"#;
        session.handle_payload(projected_only, 62);
        session.flush_due(62);
        session.drain_outbound();
        assert!(
            session.native_upstream["file:///root/a.rs"]
                .diagnostics
                .is_empty(),
            "a projected-only native publication clears prior upstream evidence once"
        );
        session.handle_payload(projected_only, 63);
        assert_eq!(
            session.flush_due(10_000).queued_messages,
            0,
            "an unchanged projected-only publication must not start a refresh loop"
        );
        session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"tracedecay/nativeDiagnostics","params":{"uri":"file:///root/a.rs","version":7,"diagnostics":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"source":"typescript","message":"changed native diagnostic"}]}}"#,
            10_001,
        );
        assert_eq!(
            session.native_upstream["file:///root/a.rs"].diagnostics[0].message,
            "changed native diagnostic",
            "duplicate suppression must not suppress a real native evidence change"
        );
        session.drain_outbound();
        session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didClose","params":{"textDocument":{"uri":"file:///root/a.rs"}}}"#,
            10_002,
        );
        assert!(
            !session.native_upstream.contains_key("file:///root/a.rs"),
            "closing a document clears its native diagnostic lane"
        );
    }

    #[test]
    fn exit_releases_session_local_overlays_and_queued_frames() {
        let mut session = session();
        initialize(&mut session);
        session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":1,"text":"secret"}}}"#,
            2,
        );
        session.handle_payload(
            br#"{"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}"#,
            3,
        );
        session.handle_payload(br#"{"jsonrpc":"2.0","method":"exit","params":{}}"#, 4);

        assert_eq!(session.lifecycle(), SessionLifecycle::Exited);
        assert!(session.overlays().snapshot("file:///root/a.rs").is_none());
        assert!(session.drain_outbound().is_empty());
    }

    #[test]
    fn overlays_debounce_publish_and_do_not_become_clean_generation_state() {
        let mut session = session();
        initialize(&mut session);
        session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":1,"text":"fn a() {}"}}}"#,
            10,
        );
        session.drain_outbound();
        session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didChange","params":{"textDocument":{"uri":"file:///root/a.rs","version":2},"contentChanges":[{"text":"fn b() {}"}]}}"#,
            40,
        );
        assert_eq!(session.flush_due(89).queued_messages, 0);
        let output = session.flush_due(90);
        assert!(output.queued_messages >= 1);
        let messages = session.drain_outbound();
        let publication: Value = messages
            .iter()
            .map(|message| serde_json::from_slice(message).unwrap())
            .find(|message: &Value| message["method"] == "textDocument/publishDiagnostics")
            .unwrap();
        assert_eq!(publication["params"]["version"], 2);
        assert!(
            session
                .overlays()
                .snapshot("file:///root/a.rs")
                .unwrap()
                .ephemeral
        );
    }

    #[test]
    fn close_then_reopen_resets_debounce_and_publication_version_ordering() {
        let mut session = session();
        initialize(&mut session);
        session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":10,"text":"old"}}}"#,
            10,
        );
        session.flush_due(60);
        session.drain_outbound();
        session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didClose","params":{"textDocument":{"uri":"file:///root/a.rs"}}}"#,
            61,
        );
        session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":1,"text":"new"}}}"#,
            62,
        );
        session.flush_due(112);

        let messages = session.drain_outbound();
        let publication: Value = messages
            .iter()
            .map(|message| serde_json::from_slice(message).unwrap())
            .find(|message: &Value| message["method"] == "textDocument/publishDiagnostics")
            .unwrap();
        assert_eq!(publication["params"]["version"], 1);
        assert_eq!(
            publication["params"]["diagnostics"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn server_refresh_responses_do_not_create_json_rpc_response_loops() {
        let mut session = session();
        initialize(&mut session);
        session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":1,"text":"x"}}}"#,
            10,
        );
        session.flush_due(60);
        let messages = session.drain_outbound();
        let refresh: Value = messages
            .iter()
            .map(|message| serde_json::from_slice(message).unwrap())
            .find(|message: &Value| message["method"] == "workspace/diagnostic/refresh")
            .unwrap();
        let response = json!({
            "jsonrpc": "2.0",
            "id": refresh["id"].clone(),
            "result": null,
        });
        session.handle_payload(&serde_json::to_vec(&response).unwrap(), 61);
        assert!(session.drain_outbound().is_empty());
    }

    #[test]
    fn exit_request_is_rejected_without_closing_the_session() {
        let mut session = session();
        initialize(&mut session);
        session.handle_payload(
            br#"{"jsonrpc":"2.0","id":9,"method":"exit","params":{}}"#,
            2,
        );
        let response: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
        assert_eq!(response["error"]["code"], -32600);
        assert_eq!(session.lifecycle(), SessionLifecycle::Ready);
    }

    #[test]
    fn pull_diagnostics_are_generation_bound_and_return_unchanged_for_same_result_id() {
        let mut session = session();
        initialize(&mut session);
        session.handle_payload(
            br#"{"jsonrpc":"2.0","id":3,"method":"textDocument/diagnostic","params":{"textDocument":{"uri":"file:///root/a.rs"}}}"#,
            10,
        );
        let first = session.drain_outbound();
        let first: Value = serde_json::from_slice(&first[0]).unwrap();
        let result_id = first["result"]["resultId"].as_str().unwrap().to_owned();
        assert_eq!(result_id, "generation:9:version:0");
        let request = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "textDocument/diagnostic",
            "params": {
                "textDocument": { "uri": "file:///root/a.rs" },
                "previousResultId": result_id,
            },
        });
        session.handle_payload(&serde_json::to_vec(&request).unwrap(), 11);
        let second = session.drain_outbound();
        let second: Value = serde_json::from_slice(&second[0]).unwrap();
        assert_eq!(second["result"]["kind"], "unchanged");
    }

    #[test]
    fn bridge_transport_parses_typed_session_frames_and_acks_delivery() {
        let mut transport = DaemonLspProtocolTransport::new(session());
        transport.set_now_ms(0);
        assert_eq!(
            transport
                .try_send_client_frame(
                    br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":"file:///root","capabilities":{"general":{"positionEncodings":["utf-16"]}}}}"#,
                )
                .unwrap(),
            FrameSend::Sent
        );
        assert!(matches!(
            transport.poll_daemon_frame().unwrap(),
            FramePoll::Frame(frame) if serde_json::from_slice::<Value>(&frame).unwrap()["id"] == 1
        ));
        transport.acknowledge_daemon_frame().unwrap();
        assert_eq!(transport.poll_daemon_frame().unwrap(), FramePoll::Pending);
    }

    #[test]
    fn in_flight_publication_is_not_replaced_or_used_to_ack_a_newer_version() {
        let mut session = session();
        initialize(&mut session);
        session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":1,"text":"a"}}}"#,
            10,
        );
        assert!(session.poll_outbound().is_some());
        session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didChange","params":{"textDocument":{"uri":"file:///root/a.rs","version":2},"contentChanges":[{"text":"b"}]}}"#,
            11,
        );
        session.flush_due(61);

        assert_eq!(
            session
                .outbound
                .iter()
                .filter(|frame| frame.publication.is_some())
                .count(),
            2
        );
        assert!(session.acknowledge_outbound());
        assert_eq!(
            session
                .control
                .publication("file:///root/a.rs")
                .unwrap()
                .delivery,
            super::super::session::PublicationDelivery::Queued
        );
        assert!(session.poll_outbound().is_some());
        assert!(session.acknowledge_outbound());
        assert_eq!(
            session
                .control
                .publication("file:///root/a.rs")
                .unwrap()
                .delivery,
            super::super::session::PublicationDelivery::BridgeAcknowledged
        );
    }

    #[test]
    fn oversized_overlay_closes_before_the_bridge_acknowledges_the_notification() {
        let mut transport = DaemonLspProtocolTransport::new(session());
        transport.session_mut().handle_payload(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":"file:///root","capabilities":{"general":{"positionEncodings":["utf-16"]}}}}"#,
            0,
        );
        transport.session_mut().drain_outbound();
        transport.session_mut().handle_payload(
            br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
            1,
        );

        let notification = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": "file:///root/oversized.rs",
                    "languageId": "rust",
                    "version": 1,
                    "text": "x".repeat(MAX_OVERLAY_BYTES + 1),
                }
            }
        });
        let notification = serde_json::to_vec(&notification).unwrap();

        assert_eq!(
            transport.try_send_client_frame(&notification).unwrap(),
            FrameSend::Closed
        );
        assert_eq!(transport.session().lifecycle(), SessionLifecycle::Expired);
        assert!(
            transport
                .session()
                .overlays()
                .snapshot("file:///root/oversized.rs")
                .is_none()
        );
    }
}
