use super::{
    ClientCapabilities, ConnectionLocalRequestSequence, DaemonLspProtocolSession,
    DiagnosticSnapshotPort, FeedbackCyclePort, GatewayCapabilities, LspRequestId,
    SemanticProviderPort, SessionLifecycle, UpstreamCapabilities, json, request_id_value,
};
use crate::capabilities::PositionEncoding;

const DYNAMIC_DIAGNOSTIC_REGISTRATION_ID: &str = "tracedecay.workspace-diagnostics.v1";

#[derive(Clone, Debug, Eq, PartialEq)]
enum DynamicDiagnosticState {
    Unregistered,
    Registering {
        request_id: LspRequestId,
    },
    Registered,
    Unregistering {
        request_id: LspRequestId,
        register_after: bool,
    },
    RegistrationFailed,
    UnregistrationFailed,
}

pub(super) struct DynamicDiagnosticsController {
    negotiated: bool,
    workspace_diagnostics: bool,
    refresh: bool,
    last_desired: bool,
    state: DynamicDiagnosticState,
    next_request_id: ConnectionLocalRequestSequence,
}

impl Default for DynamicDiagnosticsController {
    fn default() -> Self {
        Self {
            negotiated: false,
            workspace_diagnostics: false,
            refresh: false,
            last_desired: false,
            state: DynamicDiagnosticState::Unregistered,
            next_request_id: ConnectionLocalRequestSequence::starting_at(1),
        }
    }
}

