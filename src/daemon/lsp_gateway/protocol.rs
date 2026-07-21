//! Typed JSON-RPC 2.0 / LSP 3.17 session actor.
//!
//! The actor accepts already-authenticated, already-framed payloads from the
//! bridge. It is intentionally not a raw socket tunnel: every accepted method
//! is parsed, lifecycle-gated, root-gated, bounded, and dispatched through a
//! typed gateway/provider port.

use std::collections::{BTreeMap, VecDeque};
use std::convert::Infallible;

use serde_json::{Map, Value, json};

use super::capabilities::{
    CapabilityAvailability, CapabilityParseError, ClientCapabilities, GatewayCapabilities,
    UpstreamCapabilities, negotiate_capabilities,
};
use super::diagnostics::{
    DiagnosticMerge, DiagnosticSeverity, DocumentDiagnosticReport, GatewayDiagnostic, LspPosition,
    LspRange,
};
use super::dispatch::{dispatch_incoming, parse_incoming};
use super::gateway::{
    AdmittedRoot, CallHierarchyItem, DaemonLspGateway, FeedbackCyclePort, FeedbackCycleResponse,
    GatewayDocumentDiagnostics, GatewayMethod, LspLocation, MethodUnavailableReason,
    SemanticProviderPort, TypeHierarchyItem,
};
use super::overlay::{
    DebouncedDiagnosticKind, OverlayDiagnosticDebouncer, OverlayError, OverlayStore,
};
use super::provider::{
    DiagnosticSnapshotOutcome, DiagnosticSnapshotPort, UnavailableDiagnosticSnapshotProvider,
};
use super::rpc::{
    RpcFailure, diagnostic_result_id, diagnostic_value, document_diagnostic_report_value,
    document_position, document_uri, error_response, gateway_diagnostic_value,
    incoming_calls_value, initialized_root_uri, outgoing_calls_value, overlay_failure,
    parse_call_item, parse_overlay_change, parse_type_item, request_id, request_id_value,
    required_i64, required_nonempty_string, required_string, response_value, success_response,
    text_document, type_items_value,
};
use super::session::{
    CancellationOutcome, CompletionDisposition, LifecycleError, LspRequestFailure, LspRequestId,
    LspSessionControl, MAX_PUBLICATION_BYTES, PublicationAdmission, SessionLifecycle,
};
use crate::lsp_bridge::{
    DaemonLspSessionTransport, FramePoll, FrameSend, LspFrame, MAX_LSP_FRAME_BYTES,
};

/// A protocol actor allows bounded synchronous work before returning a typed
/// cancellation response. Long-running adapters receive the same deadline via
/// their daemon-owned runtime contracts.
pub const DEFAULT_LSP_REQUEST_DEADLINE_MS: u64 = 5_000;
/// Session-local queued outbound bytes. The bridge retains one additional
/// frame per direction while its peer is backpressured.
pub const MAX_QUEUED_OUTBOUND_BYTES: usize = 1024 * 1024;
pub const MAX_QUEUED_OUTBOUND_MESSAGES: usize = 64;
const MIN_CLIENT_FRAME_OUTBOUND_RESERVE: usize = MAX_PUBLICATION_BYTES;

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
    result_id: String,
}

/// One authenticated daemon LSP session. It owns no durable state and is
/// dropped alongside its registry entry after TTL expiry.
pub struct DaemonLspProtocolSession<P, S, D = UnavailableDiagnosticSnapshotProvider>
where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    pub(super) gateway: DaemonLspGateway<P, S>,
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
    request_deadline_ms: u64,
    next_server_request_id: u64,
    diagnostic_refresh_request: Option<LspRequestId>,
    diagnostic_refresh_needed: bool,
}

