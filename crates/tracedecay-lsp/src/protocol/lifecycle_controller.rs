use super::{
    AdmittedRoot, AnalyzerCancellationPort, Arc, AuthorizedLspWorkspace, BTreeMap, BTreeSet,
    BindingId, CancellationOutcome, CapabilityAvailability, CapabilityParseError,
    ClientCapabilities, ContextController, ContextProjectionPort, DEFAULT_LSP_REQUEST_DEADLINE_MS,
    DaemonLspGateway, DaemonLspProtocolSession, DiagnosticSnapshotPort, DiagnosticsController,
    DynamicDiagnosticsController, EffectiveCapabilities, FeedbackCyclePort, GatewayCapabilities,
    GatewayMethod, LifecycleError, LspCatalogAdmission, LspRequestFailure, LspRequestId,
    LspSessionControl, MAX_CONTEXT_PROJECTION_KINDS, Map, MethodUnavailableReason,
    NativeIntegrationController, OutboundController, OverlayError, OverlayStore, RpcFailure,
    SemanticController, SemanticProviderPort, SessionLifecycle, TRACEDECAY_CONTEXT_EXPAND_METHOD,
    TRACEDECAY_CONTEXT_METHOD, UnavailableDiagnosticSnapshotProvider, UpstreamCapabilities, Value,
    error_response, initialized_workspace_uris, is_supported_context_projection, json,
    negotiate_capabilities, overlay_failure, request_id, request_id_value, success_response,
};

pub(super) struct LifecycleController<P, S>
where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
{
    pub(super) gateway: DaemonLspGateway<P, S>,
    pub(super) control: LspSessionControl,
    pub(super) gateway_capabilities: GatewayCapabilities,
    pub(super) upstream_capabilities: UpstreamCapabilities,
    pub(super) overlays: OverlayStore,
    pub(super) request_deadline_ms: u64,
}

