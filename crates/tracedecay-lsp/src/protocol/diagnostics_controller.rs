use super::outbound_controller::PublicationTag;
use super::*;

const MAX_NATIVE_DIAGNOSTIC_URI_BYTES: usize = 4 * 1024;
const MAX_NATIVE_DIAGNOSTIC_METADATA_BYTES: usize = 256;

#[derive(Clone, Debug)]
pub(super) struct PublishedDiagnostic {
    pub(super) version: i64,
    pub(super) generation: u64,
}

#[derive(Clone)]
pub(super) struct PendingDiagnosticRefresh {
    pub(super) identity: DiagnosticRefreshIdentity,
    pub(super) overlay_version: i64,
}
#[derive(Clone, Eq, PartialEq)]
pub(super) struct NativeDiagnosticSnapshot {
    pub(super) version: i64,
    pub(super) diagnostics: Vec<GatewayDiagnostic>,
}

pub(super) struct DiagnosticsController<D>
where
    D: DiagnosticSnapshotPort,
{
    pub(super) provider: D,
    pub(super) debounce: OverlayDiagnosticDebouncer,
    pub(super) published: BTreeMap<String, PublishedDiagnostic>,
    pub(super) native_upstream: BTreeMap<String, NativeDiagnosticSnapshot>,
    pub(super) cursor_native_mode: bool,
    pub(super) next_server_request_id: ConnectionLocalRequestSequence,
    pub(super) refresh_request: Option<LspRequestId>,
    pub(super) refresh_needed: bool,
    pub(super) active_refreshes: BTreeMap<String, PendingDiagnosticRefresh>,
}