/// Concrete bridge-facing adapter for one typed daemon session actor. It
/// parses each client payload through [`DaemonLspProtocolSession`] and exposes
/// only queued LSP frames back to the bridge; it cannot become a raw daemon
/// socket tunnel.
pub struct DaemonLspProtocolTransport<P, S, D = UnavailableDiagnosticSnapshotProvider>
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
            request_deadline_ms: DEFAULT_LSP_REQUEST_DEADLINE_MS,
            next_server_request_id: 1,
            diagnostic_refresh_request: None,
            diagnostic_refresh_needed: false,
        }
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
        self.control.cancel_request(id)
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
        self.overlays.clear();
        self.debounce.clear();
        self.outbound.clear();
        self.outbound_in_flight = false;
        self.queued_outbound_bytes = 0;
        self.published.clear();
        self.diagnostic_refresh_request = None;
        self.diagnostic_refresh_needed = false;
    }

    fn dispatch_value(&mut self, value: Value, now_ms: u64) {
        match parse_incoming(value) {
            Ok(incoming) => dispatch_incoming(self, incoming, now_ms),
            Err((response_id, failure)) => {
                self.enqueue_value(error_response(response_id, failure));
            }
        }
    }

    pub(super) fn handle_initialized_notification(&mut self) {
        let _ = self.control.initialized();
    }

    pub(super) fn handle_initialized_request(&mut self, response_id: Value) {
        let _ = self.enqueue_value(error_response(
            response_id,
            RpcFailure {
                code: -32600,
                message: "Invalid Request",
                data: json!({ "detail": "initialized must be a notification" }),
            },
        ));
    }

    pub(super) fn handle_shutdown_request(&mut self, response_id: Value) {
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

    pub(super) fn handle_exit_notification(&mut self) {
        if self.control.exit().is_err() {
            self.expire();
        } else {
            self.clear_volatile_state();
        }
    }

    pub(super) fn handle_exit_request(&mut self, response_id: Value) {
        let _ = self.enqueue_value(error_response(
            response_id,
            RpcFailure {
                code: -32600,
                message: "Invalid Request",
                data: json!({ "detail": "exit must be a notification" }),
            },
        ));
    }

    pub(super) fn handle_client_response(&mut self, id: LspRequestId) {
        if self.diagnostic_refresh_request.as_ref() == Some(&id) {
            self.diagnostic_refresh_request = None;
        }
        if self.diagnostic_refresh_needed {
            self.queue_diagnostic_refresh();
        }
    }

    pub(super) fn document_version(&self, uri: &str) -> i64 {
        self.overlays.version(uri).unwrap_or_default()
    }

    pub(super) fn handle_initialize(&mut self, id: Value, params: &Value) {
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
        if root_uri != self.gateway.root().uri() {
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
        };
        let effective = negotiate_capabilities(
            &client,
            &self.gateway_capabilities,
            &self.upstream_capabilities,
        );
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
        self.gateway
            .bind_initialized_capabilities(effective.clone());
    }

    pub(super) fn handle_cancel(&mut self, params: &Value) {
        let Some(id) = params.get("id").and_then(request_id) else {
            return;
        };
        let _ = self.control.cancel_request(&id);
    }

    pub(super) fn handle_did_open(&mut self, params: &Value, now_ms: u64) -> Result<(), RpcFailure> {
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

    pub(super) fn handle_did_change(&mut self, params: &Value, now_ms: u64) -> Result<(), RpcFailure> {
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
        self.control.supersede_document(&uri, snapshot.version);
        if !self
            .debounce
            .schedule_refresh(uri, snapshot.version, now_ms)
        {
            return Err(self.close_for_debounce_overflow());
        }
        Ok(())
    }

    pub(super) fn handle_did_close(&mut self, params: &Value, now_ms: u64) -> Result<(), RpcFailure> {
        self.require_ready()?;
        let uri = required_nonempty_string(text_document(params)?, "uri")?;
        self.require_document_root(&uri)?;
        let closed = self.overlays.close(&uri).map_err(overlay_failure)?;
        self.control
            .supersede_document(&uri, closed.version.saturating_add(1));
        if !self.debounce.schedule_clear(uri, closed.version, now_ms) {
            return Err(self.close_for_debounce_overflow());
        }
        Ok(())
    }

    pub(super) fn handle_did_save(&mut self, params: &Value, now_ms: u64) -> Result<(), RpcFailure> {
        self.require_ready()?;
        let uri = required_nonempty_string(text_document(params)?, "uri")?;
        self.require_document_root(&uri)?;
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

    pub(super) fn handle_position_request(
        &mut self,
        id: Value,
        params: &Value,
        now_ms: u64,
        route: impl FnOnce(&DaemonLspGateway<P, S>, &str, LspPosition) -> Result<Value, RpcFailure>,
    ) {
        let parsed = document_position(params);
        match parsed {
            Ok((uri, position)) => {
                let version = self.overlays.version(&uri).unwrap_or_default();
                self.with_request(id, Some((uri.clone(), version)), now_ms, move |session| {
                    route(&session.gateway, &uri, position)
                });
            }
            Err(error) => {
                let _ = self.enqueue_value(error_response(id, error));
            }
        }
    }

    pub(super) fn handle_document_request(
        &mut self,
        id: Value,
        params: &Value,
        now_ms: u64,
        route: impl FnOnce(&DaemonLspGateway<P, S>, &str) -> Result<Value, RpcFailure>,
    ) {
        match document_uri(params) {
            Ok(uri) => {
                let version = self.overlays.version(&uri).unwrap_or_default();
                self.with_request(id, Some((uri.clone(), version)), now_ms, move |session| {
                    route(&session.gateway, &uri)
                });
            }
            Err(error) => {
                let _ = self.enqueue_value(error_response(id, error));
            }
        }
    }

    pub(super) fn handle_call_request(&mut self, id: Value, params: &Value, now_ms: u64, incoming: bool) {
        let item = params
            .get("item")
            .ok_or_else(|| RpcFailure::invalid_params("item is required"));
        match item.and_then(parse_call_item) {
            Ok(item) => {
                let uri = item.uri.clone();
                let version = self.overlays.version(&uri).unwrap_or_default();
                self.with_request(id, Some((uri, version)), now_ms, move |session| {
                    if incoming {
                        response_value(session.gateway.incoming_calls(&item), incoming_calls_value)
                    } else {
                        response_value(session.gateway.outgoing_calls(&item), outgoing_calls_value)
                    }
                });
            }
            Err(error) => {
                let _ = self.enqueue_value(error_response(id, error));
            }
        }
    }

    pub(super) fn handle_type_request(&mut self, id: Value, params: &Value, now_ms: u64, supertypes: bool) {
        let item = params
            .get("item")
            .ok_or_else(|| RpcFailure::invalid_params("item is required"));
        match item.and_then(parse_type_item) {
            Ok(item) => {
                let uri = item.uri.clone();
                let version = self.overlays.version(&uri).unwrap_or_default();
                self.with_request(id, Some((uri, version)), now_ms, move |session| {
                    if supertypes {
                        response_value(
                            session.gateway.type_hierarchy_supertypes(&item),
                            type_items_value,
                        )
                    } else {
                        response_value(
                            session.gateway.type_hierarchy_subtypes(&item),
                            type_items_value,
                        )
                    }
                });
            }
            Err(error) => {
                let _ = self.enqueue_value(error_response(id, error));
            }
        }
    }

    pub(super) fn with_request(
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

    pub(super) fn pull_diagnostics(&mut self, uri: &str, params: &Value) -> Result<Value, RpcFailure> {
        self.require_document_root(uri)?;
        if !self.gateway.capabilities().supports_document_diagnostics {
            return Err(RpcFailure::unavailable(
                GatewayMethod::TextDocumentDiagnostic.as_lsp_method(),
                MethodUnavailableReason::CapabilityNotNegotiated,
            ));
        }
        let overlay = self.overlays.snapshot(uri);
        let outcome =
            self.diagnostics
                .document_diagnostics(self.gateway.root(), uri, overlay.as_ref());
        let (diagnostics, coverage) = match outcome {
            DiagnosticSnapshotOutcome::Complete(diagnostics) => (diagnostics, None),
            DiagnosticSnapshotOutcome::Partial {
                diagnostics,
                coverage,
            } => (diagnostics, Some(coverage)),
            DiagnosticSnapshotOutcome::Unavailable => {
                return Err(RpcFailure::unavailable(
                    GatewayMethod::TextDocumentDiagnostic.as_lsp_method(),
                    MethodUnavailableReason::ProviderUnavailable,
                ));
            }
        };
        let version = overlay.as_ref().map_or(0, |overlay| overlay.version);
        let generation = diagnostics.generation;
        let result_id = diagnostic_result_id(generation, version);
        let response = self.gateway.document_diagnostics(
            uri,
            result_id.clone(),
            diagnostics.upstream,
            diagnostics.tracedecay,
        );
        let value = response_value(response, gateway_diagnostic_value)?;
        if coverage.is_some() {
            // A standard pull diagnostic report cannot represent partial
            // coverage truthfully, so do not return its item list as clean.
            return Err(RpcFailure {
                code: -32802,
                message: "Server cancelled request",
                data: json!({ "retriggerRequest": true, "coverage": coverage }),
            });
        }
        let previous = params.get("previousResultId").and_then(Value::as_str);
        let still_current = self.published.get(uri).is_some_and(|published| {
            published.version == version
                && published.generation == generation
                && published.result_id == result_id
        });
        if previous == Some(result_id.as_str()) && (still_current || overlay.is_none()) {
            return Ok(document_diagnostic_report_value(
                DocumentDiagnosticReport::Unchanged { result_id },
            ));
        }
        if overlay.is_some() {
            self.published.insert(
                uri.to_owned(),
                PublishedDiagnostic {
                    version,
                    generation,
                    result_id,
                },
            );
        }
        Ok(value)
    }

    fn flush_debounced_diagnostics(&mut self, now_ms: u64) {
        if self.control.lifecycle() != SessionLifecycle::Ready {
            return;
        }
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
                    let DiagnosticSnapshotOutcome::Complete(snapshot) = outcome else {
                        // Partial/unavailable state is never published as a
                        // plausible clean empty set. A later save/pull may
                        // return a typed state through its owning adapter.
                        continue;
                    };
                    let generation = snapshot.generation;
                    let merged = DiagnosticMerge::for_document(
                        &scheduled.uri,
                        snapshot.upstream,
                        snapshot.tracedecay,
                    );
                    if !self.publish_diagnostics(
                        &scheduled.uri,
                        scheduled.version,
                        generation,
                        merged.items,
                    ) {
                        continue;
                    }
                    self.published.insert(
                        scheduled.uri.clone(),
                        PublishedDiagnostic {
                            version: scheduled.version,
                            generation,
                            result_id: diagnostic_result_id(generation, scheduled.version),
                        },
                    );
                    self.queue_diagnostic_refresh();
                }
            }
        }
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
        let value = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": uri,
                "version": version,
                "diagnostics": diagnostics.into_iter().map(diagnostic_value).collect::<Vec<_>>(),
            },
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
        let id = LspRequestId::String(format!(
            "tracedecay-diagnostic-refresh-{}",
            self.next_server_request_id
        ));
        self.next_server_request_id = self.next_server_request_id.saturating_add(1);
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

    fn expire_requests(&mut self, now_ms: u64) {
        for id in self.control.expire_deadlines(now_ms) {
            let disposition = self.control.complete_request(&id);
            if let Some(failure) = disposition.failure() {
                self.enqueue_value(error_response(
                    request_id_value(id),
                    RpcFailure::request_failure(failure),
                ));
            }
        }
    }

    pub(super) fn enqueue_value(&mut self, value: Value) -> bool {
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

#[cfg(test)]
mod tests {
    use super::super::capabilities::SemanticCapability;
    use super::super::diagnostics::{DiagnosticSource, LspPosition};
    use super::super::gateway::{FeedbackCycleRequest, SemanticProviderOutcome};
    use super::super::overlay::{MAX_OVERLAY_BYTES, OverlaySnapshot};
    use super::super::provider::GenerationDiagnostics;
    use super::*;
    use crate::lsp_bridge::{DaemonLspSessionTransport, FramePoll, FrameSend};
    use std::cell::RefCell;

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
            assert!(overlay.is_none() || overlay.is_some_and(|overlay| overlay.ephemeral));
            DiagnosticSnapshotOutcome::Complete(GenerationDiagnostics {
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
                    message: "bounded diagnostic".into(),
                    source: DiagnosticSource::Upstream,
                }],
                tracedecay: Vec::new(),
            })
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
                document_diagnostics_related_information: true,
                document_diagnostics_code_description: true,
                document_diagnostics_data: true,
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
        assert!(session.flush_due(89).queued_messages == 0);
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
