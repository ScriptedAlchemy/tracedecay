//! Typed JSON-RPC 2.0 / LSP 3.17 session actor.
//!
//! The actor accepts already-authenticated, already-framed payloads from the
//! bridge. It is intentionally not a raw socket tunnel: every accepted method
//! is parsed, lifecycle-gated, root-gated, bounded, and dispatched through a
//! typed gateway/provider port.

use std::collections::{BTreeMap, BTreeSet};
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
    GatewayCapabilities, UpstreamCapabilities, is_supported_context_projection,
    negotiate_capabilities,
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
    document_diagnostic_report_value, error_response, initialized_workspace_uris, overlay_failure,
    parse_overlay_change, partial_failure_data, request_id, request_id_value, required_i64,
    required_nonempty_string, required_string, response_value, semantic_response_value,
    success_response, text_document,
};
use crate::session::{
    AuthorizedLspWorkspace, CancellationOutcome, CompletionDisposition, LifecycleError,
    LspRequestFailure, LspRequestId, LspSessionControl, MAX_PUBLICATION_BYTES,
    PublicationAdmission, SessionLifecycle,
};

/// A protocol actor allows bounded synchronous work before returning a typed
/// cancellation response. Long-running adapters receive the same deadline via
/// their daemon-owned runtime contracts.
pub const DEFAULT_LSP_REQUEST_DEADLINE_MS: u64 = 5_000;
/// Session-local queued outbound bytes. The bridge retains one additional
/// frame per direction while its peer is backpressured.
pub const MAX_QUEUED_OUTBOUND_BYTES: usize = 1024 * 1024;
pub const MAX_QUEUED_OUTBOUND_MESSAGES: usize = 64;
pub(crate) const TRACEDECAY_NATIVE_DIAGNOSTICS_METHOD: &str = "tracedecay/nativeDiagnostics";

mod context_controller;
mod diagnostics_controller;
mod lifecycle_controller;
mod outbound_controller;
mod semantic_controller;
mod workspace_diagnostics_controller;

use context_controller::ContextController;
#[cfg(test)]
use context_controller::bind_context_document_digest;
use diagnostics_controller::DiagnosticsController;
use lifecycle_controller::LifecycleController;
pub use outbound_controller::DaemonLspProtocolTransport;
use outbound_controller::OutboundController;
#[cfg(test)]
use outbound_controller::QueuedFrame;
use semantic_controller::SemanticController;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProtocolDispatch {
    pub queued_messages: usize,
    pub backpressured: bool,
    pub closed: bool,
}

/// One authenticated daemon LSP session. It owns no durable state and is
/// dropped alongside its registry entry after TTL expiry.
pub struct DaemonLspProtocolSession<P, S, D>
where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    lifecycle: LifecycleController<P, S>,
    outbound: OutboundController,
    diagnostics: DiagnosticsController<D>,
    context: ContextController,
    semantic: SemanticController,
}

impl<P, S, D> DaemonLspProtocolSession<P, S, D>
where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    /// Decodes and routes one opaque JSON-RPC payload. Responses and server
    /// notifications remain queued until a typed daemon-session transport
    /// acknowledges delivery to the bridge.
    pub fn handle_payload(&mut self, payload: &[u8], now_ms: u64) -> ProtocolDispatch {
        self.expire_requests(now_ms);
        let before = self.outbound.queue.len();
        let backpressured_before = self.outbound.queued_bytes >= MAX_QUEUED_OUTBOUND_BYTES;
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
                queued_messages: self.outbound.queue.len().saturating_sub(before),
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
                queued_messages: self.outbound.queue.len().saturating_sub(before),
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
            queued_messages: self.outbound.queue.len().saturating_sub(before),
            backpressured: backpressured_before
                || self.outbound.queued_bytes >= MAX_QUEUED_OUTBOUND_BYTES,
            closed: matches!(
                self.lifecycle.control.lifecycle(),
                SessionLifecycle::Exited | SessionLifecycle::Expired
            ),
        }
    }

    /// Runs only coalesced overlay work. A daemon scheduler can call this when
    /// no new frame arrives so a quiet editor still receives its refresh.
    pub fn flush_due(&mut self, now_ms: u64) -> ProtocolDispatch {
        let before = self.outbound.queue.len();
        self.expire_requests(now_ms);
        self.flush_debounced_diagnostics(now_ms);
        self.poll_context_requests();
        self.poll_context_expansions();
        self.poll_semantic_requests();
        self.flush_context_changes();
        ProtocolDispatch {
            queued_messages: self.outbound.queue.len().saturating_sub(before),
            backpressured: self.outbound.queued_bytes >= MAX_QUEUED_OUTBOUND_BYTES,
            closed: matches!(
                self.lifecycle.control.lifecycle(),
                SessionLifecycle::Exited | SessionLifecycle::Expired
            ),
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
            .lifecycle
            .overlays
            .open(uri.clone(), language_id, version, text)
            .map_err(|error| self.close_for_overlay_error(error))?;
        self.diagnostics.workspace_snapshots.clear();
        self.diagnostics.workspace_failures.clear();
        // A close followed by a reopen starts a new document incarnation; LSP
        // versions need not remain monotone across that boundary. Remove any
        // queued/acknowledged publication ordering state before publishing the
        // new incarnation.
        self.diagnostics.debounce.cancel(&uri);
        self.discard_document_publications(&uri);
        if self
            .diagnostics
            .native_upstream
            .get(&uri)
            .is_some_and(|native| native.version != snapshot.version)
        {
            self.diagnostics.native_upstream.remove(&uri);
        }
        let _ = self.publish_diagnostics(&uri, snapshot.version, 0, Vec::new());
        self.lifecycle
            .control
            .supersede_document(&uri, snapshot.version);
        if !self
            .diagnostics
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
            .lifecycle
            .overlays
            .change(&uri, version, &changes)
            .map_err(|error| self.close_for_overlay_error(error))?;
        self.diagnostics.workspace_snapshots.clear();
        self.diagnostics.workspace_failures.clear();
        self.discard_document_context(&uri);
        self.diagnostics.native_upstream.remove(&uri);
        self.lifecycle
            .control
            .supersede_document(&uri, snapshot.version);
        if !self
            .diagnostics
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
        let closed = self
            .lifecycle
            .overlays
            .close(&uri)
            .map_err(overlay_failure)?;
        self.diagnostics.workspace_snapshots.clear();
        self.diagnostics.workspace_failures.clear();
        self.discard_document_context(&uri);
        self.diagnostics.native_upstream.remove(&uri);
        self.lifecycle
            .control
            .supersede_document(&uri, closed.version.saturating_add(1));
        if !self
            .diagnostics
            .debounce
            .schedule_clear(uri, closed.version, now_ms)
        {
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
        self.diagnostics.workspace_snapshots.clear();
        self.diagnostics.workspace_failures.clear();
        self.discard_document_context(&uri);
        if matches!(
            self.lifecycle.gateway.document_saved(uri.clone()),
            FeedbackCycleResponse::Accepted
        ) {
            let version = self.lifecycle.overlays.version(&uri).unwrap_or_default();
            if !self
                .diagnostics
                .debounce
                .schedule_immediate_refresh(uri, version, now_ms)
            {
                return Err(self.close_for_debounce_overflow());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
