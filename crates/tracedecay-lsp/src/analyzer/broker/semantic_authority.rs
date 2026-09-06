use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;

use super::super::client::{
    LspRefreshTimeouts, LspSemanticRequestError, StdioLspClient, decode_semantic_request,
};
use super::super::error::{
    AnalyzerCancellation as CancellationToken, AnalyzerRuntimeError as TraceDecayError,
};
use super::shared_client::SharedAnalyzerClient;
use crate::{
    AdmittedRoot, AnalyzerEvent, AnalyzerSupervisor, LspRequestId, LspRuntimeFuture,
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
    shared: Arc<SharedAnalyzerClient>,
    operations: Mutex<BTreeMap<SemanticOperationKey, CancellationToken>>,
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
        let root_uri = root_uri.into();
        Self::from_shared_client(
            command,
            args,
            language,
            project_root,
            root_uri.clone(),
            timeouts,
            SharedAnalyzerClient::new(AdmittedRoot::new(root_uri)),
        )
    }

    pub(crate) fn from_shared_client(
        command: impl Into<String>,
        args: Vec<String>,
        language: impl Into<String>,
        project_root: PathBuf,
        root_uri: impl Into<String>,
        timeouts: LspRefreshTimeouts,
        shared: Arc<SharedAnalyzerClient>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::new(StdioLspSemanticAuthorityInner {
                command: command.into(),
                args,
                language: language.into(),
                project_root,
                root_uri: root_uri.into(),
                timeouts,
                shared,
                operations: Mutex::new(BTreeMap::new()),
            }),
        })
    }

    /// Atomic project-scoped lifecycle evidence for doctor, dashboard, and
    /// other non-LSP callers. The snapshot contains no process or stderr data.
    pub fn analyzer_readiness(&self) -> AnalyzerSupervisor {
        self.inner.shared.supervisor()
    }

    /// Starts the retained client once and returns the typed capabilities from
    /// its successful standard initialize response.
    pub async fn upstream_capabilities(
        &self,
    ) -> std::result::Result<UpstreamCapabilities, TraceDecayError> {
        let mut slot = self.inner.shared.client().lock().await;
        if let Some(client) = slot.as_ref() {
            return Ok(client.upstream_capabilities());
        }
        if self.inner.shared.is_terminal() {
            return Err(TraceDecayError::Unavailable);
        }

        let Some(attempt) = self.inner.shared.begin_start() else {
            return Err(TraceDecayError::Unavailable);
        };
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
                if self.inner.shared.mark_ready(attempt).is_none() {
                    return Err(TraceDecayError::Unavailable);
                }
                *slot = Some(client);
                Ok(capabilities)
            }
            Err(error) => {
                self.inner
                    .shared
                    .record(attempt, AnalyzerEvent::StartupFailed);
                Err(error)
            }
        }
    }

    fn terminal_outcome(&self) -> Option<LspSemanticOperationOutcome> {
        self.inner
            .shared
            .is_terminal()
            .then_some(LspSemanticOperationOutcome::Unavailable)
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
        Box::pin(async move {
            let outcome = tokio::select! {
                () = cancellation.cancelled() => {
                    LspSemanticOperationOutcome::Partial {
                        value: serde_json::Value::Null,
                        coverage: "semantic-cancelled".to_owned(),
                        detail: None,
                    }
                }
                slot = inner.shared.client().lock() => {
                    let mut slot = slot;
                    if slot.is_none()
                        && let Some(outcome) = inner
                            .shared
                            .is_terminal()
                            .then_some(LspSemanticOperationOutcome::Unavailable)
                    {
                        inner.operations.lock().await.remove(&key);
                        return outcome;
                    }
                    // Holding the client lock is what makes a start single
                    // owner, so an ordinary concurrent caller queues here and
                    // then joins whatever this one left behind — the client in
                    // the slot — instead of spawning a competitor. The attempt
                    // this caller ends up owning fences its own late result
                    // out if it is dropped and someone takes the start over.
                    let (attempt, client) = if let Some(client) = slot.take() {
                        (inner.shared.current_attempt(), Ok(Some(client)))
                    } else if let Some(attempt) = inner.shared.begin_start() {
                        let started = tokio::select! {
                            () = cancellation.cancelled() => Ok(None),
                            client = StdioLspClient::start_with_timeouts(
                                &inner.command,
                                &inner.args,
                                &inner.project_root,
                                inner.timeouts,
                            ) => client.map(Some),
                        };
                        (attempt, started)
                    } else {
                        inner.operations.lock().await.remove(&key);
                        return LspSemanticOperationOutcome::Unavailable;
                    };
                    match client {
                        Ok(Some(mut client)) => {
                            let Some(attempt) = inner.shared.mark_ready(attempt) else {
                                // Superseded: this caller no longer owns the
                                // start, so its client is not the session's
                                // analyzer. Drop it rather than installing it
                                // over the replacement's.
                                inner.operations.lock().await.remove(&key);
                                return analyzer_event_outcome(AnalyzerEvent::Cancelled);
                            };
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
                                }) => inner.shared.record(attempt, AnalyzerEvent::Ready),
                                Err(error) => {
                                    inner.shared.record(attempt, error.analyzer_event());
                                }
                            }
                            semantic_operation_outcome(result)
                        }
                        Ok(None) => {
                            inner.shared.record(attempt, AnalyzerEvent::Cancelled);
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
                            inner.shared.record(attempt, AnalyzerEvent::StartupFailed);
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
    use crate::{AnalyzerState, MAX_ANALYZER_RESTARTS};

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
        for _ in 0..MAX_ANALYZER_RESTARTS {
            let attempt = authority.inner.shared.begin_start().expect("start attempt");
            authority
                .inner
                .shared
                .record(attempt, AnalyzerEvent::StartupFailed);
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

    /// A `Ready` supervisor over an empty client slot is a process that left
    /// without either lane recording it. The refresh lane used to leave exactly
    /// this behind on every failed refresh (`collect_refresh_batch` cleared the
    /// slot and told the supervisor nothing); it now records the retirement on
    /// the shared supervisor, so this is the fallback for a client that
    /// vanished. The analyzer's semantic surface must still restart the process
    /// and report that start's own typed outcome — never the terminal
    /// `Unavailable` the gateway renders as `providerUnavailable` on a live
    /// project.
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
        let attempt = authority.inner.shared.begin_start().expect("start attempt");
        authority.inner.shared.mark_ready(attempt).expect("ready");
        assert_eq!(authority.analyzer_readiness().state(), AnalyzerState::Ready);
        // A Ready supervisor over an empty client slot.
        assert!(authority.inner.shared.client().lock().await.is_none());

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
        assert!(authority.inner.shared.begin_start().is_some());
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
    /// A start has exactly one owner. When that owner is dropped mid-flight
    /// and a later caller takes the start over, the abandoned owner can still
    /// come back with a result: it must neither conclude the replacement's
    /// start nor install its client over the replacement's, and its own
    /// failure must not spend a budget the live process never spent.
    #[test]
    fn a_late_result_from_a_superseded_start_is_ignored() {
        let authority = StdioLspSemanticAuthority::new(
            "tracedecay-analyzer-that-cannot-spawn",
            Vec::new(),
            "rust",
            std::env::temp_dir(),
            "file:///project",
            LspRefreshTimeouts::from_diagnostics_quiet_window(Duration::from_secs(1)),
        );
        let abandoned = authority.inner.shared.begin_start().expect("first attempt");
        let replacement = authority.inner.shared.begin_start().expect("takeover");
        assert_ne!(abandoned, replacement, "the takeover is its own attempt");

        assert_eq!(
            authority.inner.shared.mark_ready(abandoned),
            None,
            "a superseded start cannot conclude the one that replaced it"
        );
        authority
            .inner
            .shared
            .record(abandoned, AnalyzerEvent::StartupFailed);
        let readiness = authority.analyzer_readiness();
        assert_eq!(readiness.state(), AnalyzerState::Starting);
        assert_eq!(readiness.attempt(), replacement);
        assert_eq!(readiness.restart_attempts(), 0);
        assert_eq!(readiness.last_failure(), None);

        assert_eq!(
            authority.inner.shared.mark_ready(replacement),
            Some(replacement),
            "the attempt that owns the start still concludes it"
        );
        assert_eq!(authority.analyzer_readiness().state(), AnalyzerState::Ready);
    }

    /// An ordinary concurrent caller joins the start already running rather
    /// than opening a competing one. The shared client lock is what makes that
    /// true: while a live start holds it, a second request cannot reach the
    /// supervisor at all, so it opens no attempt and charges no budget, and it
    /// continues from whatever that start leaves behind.
    #[tokio::test]
    async fn a_concurrent_caller_joins_a_live_start_instead_of_competing() {
        let authority = StdioLspSemanticAuthority::new(
            "tracedecay-analyzer-that-cannot-spawn",
            Vec::new(),
            "rust",
            std::env::temp_dir(),
            "file:///project",
            LspRefreshTimeouts::from_diagnostics_quiet_window(Duration::from_secs(1)),
        );
        // Exactly what a live start looks like from outside: its owner holds
        // the shared client lock, and the supervisor is `Starting` on its
        // attempt.
        let held = authority.inner.shared.client().lock().await;
        let live = authority.inner.shared.begin_start().expect("live attempt");

        let joining = tokio::spawn({
            let authority = Arc::clone(&authority);
            async move {
                authority
                    .start(
                        AdmittedRoot::new("file:///project"),
                        LspRequestId::Number(2),
                        crate::LspSemanticRequest::from_standard(
                            "textDocument/documentSymbol",
                            serde_json::json!({
                                "textDocument": { "uri": "file:///project/src/lib.rs" }
                            }),
                        ),
                    )
                    .await
            }
        });
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }

        let readiness = authority.analyzer_readiness();
        assert!(
            !joining.is_finished(),
            "the joining caller waits on the live start"
        );
        assert_eq!(readiness.attempt(), live, "no competing start attempt");
        assert_eq!(readiness.state(), AnalyzerState::Starting);
        assert_eq!(readiness.restart_attempts(), 0);

        drop(held);
        assert_ne!(
            joining.await.expect("joining caller"),
            LspSemanticOperationOutcome::Unavailable,
            "a caller that joined a live start must not be answered as retired"
        );
    }
}
