use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as SyncMutex};

use tokio::sync::Mutex;

use super::super::client::{
    LspRefreshTimeouts, LspSemanticRequestError, StdioLspClient, decode_semantic_request,
};
use super::super::error::{
    AnalyzerCancellation as CancellationToken, AnalyzerRuntimeError as TraceDecayError,
};
use crate::{
    AdmittedRoot, AnalyzerEvent, AnalyzerState, AnalyzerSupervisor, LspRequestId, LspRuntimeFuture,
    LspSemanticOperationOutcome, LspSemanticRequestAuthority, UpstreamCapabilities,
};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SemanticOperationKey {
    root_uri: String,
    request_id: LspRequestId,
}

struct StdioLspSemanticAuthorityInner {
    command: String,
    args: Vec<String>,
    /// The adapter language this analyzer serves, used verbatim as the
    /// `languageId` when this lane has to open a document upstream.
    language: String,
    project_root: PathBuf,
    root_uri: String,
    timeouts: LspRefreshTimeouts,
    client: Arc<Mutex<Option<StdioLspClient>>>,
    operations: Mutex<BTreeMap<SemanticOperationKey, CancellationToken>>,
    supervisor: SyncMutex<AnalyzerSupervisor>,
}

/// Retained analyzer authority sharing the broker's stdio client slot.
///
/// Queued operations race lock acquisition against their cancellation token;
/// in-flight operations delegate cancellation to `StdioLspClient`, which
/// writes the standard `$/cancelRequest` notification.
#[derive(Clone)]
pub struct StdioLspSemanticAuthority {
    inner: Arc<StdioLspSemanticAuthorityInner>,
}

impl StdioLspSemanticAuthority {
    pub fn new(
        command: impl Into<String>,
        args: Vec<String>,
        language: impl Into<String>,
        project_root: PathBuf,
        root_uri: impl Into<String>,
        timeouts: LspRefreshTimeouts,
    ) -> Arc<Self> {
        Self::from_shared_client(
            command,
            args,
            language,
            project_root,
            root_uri,
            timeouts,
            Arc::new(Mutex::new(None)),
        )
    }

    pub(crate) fn from_shared_client(
        command: impl Into<String>,
        args: Vec<String>,
        language: impl Into<String>,
        project_root: PathBuf,
        root_uri: impl Into<String>,
        timeouts: LspRefreshTimeouts,
        client: Arc<Mutex<Option<StdioLspClient>>>,
    ) -> Arc<Self> {
        let root_uri = root_uri.into();
        Arc::new(Self {
            inner: Arc::new(StdioLspSemanticAuthorityInner {
                command: command.into(),
                args,
                language: language.into(),
                project_root,
                root_uri: root_uri.clone(),
                timeouts,
                client,
                operations: Mutex::new(BTreeMap::new()),
                supervisor: SyncMutex::new(AnalyzerSupervisor::new(AdmittedRoot::new(root_uri))),
            }),
        })
    }