impl<P, S> LifecycleController<P, S>
where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
{
    pub(super) fn new(
        gateway: DaemonLspGateway<P, S>,
        gateway_capabilities: GatewayCapabilities,
        upstream_capabilities: UpstreamCapabilities,
    ) -> Self {
        Self {
            gateway,
            control: LspSessionControl::default(),
            gateway_capabilities,
            upstream_capabilities,
            overlays: OverlayStore::default(),
            request_deadline_ms: DEFAULT_LSP_REQUEST_DEADLINE_MS,
        }
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

    #[allow(clippy::too_many_arguments)]
    pub fn from_workspace_ports(
        workspace: AuthorizedLspWorkspace,
        initial_capabilities: EffectiveCapabilities,
        gateway_capabilities: GatewayCapabilities,
        upstream_capabilities: UpstreamCapabilities,
        feedback_cycle: P,
        semantic_provider: S,
        diagnostics: D,
    ) -> Self {
        Self::new(
            DaemonLspGateway::for_workspace(
                workspace,
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
            lifecycle: LifecycleController::new(
                gateway,
                gateway_capabilities,
                upstream_capabilities,
            ),
            outbound: OutboundController::default(),
            diagnostics: DiagnosticsController::new(diagnostics),
            dynamic_diagnostics: DynamicDiagnosticsController::default(),
            context: ContextController::default(),
            native_integration: NativeIntegrationController::default(),
            semantic: SemanticController::default(),
            catalog: LspCatalogAdmission::from_application_catalog(),
            pending_workspace_mutation: None,
        }
    }

    pub(crate) fn catalog_binding(&self, operation: &str) -> Result<BindingId, RpcFailure> {
        self.catalog
            .as_ref()
            .map_err(|_| {
                RpcFailure::unavailable(operation, MethodUnavailableReason::ExplicitlyUnavailable)
            })?
            .binding(operation)
            .cloned()
            .map_err(|_| {
                RpcFailure::unavailable(operation, MethodUnavailableReason::ExplicitlyUnavailable)
            })
    }

    /// Mounts the daemon-owned analyzer cancellation adapter. Session
    /// cancellation remains authoritative even when the provider reports that
    /// its upstream work could not be interrupted.
    #[must_use]
    pub fn with_cancellation_port<C>(mut self, cancellation: C) -> Self
    where
        C: AnalyzerCancellationPort + Send + Sync + 'static,
    {
        self.semantic.cancellation = Some(Arc::new(cancellation));
        self
    }

    #[must_use]
    pub fn with_context_projection_port<C>(mut self, context: C) -> Self
    where
        C: ContextProjectionPort + Send + Sync + 'static,
    {
        self.context.port = Some(Arc::new(context));
        self
    }

    /// Mounts the daemon-owned read of recently observed native-integration
    /// transaction statuses, forwarded as server-to-client notifications only.
    #[must_use]
    pub fn with_native_integration_status_port(
        mut self,
        port: Arc<dyn crate::native_integration::NativeIntegrationStatusPort>,
    ) -> Self {
        self.native_integration.port = Some(port);
        self
    }

    pub fn root(&self) -> &AdmittedRoot {
        self.lifecycle.gateway.root()
    }

    pub fn lifecycle(&self) -> SessionLifecycle {
        self.lifecycle.control.lifecycle()
    }

    pub fn overlays(&self) -> &OverlayStore {
        &self.lifecycle.overlays
    }

    pub fn set_request_deadline_ms(&mut self, deadline_ms: u64) {
        self.lifecycle.request_deadline_ms = deadline_ms;
    }

    pub fn cancel_request(&mut self, id: &LspRequestId) -> CancellationOutcome {
        self.cancel_request_and_upstream(id)
    }

    /// Preserves session-only state while a bridge reconnects. Publications
    /// may be redelivered after this transition; exact-once delivery is never
    /// claimed across a transport interruption.
    pub fn detach(&mut self) -> Result<(), LifecycleError> {
        self.lifecycle.control.detach()?;
        // A bridge-local copy may have been lost before acknowledgement. The
        // retained queue remains authoritative and is eligible for redelivery.
        self.outbound.in_flight = false;
        Ok(())
    }

    pub fn reconnect(&mut self) -> Result<(), LifecycleError> {
        self.lifecycle.control.reconnect()?;
        if let Some(request_id) = self.diagnostics.refresh_request.as_ref()
            && !self
                .outbound
                .queue
                .iter()
                .any(|frame| frame.server_request.as_ref() == Some(request_id))
        {
            // A bridge acknowledgement may have raced the disconnect before
            // the client response arrived. Reissue one coalesced refresh; the
            // old response remains harmless and is ignored as unknown.
            self.diagnostics.refresh_request = None;
            self.queue_diagnostic_refresh();
        }
        self.reset_dynamic_diagnostics_after_reconnect();
        Ok(())
    }

    /// Marks session-local state expired. The retained registry calls this on
    /// TTL expiry; no overlay or queued document text survives the call.
    pub fn expire(&mut self) {
        self.lifecycle.control.expire();
        self.clear_volatile_state();
    }

    pub(super) fn clear_volatile_state(&mut self) {
        if let Some(cancellation) = &self.semantic.cancellation {
            for (request_id, pending) in &self.semantic.pending {
                if let Some(root) = pending
                    .request
                    .document_uri()
                    .and_then(|uri| self.document_root(uri).ok())
                {
                    let _ = cancellation.cancel_upstream(&root, request_id);
                }
            }
        }
        if let Some(context) = &self.context.port {
            for pending in self.context.pending_requests.values() {
                if let Ok(root) = self.optional_document_root(
                    pending.request.document_uri.as_deref(),
                    TRACEDECAY_CONTEXT_METHOD,
                ) {
                    let _ = context.cancel_request(&root, &pending.operation_id);
                }
            }
            for pending in self.context.pending_expansions.values() {
                if let Ok(root) = self.workspace_root(TRACEDECAY_CONTEXT_EXPAND_METHOD) {
                    let _ = context.cancel_request(&root, &pending.operation_id);
                }
            }
        }
        self.lifecycle.overlays.clear();
        self.diagnostics.debounce.clear();
        self.outbound.queue.clear();
        self.outbound.in_flight = false;
        self.outbound.queued_bytes = 0;
        self.diagnostics.published.clear();
        self.diagnostics.native_upstream.clear();
        self.diagnostics.cursor_native_mode = false;
        self.diagnostics.refresh_request = None;
        self.diagnostics.refresh_needed = false;
        self.diagnostics.active_refreshes.clear();
        self.diagnostics.workspace_results.clear();
        self.diagnostics.workspace_snapshots.clear();
        self.diagnostics.workspace_failures.clear();
        self.context.subscriptions.clear();
        self.context.currentness.clear();
        self.context.pending_requests.clear();
        self.context.pending_expansions.clear();
        self.semantic.pending.clear();
        self.pending_workspace_mutation = None;
    }

    /// Releases every session-local value bound to a root the daemon owner just
    /// removed. Nothing here re-derives authority: the removed roots are the
    /// exact ones the owner authorized dropping.
    pub(super) fn clear_removed_workspace_root_state(&mut self, removed: &[AdmittedRoot]) {
        let belongs_to_removed_root =
            |uri: &str| removed.iter().any(|root| root.contains_document(uri));
        let request_ids = self
            .semantic
            .pending
            .iter()
            .filter(|(_, pending)| {
                pending
                    .request
                    .document_uri()
                    .is_some_and(belongs_to_removed_root)
            })
            .map(|(request_id, _)| request_id.clone())
            .chain(
                self.context
                    .pending_requests
                    .iter()
                    .filter(|(_, pending)| {
                        pending
                            .request
                            .document_uri
                            .as_deref()
                            .is_some_and(belongs_to_removed_root)
                    })
                    .map(|(request_id, _)| request_id.clone()),
            )
            .collect::<BTreeSet<_>>();
        for request_id in request_ids {
            let _ = self.cancel_request_and_upstream(&request_id);
        }
        self.lifecycle
            .overlays
            .retain_documents(|uri| !belongs_to_removed_root(uri));
        self.diagnostics
            .debounce
            .retain_documents(|uri| !belongs_to_removed_root(uri));
        self.diagnostics
            .published
            .retain(|uri, _| !belongs_to_removed_root(uri));
        self.diagnostics
            .native_upstream
            .retain(|uri, _| !belongs_to_removed_root(uri));
        self.diagnostics
            .active_refreshes
            .retain(|uri, _| !belongs_to_removed_root(uri));
        self.diagnostics
            .workspace_results
            .retain(|uri, _| !belongs_to_removed_root(uri));
        self.diagnostics
            .workspace_snapshots
            .retain(|root_uri, _| !removed.iter().any(|root| root.matches_root_uri(root_uri)));
        self.diagnostics
            .workspace_failures
            .retain(|root_uri, _| !removed.iter().any(|root| root.matches_root_uri(root_uri)));
        self.context.currentness.retain(|(_, uri), _| {
            uri.as_deref()
                .is_none_or(|uri| !belongs_to_removed_root(uri))
        });
        let in_flight = self.outbound.in_flight;
        let queued = std::mem::take(&mut self.outbound.queue);
        for (index, frame) in queued.into_iter().enumerate() {
            let removed_publication = frame
                .publication
                .as_ref()
                .is_some_and(|publication| belongs_to_removed_root(&publication.uri));
            if removed_publication {
                if let Some(publication) = &frame.publication {
                    self.lifecycle.control.remove_publication(&publication.uri);
                }
                // The head frame of an in-flight write cannot be withdrawn: the
                // peer is already reading those bytes.
                if !(in_flight && index == 0) {
                    self.outbound.queued_bytes = self
                        .outbound
                        .queued_bytes
                        .saturating_sub(frame.payload.len());
                    continue;
                }
            }
            self.outbound.queue.push_back(frame);
        }
    }

    pub(super) fn cancel_pending_operations(&mut self) {
        let semantic = std::mem::take(&mut self.semantic.pending);
        for (request_id, pending) in semantic {
            let _ = self.lifecycle.control.cancel_request(&request_id);
            if let Some(cancellation) = &self.semantic.cancellation
                && let Some(root) = pending
                    .request
                    .document_uri()
                    .and_then(|uri| self.document_root(uri).ok())
            {
                let _ = cancellation.cancel_upstream(&root, &request_id);
            }
            self.complete_context_request(
                request_id,
                pending.response_id,
                Err(RpcFailure::request_failure(
                    LspRequestFailure::RequestCancelled,
                )),
            );
        }

        let snapshots = std::mem::take(&mut self.context.pending_requests);
        for (request_id, pending) in snapshots {
            let _ = self.lifecycle.control.cancel_request(&request_id);
            if let Some(context) = &self.context.port
                && let Ok(root) = self.optional_document_root(
                    pending.request.document_uri.as_deref(),
                    TRACEDECAY_CONTEXT_METHOD,
                )
            {
                let _ = context.cancel_request(&root, &pending.operation_id);
            }
            self.complete_context_request(
                request_id,
                pending.response_id,
                Err(RpcFailure::request_failure(
                    LspRequestFailure::RequestCancelled,
                )),
            );
        }

        let expansions = std::mem::take(&mut self.context.pending_expansions);
        for (request_id, pending) in expansions {
            let _ = self.lifecycle.control.cancel_request(&request_id);
            if let Some(context) = &self.context.port
                && let Ok(root) = self.workspace_root(TRACEDECAY_CONTEXT_EXPAND_METHOD)
            {
                let _ = context.cancel_request(&root, &pending.operation_id);
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
    pub(crate) fn handle_initialized_notification(&mut self) {
        let _ = self.lifecycle.control.initialized();
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
        if self.lifecycle.control.lifecycle() != SessionLifecycle::Ready {
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
        match self.lifecycle.control.shutdown() {
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
        if self.lifecycle.control.exit().is_err() {
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

    pub(crate) fn handle_client_response(&mut self, id: LspRequestId, succeeded: bool) {
        if self.handle_dynamic_diagnostic_response(&id, succeeded) {
            return;
        }
        if self.diagnostics.refresh_request.as_ref() == Some(&id) {
            self.diagnostics.refresh_request = None;
        }
        if self.diagnostics.refresh_needed {
            self.queue_diagnostic_refresh();
        }
    }

    pub(crate) fn document_version(&self, uri: &str) -> i64 {
        self.lifecycle.overlays.version(uri).unwrap_or_default()
    }

    pub(crate) fn handle_initialize(&mut self, id: Value, params: &Value) {
        if self.lifecycle.control.lifecycle() != SessionLifecycle::AwaitingInitialize {
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
        let root_uris = match initialized_workspace_uris(params) {
            Ok(root_uris) => root_uris,
            Err(error) => {
                self.enqueue_value(error_response(id, error));
                return;
            }
        };
        if !self
            .lifecycle
            .gateway
            .workspace()
            .admits_exact_root_hints(&root_uris)
        {
            self.enqueue_value(error_response(
                id,
                RpcFailure {
                    code: -32602,
                    message: "Invalid params",
                    data: json!({ "detail": "workspace roots differ from the daemon-admitted set" }),
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
            .port
            .as_ref()
            .map(|context| {
                context
                    .registrations()
                    .into_iter()
                    .filter(|registration| {
                        is_supported_context_projection(&registration.kind)
                            && registration.revision > 0
                    })
                    .take(MAX_CONTEXT_PROJECTION_KINDS)
                    .map(|registration| (registration.kind, registration.revision))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let mut gateway_capabilities = self.lifecycle.gateway_capabilities.clone();
        let dynamic_workspace_diagnostics = client.diagnostic_dynamic_registration
            && client.supports_document_diagnostics
            && gateway_capabilities.supports_document_diagnostics
            && gateway_capabilities.supports_workspace_diagnostics;
        if !dynamic_workspace_diagnostics {
            gateway_capabilities.supports_workspace_diagnostics &=
                self.diagnostics.provider.supports_workspace_diagnostics();
        }
        let upstream_capabilities = self.lifecycle.upstream_capabilities.clone();
        self.configure_dynamic_diagnostics(&client, &gateway_capabilities, &upstream_capabilities);
        gateway_capabilities
            .context_projections
            .retain(|kind, revision| mounted_context.get(kind) == Some(revision));
        let effective = negotiate_capabilities(
            &client,
            &gateway_capabilities,
            &self.lifecycle.upstream_capabilities,
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
        if self.lifecycle.control.begin_initialize().is_err() {
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
        self.diagnostics.cursor_native_mode = cursor_native_mode;
        self.lifecycle
            .gateway
            .bind_initialized_capabilities(effective.clone());
    }

    pub(crate) fn handle_cancel(&mut self, params: &Value) {
        let Some(id) = params.get("id").and_then(request_id) else {
            return;
        };
        let _ = self.cancel_request_and_upstream(&id);
    }
    pub(super) fn expire_requests(&mut self, now_ms: u64) {
        for id in self.lifecycle.control.expire_deadlines(now_ms) {
            if let Some(pending) = self.semantic.pending.remove(&id)
                && let Some(cancellation) = &self.semantic.cancellation
                && let Some(root) = pending
                    .request
                    .document_uri()
                    .and_then(|uri| self.document_root(uri).ok())
            {
                let _ = cancellation.cancel_upstream(&root, &id);
            }
            if let Some(pending) = self.context.pending_requests.remove(&id)
                && let Some(context) = &self.context.port
                && let Ok(root) = self.optional_document_root(
                    pending.request.document_uri.as_deref(),
                    TRACEDECAY_CONTEXT_METHOD,
                )
            {
                let _ = context.cancel_request(&root, &pending.operation_id);
            }
            if let Some(pending) = self.context.pending_expansions.remove(&id)
                && let Some(context) = &self.context.port
                && let Ok(root) = self.workspace_root(TRACEDECAY_CONTEXT_EXPAND_METHOD)
            {
                let _ = context.cancel_request(&root, &pending.operation_id);
            }
            let disposition = self.lifecycle.control.complete_request(&id);
            if let Some(failure) = disposition.failure() {
                self.enqueue_value(error_response(
                    request_id_value(id),
                    RpcFailure::request_failure(failure),
                ));
            }
        }
    }

    pub(super) fn cancel_request_and_upstream(&mut self, id: &LspRequestId) -> CancellationOutcome {
        let outcome = self.lifecycle.control.cancel_request(id);
        if outcome == CancellationOutcome::Accepted {
            let semantic = self.semantic.pending.remove(id);
            let context_pending = self.context.pending_requests.remove(id);
            let expansion_pending = self.context.pending_expansions.remove(id);
            if let Some(cancellation) = &self.semantic.cancellation
                && let Some(root) = semantic
                    .as_ref()
                    .and_then(|pending| pending.request.document_uri())
                    .and_then(|uri| self.document_root(uri).ok())
            {
                let _ = cancellation.cancel_upstream(&root, id);
            }
            if let Some(context) = &self.context.port {
                if let Some(pending) = context_pending.as_ref()
                    && let Ok(root) = self.optional_document_root(
                        pending.request.document_uri.as_deref(),
                        TRACEDECAY_CONTEXT_METHOD,
                    )
                {
                    let _ = context.cancel_request(&root, &pending.operation_id);
                }
                if let Some(pending) = expansion_pending.as_ref()
                    && let Ok(root) = self.workspace_root(TRACEDECAY_CONTEXT_EXPAND_METHOD)
                {
                    let _ = context.cancel_request(&root, &pending.operation_id);
                }
                if context_pending.is_none() && expansion_pending.is_none() {
                    for root in self.lifecycle.gateway.workspace().roots() {
                        let _ = context.cancel_request(root, id);
                    }
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

    pub(super) fn require_ready(&self) -> Result<(), RpcFailure> {
        (self.lifecycle.control.lifecycle() == SessionLifecycle::Ready)
            .then_some(())
            .ok_or_else(|| {
                RpcFailure::request_failure(LspRequestFailure::ServerCancelled {
                    retrigger_request: true,
                })
            })
    }

    pub(super) fn require_document_root(&self, uri: &str) -> Result<(), RpcFailure> {
        self.document_root(uri).map(drop)
    }

    pub(super) fn document_root(&self, uri: &str) -> Result<AdmittedRoot, RpcFailure> {
        self.lifecycle
            .gateway
            .root_for_document(uri)
            .cloned()
            .map_err(|reason| {
                RpcFailure::unavailable(
                    GatewayMethod::TextDocumentDiagnostic.as_lsp_method(),
                    reason,
                )
            })
    }

    pub(super) fn optional_document_root(
        &self,
        uri: Option<&str>,
        method: &str,
    ) -> Result<AdmittedRoot, RpcFailure> {
        match uri {
            Some(uri) => self.document_root(uri),
            None => self.workspace_root(method),
        }
    }

    pub(super) fn workspace_root(&self, method: &str) -> Result<AdmittedRoot, RpcFailure> {
        let roots = self.lifecycle.gateway.workspace().roots();
        if roots.len() == 1 {
            Ok(roots[0].clone())
        } else {
            Err(RpcFailure::unavailable(
                method,
                MethodUnavailableReason::AmbiguousAdmittedRoot,
            ))
        }
    }

    pub(super) fn close_for_overlay_error(&mut self, error: OverlayError) -> RpcFailure {
        if matches!(
            error,
            OverlayError::TooLarge { .. } | OverlayError::TooManyDocuments { .. }
        ) {
            self.expire();
        }
        overlay_failure(error)
    }

    pub(super) fn close_for_debounce_overflow(&mut self) -> RpcFailure {
        self.expire();
        RpcFailure::request_failure(LspRequestFailure::ServerCancelled {
            retrigger_request: true,
        })
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

#[cfg(test)]
mod controller_tests {
    use super::*;

    #[test]
    fn detach_and_reconnect_preserve_ready_lifecycle() {
        let mut session = super::super::tests::session();
        super::super::tests::initialize(&mut session);

        session.detach().unwrap();
        assert_eq!(session.lifecycle(), SessionLifecycle::Detached);
        session.reconnect().unwrap();
        assert_eq!(session.lifecycle(), SessionLifecycle::Ready);
    }
}
