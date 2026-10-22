use std::future::poll_fn;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, Waker};
use std::time::Instant;

use super::{Diagnostics, Feedback};
use crate::gateway::{
    AnalyzerCancellationAdapter, LspRuntimeFuture, LspRuntimeSpawner, LspRuntimeTask,
    LspSemanticOperationOutcome, LspSemanticRequest, LspSemanticRequestAuthority,
    SemanticProviderAdapter, SemanticProviderOutcome, UnavailableSemanticProvider,
};
use crate::protocol::*;
use crate::provider::{AnalyzerSemanticAdapter, AnalyzerState};
use std::sync::Mutex;

#[derive(Default)]
struct ControlledRuntime(Mutex<Vec<Arc<ControlledTask>>>);

struct ControlledTask {
    future: Mutex<Option<LspRuntimeFuture<()>>>,
    aborted: AtomicBool,
}

impl LspRuntimeTask for Arc<ControlledTask> {
    fn abort(&self) {
        self.aborted.store(true, Ordering::Release);
        self.future.lock().unwrap().take();
    }
}

impl LspRuntimeSpawner for ControlledRuntime {
    fn spawn(&self, future: LspRuntimeFuture<()>) -> Box<dyn LspRuntimeTask> {
        let task = Arc::new(ControlledTask {
            future: Mutex::new(Some(future)),
            aborted: AtomicBool::new(false),
        });
        self.0.lock().unwrap().push(Arc::clone(&task));
        Box::new(task)
    }
}

impl ControlledRuntime {
    fn poll(&self) {
        for task in self.0.lock().unwrap().iter() {
            let mut future = task.future.lock().unwrap();
            if future.as_mut().is_some_and(|future| {
                future
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_ready()
            }) {
                future.take();
            }
        }
    }

    fn assert_cancelled(&self) {
        let tasks = self.0.lock().unwrap();
        assert!(
            !tasks.is_empty(),
            "the canonical adapter must have started work"
        );
        assert!(
            tasks
                .iter()
                .all(|task| task.future.lock().unwrap().is_none())
        );
        assert!(
            tasks
                .iter()
                .any(|task| task.aborted.load(Ordering::Acquire))
        );
    }
}

#[derive(Default)]
struct ControlledAuthority {
    started: Mutex<Vec<(String, LspRequestId, LspSemanticRequest)>>,
    cancelled: Mutex<Vec<(String, LspRequestId)>>,
    results: Arc<Mutex<BTreeMap<String, LspSemanticOperationOutcome>>>,
}

impl LspSemanticRequestAuthority for ControlledAuthority {
    fn start(
        &self,
        root: AdmittedRoot,
        request_id: LspRequestId,
        request: LspSemanticRequest,
    ) -> LspRuntimeFuture<LspSemanticOperationOutcome> {
        self.started
            .lock()
            .unwrap()
            .push((root.uri().to_owned(), request_id, request));
        let results = Arc::clone(&self.results);
        Box::pin(poll_fn(move |_| {
            results
                .lock()
                .unwrap()
                .remove(root.uri())
                .map_or(Poll::Pending, Poll::Ready)
        }))
    }

    fn cancel_request(&self, root: &AdmittedRoot, request_id: &LspRequestId) -> bool {
        self.cancelled
            .lock()
            .unwrap()
            .push((root.uri().to_owned(), request_id.clone()));
        true
    }
}

type TestSemantics =
    Arc<AnalyzerSemanticAdapter<Arc<SemanticProviderAdapter>, UnavailableSemanticProvider>>;

struct Harness {
    session: DaemonLspProtocolSession<Feedback, TestSemantics, Diagnostics>,
    runtime: Arc<ControlledRuntime>,
    authority: Arc<ControlledAuthority>,
}