impl<P, S, D> DaemonLspProtocolSession<P, S, D>
where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    pub(super) fn configure_dynamic_diagnostics(
        &mut self,
        client: &ClientCapabilities,
        gateway: &GatewayCapabilities,
        upstream: &UpstreamCapabilities,
    ) {
        let diagnostics_supported = client.supports_position_encoding(PositionEncoding::Utf16)
            && (gateway.supports_managed_diagnostics || upstream.supports_diagnostics);
        let negotiated = diagnostics_supported
            && client.supports_document_diagnostics
            && client.diagnostic_dynamic_registration
            && gateway.supports_document_diagnostics
            && gateway.supports_workspace_diagnostics;
        self.dynamic_diagnostics = DynamicDiagnosticsController {
            negotiated,
            workspace_diagnostics: negotiated,
            refresh: negotiated && client.workspace_diagnostic_refresh_support,
            ..DynamicDiagnosticsController::default()
        };
    }

    pub(super) fn reconcile_dynamic_diagnostics(&mut self) {
        let desired = self.dynamic_diagnostics.negotiated
            && self.lifecycle.control.lifecycle() == SessionLifecycle::Ready
            && self.diagnostics.provider.supports_workspace_diagnostics();
        let readiness_changed = desired != self.dynamic_diagnostics.last_desired;
        self.dynamic_diagnostics.last_desired = desired;

        if !desired {
            self.lifecycle
                .gateway
                .bind_dynamic_diagnostics(false, false, false);
            match self.dynamic_diagnostics.state.clone() {
                DynamicDiagnosticState::Registering { request_id } => {
                    self.queue_server_request_cancellation(request_id);
                    self.queue_dynamic_diagnostic_unregistration(false);
                }
                DynamicDiagnosticState::Registered => {
                    self.queue_dynamic_diagnostic_unregistration(false);
                }
                DynamicDiagnosticState::RegistrationFailed if readiness_changed => {
                    self.dynamic_diagnostics.state = DynamicDiagnosticState::Unregistered;
                }
                DynamicDiagnosticState::Unregistered
                | DynamicDiagnosticState::RegistrationFailed
                | DynamicDiagnosticState::Unregistering { .. }
                | DynamicDiagnosticState::UnregistrationFailed => {}
            }
            return;
        }

        if readiness_changed {
            match self.dynamic_diagnostics.state.clone() {
                DynamicDiagnosticState::RegistrationFailed => {
                    self.dynamic_diagnostics.state = DynamicDiagnosticState::Unregistered;
                }
                DynamicDiagnosticState::UnregistrationFailed => {
                    // The client may still retain the old registration. Clear
                    // that uncertainty before attempting the same stable id.
                    self.dynamic_diagnostics.state = DynamicDiagnosticState::Registered;
                    self.queue_dynamic_diagnostic_unregistration(true);
                    return;
                }
                DynamicDiagnosticState::Unregistering { request_id, .. } => {
                    self.dynamic_diagnostics.state = DynamicDiagnosticState::Unregistering {
                        request_id,
                        register_after: true,
                    };
                    return;
                }
                _ => {}
            }
        }

        match self.dynamic_diagnostics.state.clone() {
            DynamicDiagnosticState::Unregistered => {
                self.queue_dynamic_diagnostic_registration();
            }
            DynamicDiagnosticState::Registered => {
                self.lifecycle.gateway.bind_dynamic_diagnostics(
                    true,
                    self.dynamic_diagnostics.workspace_diagnostics,
                    self.dynamic_diagnostics.refresh,
                );
            }
            DynamicDiagnosticState::Registering { .. }
            | DynamicDiagnosticState::Unregistering { .. }
            | DynamicDiagnosticState::RegistrationFailed
            | DynamicDiagnosticState::UnregistrationFailed => {}
        }
    }

    pub(super) fn handle_dynamic_diagnostic_response(
        &mut self,
        id: &LspRequestId,
        succeeded: bool,
    ) -> bool {
        match self.dynamic_diagnostics.state.clone() {
            DynamicDiagnosticState::Registering { request_id } if &request_id == id => {
                self.dynamic_diagnostics.state = if succeeded {
                    DynamicDiagnosticState::Registered
                } else {
                    DynamicDiagnosticState::RegistrationFailed
                };
                if !succeeded {
                    self.lifecycle
                        .gateway
                        .bind_dynamic_diagnostics(false, false, false);
                }
                self.reconcile_dynamic_diagnostics();
                true
            }
            DynamicDiagnosticState::Unregistering {
                request_id,
                register_after,
            } if &request_id == id => {
                self.lifecycle
                    .gateway
                    .bind_dynamic_diagnostics(false, false, false);
                self.dynamic_diagnostics.state = if succeeded || register_after {
                    DynamicDiagnosticState::Unregistered
                } else {
                    DynamicDiagnosticState::UnregistrationFailed
                };
                self.reconcile_dynamic_diagnostics();
                true
            }
            _ => false,
        }
    }

    pub(super) fn reset_dynamic_diagnostics_after_reconnect(&mut self) {
        if !self.dynamic_diagnostics.negotiated {
            return;
        }
        self.lifecycle
            .gateway
            .bind_dynamic_diagnostics(false, false, false);
        match self.dynamic_diagnostics.state.clone() {
            DynamicDiagnosticState::Unregistered | DynamicDiagnosticState::RegistrationFailed => {
                self.dynamic_diagnostics.state = DynamicDiagnosticState::Unregistered;
                self.dynamic_diagnostics.last_desired = false;
                self.reconcile_dynamic_diagnostics();
            }
            DynamicDiagnosticState::Registering { request_id }
            | DynamicDiagnosticState::Unregistering { request_id, .. } => {
                self.queue_server_request_cancellation(request_id);
                self.dynamic_diagnostics.state = DynamicDiagnosticState::Registered;
                self.queue_dynamic_diagnostic_unregistration(true);
            }
            DynamicDiagnosticState::Registered | DynamicDiagnosticState::UnregistrationFailed => {
                self.dynamic_diagnostics.state = DynamicDiagnosticState::Registered;
                self.queue_dynamic_diagnostic_unregistration(true);
            }
        }
    }

    fn queue_dynamic_diagnostic_registration(&mut self) {
        let Ok(request_id) = self
            .dynamic_diagnostics
            .next_request_id
            .next_string("tracedecay-diagnostic-register-")
            .map(LspRequestId::String)
        else {
            self.dynamic_diagnostics.state = DynamicDiagnosticState::RegistrationFailed;
            return;
        };
        let queued = self.enqueue_value(json!({
            "jsonrpc": "2.0",
            "id": request_id_value(request_id.clone()),
            "method": "client/registerCapability",
            "params": {
                "registrations": [{
                    "id": DYNAMIC_DIAGNOSTIC_REGISTRATION_ID,
                    "method": "textDocument/diagnostic",
                    "registerOptions": {
                        "documentSelector": null,
                        "identifier": DYNAMIC_DIAGNOSTIC_REGISTRATION_ID,
                        "interFileDependencies": true,
                        "workspaceDiagnostics": true,
                    },
                }],
            },
        }));
        if queued {
            self.dynamic_diagnostics.state = DynamicDiagnosticState::Registering { request_id };
        }
    }

    fn queue_dynamic_diagnostic_unregistration(&mut self, register_after: bool) {
        let Ok(request_id) = self
            .dynamic_diagnostics
            .next_request_id
            .next_string("tracedecay-diagnostic-unregister-")
            .map(LspRequestId::String)
        else {
            self.dynamic_diagnostics.state = DynamicDiagnosticState::UnregistrationFailed;
            return;
        };
        let queued = self.enqueue_value(json!({
            "jsonrpc": "2.0",
            "id": request_id_value(request_id.clone()),
            "method": "client/unregisterCapability",
            "params": {
                "unregisterations": [{
                    "id": DYNAMIC_DIAGNOSTIC_REGISTRATION_ID,
                    "method": "textDocument/diagnostic",
                }],
            },
        }));
        if queued {
            self.dynamic_diagnostics.state = DynamicDiagnosticState::Unregistering {
                request_id,
                register_after,
            };
        }
    }

    fn queue_server_request_cancellation(&mut self, request_id: LspRequestId) {
        let _ = self.enqueue_value(json!({
            "jsonrpc": "2.0",
            "method": "$/cancelRequest",
            "params": { "id": request_id_value(request_id) },
        }));
    }
}