impl<D> DiagnosticsController<D>
where
    D: DiagnosticSnapshotPort,
{
    pub(super) fn new(provider: D) -> Self {
        Self {
            provider,
            debounce: OverlayDiagnosticDebouncer::default(),
            published: BTreeMap::new(),
            native_upstream: BTreeMap::new(),
            cursor_native_mode: false,
            next_server_request_id: ConnectionLocalRequestSequence::starting_at(1),
            refresh_request: None,
            refresh_needed: false,
            active_refreshes: BTreeMap::new(),
        }
    }
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
                crate::diagnostics::MAX_DIAGNOSTIC_MESSAGE_BYTES,
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

impl<P, S, D> DaemonLspProtocolSession<P, S, D>
where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    pub(crate) fn handle_native_diagnostics_notification(&mut self, params: &Value, now_ms: u64) {
        if !self.diagnostics.cursor_native_mode || self.require_ready().is_err() {
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
        let Some(document_version) = self.lifecycle.overlays.version(&uri) else {
            return;
        };
        if document_version != snapshot.version {
            return;
        }
        if self.diagnostics.native_upstream.get(&uri) == Some(&snapshot) {
            return;
        }
        let version = snapshot.version;
        self.diagnostics
            .native_upstream
            .insert(uri.clone(), snapshot);
        if !self
            .diagnostics
            .debounce
            .schedule_immediate_refresh(uri.clone(), version, now_ms)
        {
            self.diagnostics.native_upstream.remove(&uri);
        }
    }
    pub(crate) fn pull_diagnostics(
        &mut self,
        uri: &str,
        params: &Value,
    ) -> Result<Value, RpcFailure> {
        self.require_document_root(uri)?;
        if !self
            .lifecycle
            .gateway
            .capabilities()
            .supports_document_diagnostics
        {
            return Err(RpcFailure::unavailable(
                GatewayMethod::TextDocumentDiagnostic.as_lsp_method(),
                MethodUnavailableReason::CapabilityNotNegotiated,
            ));
        }
        response_value(
            self.lifecycle.gateway.request_document_diagnostics(uri),
            |()| Value::Null,
        )?;
        self.poll_diagnostic_refresh(uri);
        let overlay = self.lifecycle.overlays.snapshot(uri);
        let outcome = self.diagnostics.provider.document_diagnostics(
            self.lifecycle.gateway.root(),
            uri,
            overlay.as_ref(),
        );
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
        if self
            .diagnostics
            .published
            .get(uri)
            .is_some_and(|published| {
                published.version == version && published.generation > generation
            })
        {
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
                    self.lifecycle
                        .gateway
                        .capabilities()
                        .document_diagnostics_data,
                ),
            ),
            DiagnosticSerializationCapabilities::pull(self.lifecycle.gateway.capabilities()),
        );
        let previous = params.get("previousResultId").and_then(Value::as_str);
        if previous == Some(result_id.as_str()) {
            return Ok(document_diagnostic_report_value(
                DocumentDiagnosticReport::Unchanged { result_id },
                DiagnosticSerializationCapabilities::pull(self.lifecycle.gateway.capabilities()),
            ));
        }
        if overlay.is_some() {
            self.diagnostics.published.insert(
                uri.to_owned(),
                PublishedDiagnostic {
                    version,
                    generation,
                },
            );
        }
        Ok(value)
    }

    pub(super) fn flush_debounced_diagnostics(&mut self, now_ms: u64) {
        if self.lifecycle.control.lifecycle() != SessionLifecycle::Ready {
            return;
        }
        self.poll_diagnostic_refreshes();
        while self.has_outbound_capacity(MAX_PUBLICATION_BYTES) {
            let Some(scheduled) = self.diagnostics.debounce.take_next_due(now_ms) else {
                break;
            };
            match scheduled.kind {
                DebouncedDiagnosticKind::Clear => {
                    let generation = self
                        .diagnostics
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
                        self.diagnostics.published.remove(&scheduled.uri);
                        self.queue_diagnostic_refresh();
                    }
                }
                DebouncedDiagnosticKind::Refresh => {
                    let overlay = self.lifecycle.overlays.snapshot(&scheduled.uri);
                    let outcome = self.diagnostics.provider.document_diagnostics(
                        self.lifecycle.gateway.root(),
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

    pub(super) fn request_diagnostic_refresh(
        &mut self,
        uri: &str,
        version: i64,
        overlay: Option<&crate::overlay::OverlaySnapshot>,
        source_generation: Option<u64>,
    ) -> Option<String> {
        if self
            .diagnostics
            .active_refreshes
            .get(uri)
            .is_some_and(|pending| pending.overlay_version == version)
        {
            return None;
        }
        match self.diagnostics.provider.request_document_refresh(
            self.lifecycle.gateway.root(),
            uri,
            overlay,
            source_generation,
        ) {
            DiagnosticRefreshAdmission::Started(identity)
            | DiagnosticRefreshAdmission::AlreadyRunning(identity) => {
                if !valid_refresh_identity(&identity, source_generation) {
                    return Some("invalid-refresh-identity".to_owned());
                }
                self.diagnostics.active_refreshes.insert(
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

    pub(super) fn poll_diagnostic_refreshes(&mut self) {
        let documents: Vec<_> = self.diagnostics.active_refreshes.keys().cloned().collect();
        for document in documents {
            if !self.has_outbound_capacity(MAX_PUBLICATION_BYTES) {
                break;
            }
            self.poll_diagnostic_refresh(&document);
        }
    }

    pub(super) fn poll_diagnostic_refresh(&mut self, uri: &str) {
        let Some(pending) = self.diagnostics.active_refreshes.get(uri).cloned() else {
            return;
        };
        let version = self.lifecycle.overlays.version(uri).unwrap_or_default();
        if version != pending.overlay_version {
            self.diagnostics.active_refreshes.remove(uri);
            return;
        }
        let overlay = self.lifecycle.overlays.snapshot(uri);
        match self.diagnostics.provider.document_diagnostics(
            self.lifecycle.gateway.root(),
            uri,
            overlay.as_ref(),
        ) {
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
                let publication_not_superseded =
                    self.diagnostics.published.get(uri).is_none_or(|published| {
                        published.version != version || published.generation <= generation
                    });
                if target_matches && source_not_superseded && publication_not_superseded {
                    if self.publish_complete_snapshot(uri, version, diagnostics) {
                        self.diagnostics.active_refreshes.remove(uri);
                    }
                } else {
                    self.diagnostics.active_refreshes.remove(uri);
                }
            }
            DiagnosticSnapshotOutcome::Partial { .. }
            | DiagnosticSnapshotOutcome::Failed { .. } => {
                self.diagnostics.active_refreshes.remove(uri);
                self.queue_diagnostic_refresh();
            }
            DiagnosticSnapshotOutcome::Ready { .. } | DiagnosticSnapshotOutcome::Refreshing(_) => {}
        }
    }

    pub(super) fn publish_complete_snapshot(
        &mut self,
        uri: &str,
        version: i64,
        snapshot: crate::provider::GenerationDiagnostics,
    ) -> bool {
        let generation = snapshot.generation;
        if self
            .diagnostics
            .published
            .get(uri)
            .is_some_and(|published| {
                published.version == version && published.generation > generation
            })
        {
            return false;
        }
        let merged = self.merge_document_diagnostics(uri, snapshot.upstream, snapshot.tracedecay);
        if self
            .lifecycle
            .gateway
            .capabilities()
            .supports_publish_diagnostics
            && !self.publish_diagnostics(
                uri,
                version,
                generation,
                self.visible_diagnostics(
                    merged.items,
                    self.lifecycle
                        .gateway
                        .capabilities()
                        .publish_diagnostics_data,
                ),
            )
        {
            return false;
        }
        self.diagnostics.published.insert(
            uri.to_owned(),
            PublishedDiagnostic {
                version,
                generation,
            },
        );
        self.queue_diagnostic_refresh();
        true
    }

    pub(super) fn merge_document_diagnostics(
        &self,
        uri: &str,
        mut upstream: Vec<GatewayDiagnostic>,
        tracedecay: Vec<GatewayDiagnostic>,
    ) -> DiagnosticMerge {
        if let Some(native) = self.diagnostics.native_upstream.get(uri) {
            upstream.extend(native.diagnostics.iter().cloned());
        }
        DiagnosticMerge::for_document(uri, upstream, tracedecay)
    }

    pub(super) fn visible_diagnostics(
        &self,
        mut diagnostics: Vec<GatewayDiagnostic>,
        supports_diagnostic_data: bool,
    ) -> Vec<GatewayDiagnostic> {
        for diagnostic in &mut diagnostics {
            diagnostic.related_information.retain(|related| {
                self.lifecycle
                    .gateway
                    .root()
                    .contains_document(&related.uri)
            });
        }
        diagnostics
            .into_iter()
            .filter(|diagnostic| {
                (!self.diagnostics.cursor_native_mode || diagnostic.source.is_tracedecay())
                    && (supports_diagnostic_data || !diagnostic.source.is_tracedecay())
            })
            .collect()
    }

    pub(super) fn publish_diagnostics(
        &mut self,
        uri: &str,
        version: i64,
        generation: u64,
        diagnostics: Vec<GatewayDiagnostic>,
    ) -> bool {
        if !self
            .lifecycle
            .gateway
            .capabilities()
            .supports_publish_diagnostics
        {
            return false;
        }
        let capabilities = self.lifecycle.gateway.capabilities();
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

    pub(super) fn queue_diagnostic_refresh(&mut self) {
        if !self
            .lifecycle
            .gateway
            .capabilities()
            .supports_workspace_diagnostic_refresh
        {
            self.diagnostics.refresh_needed = false;
            return;
        }
        self.diagnostics.refresh_needed = true;
        if self.diagnostics.refresh_request.is_some() {
            return;
        }
        let Ok(id) = self
            .diagnostics
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
            self.diagnostics.refresh_request = Some(id);
            self.diagnostics.refresh_needed = false;
        }
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

pub(super) fn refresh_pending_failure(
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

#[cfg(test)]
mod controller_tests {
    use super::*;

    #[test]
    fn native_lane_never_reimports_tracedecay_diagnostics() {
        let snapshot = NativeDiagnosticsNotification {
            uri: "file:///root/a.rs".to_owned(),
            version: 1,
            diagnostics: vec![NativeDiagnostic {
                range: NativeRange {
                    start: NativePosition {
                        line: 0,
                        character: 0,
                    },
                    end: NativePosition {
                        line: 0,
                        character: 1,
                    },
                },
                severity: Some(2),
                code: None,
                source: "tracedecay-github".to_owned(),
                message: "projected".to_owned(),
                data: None,
            }],
        }
        .into_snapshot()
        .unwrap()
        .1;

        assert!(snapshot.diagnostics.is_empty());
    }
}