impl Harness {
    fn new() -> Self {
        let workspace = AuthorizedLspWorkspace::new(
            Some(ManifestDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap()),
            vec![
                AdmittedRoot::authorized(
                    "file:///left",
                    ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
                ),
                AdmittedRoot::authorized(
                    "file:///right",
                    ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
                ),
            ],
        )
        .unwrap();
        let runtime = Arc::new(ControlledRuntime::default());
        let authority = Arc::new(ControlledAuthority::default());
        let provider = SemanticProviderAdapter::shared(runtime.clone(), authority.clone());
        let semantics = Arc::new(AnalyzerSemanticAdapter::new(
            AnalyzerState::Ready,
            provider.clone(),
            UnavailableSemanticProvider,
        ));
        let gateway_capabilities = GatewayCapabilities {
            supports_workspace_folders: true,
            ..GatewayCapabilities::default()
        };
        let upstream = UpstreamCapabilities {
            supports_diagnostics: false,
            semantic: [
                crate::capabilities::SemanticCapability::WorkspaceSymbol,
                crate::capabilities::SemanticCapability::DocumentSymbol,
            ]
            .into_iter()
            .collect(),
        };
        let mut session = DaemonLspProtocolSession::from_workspace_ports(
            workspace,
            negotiate_capabilities(
                &ClientCapabilities::default(),
                &gateway_capabilities,
                &upstream,
            ),
            gateway_capabilities,
            upstream,
            Feedback::default(),
            semantics,
            Diagnostics,
        )
        .with_cancellation_port(AnalyzerCancellationAdapter::new(provider));
        session.handle_payload(&serde_json::to_vec(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "workspaceFolders": [{"uri": "file:///left", "name": "left"}, {"uri": "file:///right", "name": "right"}],
                "capabilities": {
                    "workspace": {"workspaceFolders": true, "symbol": {}},
                    "textDocument": {"documentSymbol": {}},
                    "general": {"positionEncodings": ["utf-16"]}
                }
            }
        })).unwrap(), 0);
        let initialized: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
        assert_eq!(
            initialized["result"]["capabilities"]["workspaceSymbolProvider"],
            json!({"resolveProvider": false})
        );
        session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
            1,
        );
        Self {
            session,
            runtime,
            authority,
        }
    }

    fn start_symbols(&mut self) {
        self.session.handle_payload(
            br#"{"jsonrpc":"2.0","id":2,"method":"workspace/symbol","params":{"query":"needle"}}"#,
            10,
        );
        assert_eq!(self.session.drain_outbound(), Vec::<Vec<u8>>::new());
        let started = self.authority.started.lock().unwrap();
        assert_eq!(started.len(), 2);
        let deadline = self.session.semantic.pending[&LspRequestId::Number(2)].deadline;
        for (root, id, request) in started.iter() {
            assert!(matches!(root.as_str(), "file:///left" | "file:///right"));
            assert_eq!(*id, LspRequestId::Number(2));
            assert_eq!(request.method(), "workspace/symbol");
            assert_eq!(request.params()["query"], "needle");
            assert_eq!(request.deadline(), Some(deadline));
        }
    }

    fn complete(&self, root: &str) {
        self.authority.results.lock().unwrap().insert(root.to_owned(), LspSemanticOperationOutcome::Complete(json!([{
            "name": root, "kind": 1,
            "location": {"uri": format!("{root}/src/lib.rs"), "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}}
        }])));
        self.runtime.poll();
    }

    fn assert_cancelled(&mut self, response_code: Option<i64>) {
        assert!(self.session.semantic.pending.is_empty());
        assert_eq!(
            *self.authority.cancelled.lock().unwrap(),
            vec![
                ("file:///left".to_owned(), LspRequestId::Number(2)),
                ("file:///right".to_owned(), LspRequestId::Number(2)),
            ]
        );
        self.runtime.assert_cancelled();
        let responses = self.session.drain_outbound();
        if let Some(code) = response_code {
            assert_eq!(responses.len(), 1);
            let response: Value = serde_json::from_slice(&responses[0]).unwrap();
            assert_eq!(response["id"], 2);
            assert_eq!(response["error"]["code"], code);
        } else {
            assert_eq!(responses, Vec::<Vec<u8>>::new());
        }
        self.complete("file:///left");
        self.complete("file:///right");
        self.session.flush_due(20_000);
        assert!(
            self.session.drain_outbound().is_empty(),
            "cancelled work cannot publish a late result"
        );
    }
}

#[test]
fn workspace_symbols_keep_admitted_deadline_and_completed_roots_across_polls() {
    let mut harness = Harness::new();
    harness.start_symbols();
    harness.complete("file:///left");
    harness.session.flush_due(11);
    assert_eq!(harness.session.drain_outbound(), Vec::<Vec<u8>>::new());
    harness.session.flush_due(12);
    assert_eq!(
        harness.authority.started.lock().unwrap().len(),
        2,
        "a completed root must not restart"
    );
    harness.complete("file:///right");
    harness.session.flush_due(13);
    let responses = harness.session.drain_outbound();
    assert_eq!(responses.len(), 1);
    let response: Value = serde_json::from_slice(&responses[0]).unwrap();
    assert_eq!(response["result"][0]["name"], "file:///left");
    assert_eq!(response["result"][1]["name"], "file:///right");
    assert_eq!(response["result"].as_array().unwrap().len(), 2);
    assert!(harness.session.semantic.pending.is_empty());
}

#[test]
fn workspace_symbol_cancellation_and_deadline_stop_every_admitted_root() {
    for deadline in [false, true] {
        let mut harness = Harness::new();
        harness.start_symbols();
        if deadline {
            harness
                .session
                .flush_due(10 + DEFAULT_LSP_REQUEST_DEADLINE_MS);
        } else {
            harness.session.handle_payload(
                br#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":2}}"#,
                11,
            );
        }
        harness.assert_cancelled(Some(if deadline { -32802 } else { -32800 }));
    }
}

