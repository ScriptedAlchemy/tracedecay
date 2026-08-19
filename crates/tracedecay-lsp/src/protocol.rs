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
use tracedecay_tool_catalog::BindingId;

use crate::bridge::{
    DaemonLspSessionTransport, FramePoll, FrameSend, LspFrame, MAX_LSP_FRAME_BYTES,
};
use crate::capabilities::{
    CapabilityAvailability, CapabilityParseError, ClientCapabilities, EffectiveCapabilities,
    GatewayCapabilities, UpstreamCapabilities, is_supported_context_projection,
    negotiate_capabilities,
};
use crate::catalog::{LspCatalogAdmission, LspCatalogAdmissionError};
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
use crate::workspace::{WorkspaceFolderMutation, WorkspaceFolderMutationApplyError};

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
mod dynamic_diagnostics_controller;
mod lifecycle_controller;
mod native_integration_controller;
mod outbound_controller;
mod semantic_controller;
mod workspace_diagnostics_controller;

use context_controller::ContextController;
#[cfg(test)]
use context_controller::bind_context_document_digest;
use diagnostics_controller::DiagnosticsController;
use dynamic_diagnostics_controller::DynamicDiagnosticsController;
use lifecycle_controller::LifecycleController;
use native_integration_controller::NativeIntegrationController;
pub use outbound_controller::DaemonLspProtocolTransport;
use outbound_controller::OutboundController;
#[cfg(test)]
use outbound_controller::QueuedFrame;
use semantic_controller::SemanticController;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProtocolDispatch {
    pub queued_messages: usize,
    pub closed: bool,
}

/// Ownership result for one client-to-daemon frame.
///
/// `Consumed` means the caller must release its copy even when dispatch filled
/// the outbound queue. `Backpressured` is returned only before payload
/// decoding or routing, so retaining and retrying that frame is lossless.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientFrameAdmission {
    Consumed(ProtocolDispatch),
    Backpressured,
    Closed,
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
    dynamic_diagnostics: DynamicDiagnosticsController,
    context: ContextController,
    native_integration: NativeIntegrationController,
    semantic: SemanticController,
    catalog: Result<LspCatalogAdmission, LspCatalogAdmissionError>,
    pending_workspace_mutation: Option<WorkspaceFolderMutation>,
}

