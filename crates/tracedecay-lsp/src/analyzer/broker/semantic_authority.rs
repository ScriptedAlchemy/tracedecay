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
    LspSemanticOperationOutcome, LspSemanticRequestAuthority,
};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SemanticOperationKey {
    root_uri: String,
    request_id: LspRequestId,
}

struct StdioLspSemanticAuthorityInner {
    command: String,
    args: Vec<String>,
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
        project_root: PathBuf,
        root_uri: impl Into<String>,
        timeouts: LspRefreshTimeouts,
    ) -> Arc<Self> {
        Self::from_shared_client(
            command,
            args,
            project_root,
            root_uri,
            timeouts,
            Arc::new(Mutex::new(None)),
        )
    }

    pub(crate) fn from_shared_client(
        command: impl Into<String>,
        args: Vec<String>,
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
                    } else {
                        begin_analyzer_start(&inner, &analyzer_root);
                        tokio::select! {
                            () = cancellation.cancelled() => Ok(None),
                            client = StdioLspClient::start_with_timeouts(
                                &inner.command,
                                &inner.args,
                                &inner.project_root,
                                inner.timeouts,
                            ) => client.map(Some),
                        }
                    };
                    match client {
                        Ok(Some(mut client)) => {
                            mark_analyzer_ready(&inner, &analyzer_root);
                            let result = client
                                .semantic_request(request, &cancellation, inner.timeouts)
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

fn begin_analyzer_start(inner: &StdioLspSemanticAuthorityInner, root: &AdmittedRoot) {
    let mut supervisor = inner
        .supervisor
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if supervisor.state() == AnalyzerState::Ready {
        let _ = supervisor.apply(root, AnalyzerEvent::Crashed);
    }
    if matches!(
        supervisor.state(),
        AnalyzerState::AwaitingStart | AnalyzerState::RestartBackoff
    ) {
        let _ = supervisor.apply(root, AnalyzerEvent::StartRequested);
    }
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