#[test]
fn workspace_symbol_removal_cancels_captured_roots_after_workspace_replacement() {
    let mut harness = Harness::new();
    harness.start_symbols();
    let roots = harness
        .session
        .lifecycle
        .gateway
        .workspace()
        .roots()
        .to_vec();
    harness
        .session
        .lifecycle
        .gateway
        .replace_workspace(AuthorizedLspWorkspace::single(roots[1].clone()));
    harness
        .session
        .clear_removed_workspace_root_state(&roots[..1]);
    harness.assert_cancelled(Some(-32800));
}

#[test]
fn workspace_symbol_session_expiry_stops_work_without_publishing() {
    let mut harness = Harness::new();
    harness.start_symbols();
    harness.session.expire();
    harness.assert_cancelled(None);
}

#[test]
fn workspace_symbol_partial_answer_stops_other_pending_roots() {
    let mut harness = Harness::new();
    harness.start_symbols();
    harness.authority.results.lock().unwrap().insert(
        "file:///left".to_owned(),
        LspSemanticOperationOutcome::Partial {
            value: json!([]),
            coverage: "analyzer-timed-out".to_owned(),
            detail: None,
        },
    );
    harness.runtime.poll();
    harness.session.flush_due(11);
    harness.assert_cancelled(Some(-32802));
}

#[test]
fn workspace_symbol_shutdown_cancels_pending_fan_out() {
    let mut harness = Harness::new();
    harness.start_symbols();
    harness.session.cancel_pending_operations();
    harness.assert_cancelled(Some(-32800));
}

#[test]
fn shared_analyzer_adapter_preserves_document_snapshot_and_deadline() {
    let mut harness = Harness::new();
    harness.session.handle_payload(br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///left/src/lib.rs","languageId":"rust","version":7,"text":"fn unsaved() {}"}}}"#, 2);
    let before = Instant::now();
    harness.session.handle_payload(br#"{"jsonrpc":"2.0","id":3,"method":"textDocument/documentSymbol","params":{"textDocument":{"uri":"file:///left/src/lib.rs"}}}"#, 3);
    let started = harness.authority.started.lock().unwrap();
    assert_eq!(started.len(), 1);
    let request = &started[0].2;
    assert_eq!(
        request.deadline(),
        Some(harness.session.semantic.pending[&LspRequestId::Number(3)].deadline)
    );
    assert!(request.deadline().unwrap() > before);
    let snapshot = request
        .document_snapshot()
        .expect("unsaved snapshot must survive both shared provider wrappers");
    assert_eq!(snapshot.text.as_ref(), "fn unsaved() {}");
    assert_eq!(snapshot.version, 7);
    drop(started);
    harness.session.expire();
    harness.runtime.assert_cancelled();
}

#[test]
fn completed_semantics_cannot_escape_a_closed_overlay_lifetime() {
    let root = AdmittedRoot::authorized(
        "file:///left",
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
    );
    let uri = "file:///left/src/lib.rs";
    let mut overlays = crate::OverlayStore::default();
    let snapshot = crate::LspSemanticDocumentSnapshot::from_overlay(
        &overlays.open(&root, uri, "rust", 7, "fn old() {}").unwrap(),
    );
    let runtime = Arc::new(ControlledRuntime::default());
    let authority = Arc::new(ControlledAuthority::default());
    let provider = SemanticProviderAdapter::shared(runtime.clone(), authority.clone());
    let request = SemanticRequest::DocumentSymbols {
        document_uri: uri.to_owned(),
    };
    let id = LspRequestId::Number(2);
    assert_eq!(
        provider.request_with_document(&root, &id, &request, Some(&snapshot), None),
        SemanticProviderOutcome::Pending
    );
    authority.results.lock().unwrap().insert(
        root.uri().to_owned(),
        LspSemanticOperationOutcome::Complete(json!([{
            "name": "old", "kind": 12,
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 11}},
            "selectionRange": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 6}}
        }])),
    );
    runtime.poll();
    overlays.close(uri).unwrap();
    let reopened = crate::LspSemanticDocumentSnapshot::from_overlay(
        &overlays.open(&root, uri, "rust", 7, "fn new() {}").unwrap(),
    );
    for _ in 0..2 {
        assert!(matches!(
            provider.request_with_document(&root, &id, &request, Some(&snapshot), None),
            SemanticProviderOutcome::Partial { coverage, .. }
                if coverage == "analyzer-cancelled"
        ));
    }
    assert_eq!(authority.started.lock().unwrap().len(), 1);
    assert_eq!(
        provider.request_with_document(
            &root,
            &LspRequestId::Number(3),
            &request,
            Some(&reopened),
            None,
        ),
        SemanticProviderOutcome::Pending
    );
    assert_eq!(authority.started.lock().unwrap().len(), 2);
    provider.cancel_request(&root, &LspRequestId::Number(3));
}