    /// Atomic project-scoped lifecycle evidence for doctor, dashboard, and
    /// other non-LSP callers. The snapshot contains no process or stderr data.
    pub fn analyzer_readiness(&self) -> AnalyzerSupervisor {
        self.inner
            .supervisor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Starts the retained client once and returns the typed capabilities from
    /// its successful standard initialize response.
    pub async fn upstream_capabilities(
        &self,
    ) -> std::result::Result<UpstreamCapabilities, TraceDecayError> {
        let mut slot = self.inner.client.lock().await;
        if let Some(client) = slot.as_ref() {
            return Ok(client.upstream_capabilities());
        }
        if analyzer_terminal_outcome(&self.inner).is_some() {
            return Err(TraceDecayError::Unavailable);
        }

        let root = AdmittedRoot::new(self.inner.root_uri.clone());
        if !begin_analyzer_start(&self.inner, &root) {
            return Err(TraceDecayError::Unavailable);
        }
        match StdioLspClient::start_with_timeouts(
            &self.inner.command,
            &self.inner.args,
            &self.inner.project_root,
            self.inner.timeouts,
        )
        .await
        {
            Ok(client) => {
                let capabilities = client.upstream_capabilities();
                mark_analyzer_ready(&self.inner, &root);
                *slot = Some(client);
                Ok(capabilities)
            }
            Err(error) => {
                record_analyzer_event(&self.inner, &root, AnalyzerEvent::StartupFailed);
                Err(error)
            }
        }
    }

    fn terminal_outcome(&self) -> Option<LspSemanticOperationOutcome> {
        analyzer_terminal_outcome(&self.inner)
    }
}

impl LspSemanticRequestAuthority for StdioLspSemanticAuthority {
    fn start(
        &self,
        root: AdmittedRoot,
        request_id: LspRequestId,
        request: crate::LspSemanticRequest,
    ) -> LspRuntimeFuture<LspSemanticOperationOutcome> {
        // Read the addressed document before the typed decode consumes the
        // request: every `textDocument/*` method names it the same way, and a
        // method that names none (workspace symbols) simply has nothing to
        // open.
        let document = semantic_request_document(&request);
        let request = match decode_semantic_request(request) {
            Ok(request) => request,
            Err(error) => {
                return Box::pin(async move { analyzer_event_outcome(error.analyzer_event()) });
            }
        };
        if root.uri() != self.inner.root_uri {
            return Box::pin(async { LspSemanticOperationOutcome::Unavailable });
        }
        if let Some(outcome) = self.terminal_outcome() {
            return Box::pin(async move { outcome });
        }
        let key = SemanticOperationKey {
            root_uri: root.uri().to_owned(),
            request_id,
        };
        let cancellation = CancellationToken::new();
        let inserted = match self.inner.operations.try_lock() {
            Ok(mut operations) => {
                if operations.contains_key(&key) {
                    false
                } else {
                    operations.insert(key.clone(), cancellation.clone());
                    true
                }
            }
            Err(_) => {
                return Box::pin(async {
                    LspSemanticOperationOutcome::Partial {
                        value: serde_json::Value::Null,
                        coverage: "semantic-runtime-busy".to_owned(),
                        detail: None,
                    }
                });
            }
        };
        if !inserted {
            return Box::pin(async {
                LspSemanticOperationOutcome::Partial {
                    value: serde_json::Value::Null,
                    coverage: "semantic-duplicate-operation".to_owned(),
                    detail: None,
                }
            });
        }

        let inner = Arc::clone(&self.inner);
        let analyzer_root = root;
        Box::pin(async move {
            let outcome = tokio::select! {
                () = cancellation.cancelled() => {
                    LspSemanticOperationOutcome::Partial {
                        value: serde_json::Value::Null,
                        coverage: "semantic-cancelled".to_owned(),
                        detail: None,
                    }
                }
                slot = inner.client.lock() => {
                    let mut slot = slot;
                    if slot.is_none()
                        && let Some(outcome) = analyzer_terminal_outcome(&inner)
                    {
                        inner.operations.lock().await.remove(&key);
                        return outcome;
                    }
                    let client = if let Some(client) = slot.take() {
                        mark_analyzer_ready(&inner, &analyzer_root);
                        Ok(Some(client))
                    } else if begin_analyzer_start(&inner, &analyzer_root) {
                        tokio::select! {
                            () = cancellation.cancelled() => Ok(None),
                            client = StdioLspClient::start_with_timeouts(
                                &inner.command,
                                &inner.args,
                                &inner.project_root,
                                inner.timeouts,
                            ) => client.map(Some),
                        }
                    } else {
                        inner.operations.lock().await.remove(&key);
                        return LspSemanticOperationOutcome::Unavailable;
                    };
                    match client {
                        Ok(Some(mut client)) => {
                            mark_analyzer_ready(&inner, &analyzer_root);
                            // The analyzer answers only for documents in its
                            // own view, and this lane is reached for documents
                            // the diagnostics sweep may never have opened.
                            if let Some(document) = document.as_deref() {
                                let _ = client
                                    .ensure_document_open(
                                        document,
                                        &inner.language,
                                        inner.timeouts,
                                    )
                                    .await;
                            }
                            // Boxed: this future covers the whole semantic
                            // request dispatch, so it is the widest one held
                            // across an await in the spawned task. With
                            // profiling enabled it grows past the point where
                            // keeping it inline is worth the stack it costs.
                            let result = Box::pin(client.semantic_request(
                                request,
                                &cancellation,
                                inner.timeouts,
                            ))
                            .await;
                            // Cancellation drops `read_message_until` wherever
                            // it was suspended, so any bytes it had already
                            // consumed into its local header/body buffers are
                            // gone and the stream may be parked mid-frame.
                            // Reusing the client would make every later request
                            // parse from the middle of a message, so retire it
                            // alongside the transport failures.
                            if !matches!(
                                &result,
                                Err(LspSemanticRequestError::Transport { .. }
                                    | LspSemanticRequestError::InvalidResponse { .. }
                                    | LspSemanticRequestError::Cancelled)
                            ) {
                                *slot = Some(client);
                            }
                            match &result {
                                Ok(_)
                                | Err(LspSemanticRequestError::Remote {
                                    code: Some(-32601),
                                    ..
                                }) => record_analyzer_event(
                                    &inner,
                                    &analyzer_root,
                                    AnalyzerEvent::Ready,
                                ),
                                Err(error) => record_analyzer_event(
                                    &inner,
                                    &analyzer_root,
                                    error.analyzer_event(),
                                ),
                            }
                            semantic_operation_outcome(result)
                        }
                        Ok(None) => {
                            record_analyzer_event(
                                &inner,
                                &analyzer_root,
                                AnalyzerEvent::Cancelled,
                            );
                            analyzer_event_outcome(AnalyzerEvent::Cancelled)
                        }
                        Err(error) => {
                            // Coverage is a stable token vocabulary that callers
                            // match on, so the analyzer's own message cannot live
                            // in it: slugifying stripped the punctuation and cut it
                            // mid-word, and a message happening to contain "stale"
                            // steered rename candidates down the wrong branch.
                            // Callers receive only a static typed template; the
                            // daemon-local event keeps the full operational error.
                            record_analyzer_event(
                                &inner,
                                &analyzer_root,
                                AnalyzerEvent::StartupFailed,
                            );
                            analyzer_start_failure(&error)
                        }
                    }
                }
            };
            inner.operations.lock().await.remove(&key);
            outcome
        })
    }

    fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        let key = SemanticOperationKey {
            root_uri: root.uri().to_owned(),
            request_id: request_id.clone(),
        };
        self.inner
            .operations
            .try_lock()
            .ok()
            .and_then(|operations| operations.get(&key).cloned())
            .is_some_and(|cancellation| {
                cancellation.cancel();
                true
            })
    }
}

/// The local file a semantic request addresses, if it names one.
///
/// Every `textDocument/*` method in the closed request protocol carries
/// `params.textDocument.uri`; `workspace/symbol` carries none and needs none.
fn semantic_request_document(request: &crate::LspSemanticRequest) -> Option<PathBuf> {
    let uri = request.params().get("textDocument")?.get("uri")?.as_str()?;
    url::Url::parse(uri).ok()?.to_file_path().ok()
}

fn begin_analyzer_start(inner: &StdioLspSemanticAuthorityInner, root: &AdmittedRoot) -> bool {
    let mut supervisor = inner
        .supervisor
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if supervisor.state() == AnalyzerState::Ready {
        let _ = supervisor.apply(root, AnalyzerEvent::Crashed);
    }
    // `Starting` belongs here too. Only one caller can be starting at a time —
    // the client mutex this runs under is what enforces that — so reaching
    // here in `Starting` means the caller that owned the previous start was
    // dropped mid-flight (its request cancelled, or its spawned operation
    // evicted) and released the lock without concluding the transition. The
    // analyzer is not starting any more, and no event will ever arrive to say
    // so, so refusing left the supervisor stranded and every later semantic
    // request answering `Unavailable` for the rest of the session. This caller
    // takes the start over; it consumes no restart budget, and a failure from
    // `Starting` still charges one.
    if matches!(
        supervisor.state(),
        AnalyzerState::AwaitingStart | AnalyzerState::RestartBackoff | AnalyzerState::Starting
    ) {
        return supervisor
            .apply(root, AnalyzerEvent::StartRequested)
            .is_ok_and(|state| state == AnalyzerState::Starting);
    }
    false
}