impl<P, S, D> DaemonLspProtocolSession<P, S, D>
where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    /// Exact workspace admitted by the daemon owner for this runtime actor.
    pub fn workspace(&self) -> &AuthorizedLspWorkspace {
        self.lifecycle.gateway.workspace()
    }

    /// Admits one bridge-owned frame without ambiguous post-dispatch
    /// backpressure. A consumed frame is never reported as retryable.
    pub fn try_handle_client_payload(
        &mut self,
        payload: &[u8],
        now_ms: u64,
    ) -> ClientFrameAdmission {
        if matches!(
            self.lifecycle.control.lifecycle(),
            SessionLifecycle::Exited | SessionLifecycle::Expired
        ) {
            return ClientFrameAdmission::Closed;
        }
        self.prepare_payload_dispatch(now_ms);
        if !self.has_client_frame_outbound_capacity() {
            return ClientFrameAdmission::Backpressured;
        }
        ClientFrameAdmission::Consumed(self.handle_prepared_payload(payload, now_ms))
    }

    /// Decodes and routes one already-admitted JSON-RPC payload. Responses and
    /// server notifications remain queued until a typed daemon-session
    /// transport acknowledges delivery to the bridge.
    pub fn handle_payload(&mut self, payload: &[u8], now_ms: u64) -> ProtocolDispatch {
        self.prepare_payload_dispatch(now_ms);
        self.handle_prepared_payload(payload, now_ms)
    }

    fn prepare_payload_dispatch(&mut self, now_ms: u64) {
        self.expire_requests(now_ms);
        // Capability loss closes admission before the triggering client
        // request is dispatched. Capability gain is projected only through
        // the dynamic-registration acknowledgement path.
        self.reconcile_dynamic_diagnostics();
    }

    fn handle_prepared_payload(&mut self, payload: &[u8], now_ms: u64) -> ProtocolDispatch {
        let before = self.outbound.queue.len();
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
                closed: false,
            };
        };
        self.dispatch_value(value, now_ms);
        self.reconcile_dynamic_diagnostics();
        self.flush_debounced_diagnostics(now_ms);
        self.poll_context_requests();
        self.poll_context_expansions();
        self.poll_semantic_requests();
        self.flush_context_changes();
        self.flush_native_integration_status();
        ProtocolDispatch {
            queued_messages: self.outbound.queue.len().saturating_sub(before),
            closed: matches!(
                self.lifecycle.control.lifecycle(),
                SessionLifecycle::Exited | SessionLifecycle::Expired
            ),
        }
    }

    /// The one validated workspace-folder intent awaiting its daemon owner.
    ///
    /// The actor never resolves or authorizes a folder URI itself: it parses
    /// and fences the client's notification, and the owner answers with either
    /// [`Self::apply_workspace_folder_mutation`] or
    /// [`Self::reject_workspace_folder_mutation`].
    pub fn pending_workspace_folder_mutation(&self) -> Option<WorkspaceFolderMutation> {
        self.pending_workspace_mutation.clone()
    }

    pub fn reject_workspace_folder_mutation(
        &mut self,
        mutation: &WorkspaceFolderMutation,
    ) -> Result<(), WorkspaceFolderMutationApplyError> {
        if self.pending_workspace_mutation.as_ref() != Some(mutation) {
            return Err(WorkspaceFolderMutationApplyError::StaleWorkspace);
        }
        self.pending_workspace_mutation = None;
        Ok(())
    }

    pub fn apply_workspace_folder_mutation(
        &mut self,
        mutation: &WorkspaceFolderMutation,
        workspace: AuthorizedLspWorkspace,
    ) -> Result<(), WorkspaceFolderMutationApplyError> {
        if self.pending_workspace_mutation.as_ref() != Some(mutation)
            || self.lifecycle.gateway.workspace().scope_set_digest()
                != mutation.observed_scope_digest.as_ref()
        {
            return Err(WorkspaceFolderMutationApplyError::StaleWorkspace);
        }
        let removed_roots = self
            .lifecycle
            .gateway
            .workspace()
            .roots()
            .iter()
            .filter(|root| {
                mutation
                    .removed
                    .iter()
                    .any(|uri| root.matches_root_uri(uri))
            })
            .cloned()
            .collect::<Vec<_>>();
        self.clear_removed_workspace_root_state(&removed_roots);
        self.diagnostics.workspace_snapshots.clear();
        self.diagnostics.workspace_failures.clear();
        self.lifecycle.gateway.replace_workspace(workspace);
        self.pending_workspace_mutation = None;
        Ok(())
    }

    pub(crate) fn handle_workspace_folders_changed(
        &mut self,
        params: &Value,
    ) -> Result<(), RpcFailure> {
        self.require_ready()?;
        if !self
            .lifecycle
            .gateway
            .capabilities()
            .workspace_folders_supported
        {
            return Err(RpcFailure::unavailable(
                "workspace/didChangeWorkspaceFolders",
                MethodUnavailableReason::CapabilityNotNegotiated,
            ));
        }
        if self.pending_workspace_mutation.is_some() {
            return Err(RpcFailure::invalid_params(
                "a workspace folder mutation is already pending",
            ));
        }
        self.pending_workspace_mutation =
            WorkspaceFolderMutation::parse(params, self.lifecycle.gateway.workspace())?;
        Ok(())
    }

    /// Runs only coalesced overlay work. A daemon scheduler can call this when
    /// no new frame arrives so a quiet editor still receives its refresh.
    pub fn flush_due(&mut self, now_ms: u64) -> ProtocolDispatch {
        let before = self.outbound.queue.len();
        self.expire_requests(now_ms);
        self.reconcile_dynamic_diagnostics();
        self.flush_debounced_diagnostics(now_ms);
        self.poll_context_requests();
        self.poll_context_expansions();
        self.poll_semantic_requests();
        self.flush_context_changes();
        self.flush_native_integration_status();
        ProtocolDispatch {
            queued_messages: self.outbound.queue.len().saturating_sub(before),
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
        let root = self.document_root(&uri)?;
        let language_id = required_nonempty_string(text_document, "languageId")?;
        let version = required_i64(text_document, "version")?;
        let text = required_string(text_document, "text")?;
        let snapshot = self
            .lifecycle
            .overlays
            .open(&root, uri.clone(), language_id, version, text)
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