fn analyzer_terminal_outcome(
    inner: &StdioLspSemanticAuthorityInner,
) -> Option<LspSemanticOperationOutcome> {
    let supervisor = inner
        .supervisor
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match supervisor.state() {
        AnalyzerState::Exhausted | AnalyzerState::Unavailable => {
            Some(LspSemanticOperationOutcome::Unavailable)
        }
        AnalyzerState::AwaitingStart
        | AnalyzerState::Starting
        | AnalyzerState::Ready
        | AnalyzerState::RestartBackoff => None,
    }
}

fn mark_analyzer_ready(inner: &StdioLspSemanticAuthorityInner, root: &AdmittedRoot) {
    let mut supervisor = inner
        .supervisor
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if matches!(
        supervisor.state(),
        AnalyzerState::AwaitingStart | AnalyzerState::RestartBackoff
    ) {
        let _ = supervisor.apply(root, AnalyzerEvent::StartRequested);
    }
    if supervisor.state() == AnalyzerState::Starting {
        let _ = supervisor.apply(root, AnalyzerEvent::Ready);
    }
}

fn record_analyzer_event(
    inner: &StdioLspSemanticAuthorityInner,
    root: &AdmittedRoot,
    event: AnalyzerEvent,
) {
    let mut supervisor = inner
        .supervisor
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _ = supervisor.apply(root, event);
}

fn analyzer_event_outcome(event: AnalyzerEvent) -> LspSemanticOperationOutcome {
    let Some(coverage) = event.coverage_token() else {
        return LspSemanticOperationOutcome::Unavailable;
    };
    LspSemanticOperationOutcome::Partial {
        value: serde_json::Value::Null,
        coverage: coverage.to_owned(),
        detail: event.failure_detail(),
    }
}

pub(crate) fn analyzer_start_failure(error: &TraceDecayError) -> LspSemanticOperationOutcome {
    eprintln!("[tracedecay] event=analyzer_start_failed error={error}");
    analyzer_event_outcome(AnalyzerEvent::StartupFailed)
}

pub(crate) fn semantic_operation_outcome(
    result: std::result::Result<serde_json::Value, LspSemanticRequestError>,
) -> LspSemanticOperationOutcome {
    match result {
        Ok(value) => LspSemanticOperationOutcome::Complete(value),
        Err(LspSemanticRequestError::Remote {
            code: Some(-32601), ..
        }) => LspSemanticOperationOutcome::Unavailable,
        Err(error) => {
            eprintln!("[tracedecay] event=analyzer_semantic_request_failed error={error}");
            analyzer_event_outcome(error.analyzer_event())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::MAX_ANALYZER_RESTARTS;

    #[tokio::test]
    async fn exhausted_supervisor_rejects_capability_initialization_without_respawning() {
        let authority = StdioLspSemanticAuthority::new(
            "tracedecay-must-not-spawn",
            Vec::new(),
            "rust",
            std::env::temp_dir(),
            "file:///project",
            LspRefreshTimeouts::from_diagnostics_quiet_window(Duration::from_secs(1)),
        );
        let root = AdmittedRoot::new("file:///project");
        for _ in 0..MAX_ANALYZER_RESTARTS {
            begin_analyzer_start(&authority.inner, &root);
            record_analyzer_event(&authority.inner, &root, AnalyzerEvent::StartupFailed);
        }
        let before = authority.analyzer_readiness();
        assert_eq!(before.state(), AnalyzerState::Exhausted);

        let result = authority.upstream_capabilities().await;

        assert_eq!(result, Err(TraceDecayError::Unavailable));
        let after = authority.analyzer_readiness();
        assert_eq!(after.state(), before.state());
        assert_eq!(after.restart_attempts(), before.restart_attempts());
        assert_eq!(after.last_failure(), before.last_failure());
    }

    /// A diagnostics refresh that fails clears the shared client slot
    /// (`collect_refresh_batch`) without telling this supervisor anything. The
    /// analyzer's semantic surface must then restart the process and report
    /// that start's own typed outcome — never the terminal `Unavailable` the
    /// gateway renders as `providerUnavailable` on a live project.
    ///
    /// A session addresses this authority with its *authorized* root, which
    /// carries the resolved scope digest; the supervisor is constructed with
    /// the plain root URI. Comparing whole `AdmittedRoot` values made every
    /// event from this lane a cross-project one, so the restart above was
    /// refused and one missed refresh retired every later semantic request.
    #[tokio::test]
    async fn failed_refresh_restarts_the_analyzer_instead_of_retiring_semantics() {
        let authority = StdioLspSemanticAuthority::new(
            "tracedecay-analyzer-that-cannot-spawn",
            Vec::new(),
            "rust",
            std::env::temp_dir(),
            "file:///project",
            LspRefreshTimeouts::from_diagnostics_quiet_window(Duration::from_secs(1)),
        );
        let owner_root = AdmittedRoot::new("file:///project");
        begin_analyzer_start(&authority.inner, &owner_root);
        mark_analyzer_ready(&authority.inner, &owner_root);
        assert_eq!(authority.analyzer_readiness().state(), AnalyzerState::Ready);
        // Exactly what a failed refresh leaves behind: a Ready supervisor over
        // an empty client slot.
        assert!(authority.inner.client.lock().await.is_none());

        let session_root = AdmittedRoot::authorized(
            "file:///project",
            tracedecay_domain::ManifestDigest::new(format!("sha256:{}", "a".repeat(64)))
                .expect("scope digest"),
        );
        let outcome = authority
            .start(
                session_root,
                LspRequestId::Number(1),
                crate::LspSemanticRequest::from_standard(
                    "textDocument/documentSymbol",
                    serde_json::json!({ "textDocument": { "uri": "file:///project/src/lib.rs" } }),
                ),
            )
            .await;

        assert_eq!(
            outcome,
            LspSemanticOperationOutcome::Partial {
                value: serde_json::Value::Null,
                coverage: "analyzer-start-failed".to_owned(),
                detail: Some("Analyzer failed to start."),
            },
            "a cleared client slot must be answered by a restart attempt, not a retired surface"
        );
        let readiness = authority.analyzer_readiness();
        assert_eq!(readiness.state(), AnalyzerState::RestartBackoff);
        assert_eq!(readiness.last_failure(), Some(AnalyzerEvent::StartupFailed));
    }

    /// A semantic request that owns an in-flight start can be dropped inside
    /// it — its LSP request is cancelled, or its spawned operation is evicted
    /// — releasing the analyzer client lock with the supervisor left in
    /// `Starting` and no event coming to conclude it. The next request holds
    /// that same lock, so nothing is racing it: it must take the start over
    /// rather than read a stranded `Starting` as a refusal and answer
    /// `Unavailable` for the rest of the session.
    #[tokio::test]
    async fn an_abandoned_start_is_taken_over_rather_than_stranding_the_analyzer() {
        let authority = StdioLspSemanticAuthority::new(
            "tracedecay-analyzer-that-cannot-spawn",
            Vec::new(),
            "rust",
            std::env::temp_dir(),
            "file:///project",
            LspRefreshTimeouts::from_diagnostics_quiet_window(Duration::from_secs(1)),
        );
        let owner_root = AdmittedRoot::new("file:///project");
        assert!(begin_analyzer_start(&authority.inner, &owner_root));
        assert_eq!(
            authority.analyzer_readiness().state(),
            AnalyzerState::Starting,
            "the abandoned caller's transition"
        );

        let outcome = authority
            .start(
                AdmittedRoot::new("file:///project"),
                LspRequestId::Number(2),
                crate::LspSemanticRequest::from_standard(
                    "textDocument/documentSymbol",
                    serde_json::json!({ "textDocument": { "uri": "file:///project/src/lib.rs" } }),
                ),
            )
            .await;

        assert_ne!(
            outcome,
            LspSemanticOperationOutcome::Unavailable,
            "a start nobody owns any more must be taken over, not refused"
        );
        assert_eq!(
            authority.analyzer_readiness().last_failure(),
            Some(AnalyzerEvent::StartupFailed)
        );
    }
}
