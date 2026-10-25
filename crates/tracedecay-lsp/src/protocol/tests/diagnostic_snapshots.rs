use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use serde_json::{Value, json};
use tracedecay_domain::test_fixtures::digest;

use super::{Feedback, Semantics};
use crate::analyzer::broker::DiagnosticSeverity;
use crate::capabilities::{
    ClientCapabilities, GatewayCapabilities, UpstreamCapabilities, negotiate_capabilities,
};
use crate::diagnostics::{DiagnosticSource, GatewayDiagnostic, LspPosition, LspRange};
use crate::gateway::{
    AdmittedRoot, LspRuntimeFailure, LspRuntimeFuture, LspRuntimeSpawner, LspRuntimeTask,
};
use crate::overlay::{
    CanonicalDiagnosticRefreshRequest, CanonicalDiagnosticSnapshotAuthority,
    DiagnosticSnapshotAdapter, OverlaySnapshot,
};
use crate::protocol::{DaemonLspProtocolSession, MAX_QUEUED_OUTBOUND_MESSAGES};
use crate::provider::{
    DiagnosticRefreshAdmission, DiagnosticSnapshotOutcome, DiagnosticSnapshotPort,
    GenerationDiagnostics,
};
use crate::session::AuthorizedLspWorkspace;
use crate::workspace_diagnostics::{
    MAX_WORKSPACE_DIAGNOSTIC_BYTES, MAX_WORKSPACE_DIAGNOSTIC_RESULTS,
};

const DOCUMENT: &str = "file:///root/a.rs";

#[derive(Default)]
struct ControlledRuntime(Mutex<Vec<Arc<ControlledTask>>>);

struct ControlledTask(Mutex<Option<LspRuntimeFuture<()>>>);

impl LspRuntimeTask for Arc<ControlledTask> {
    // Accepted work can finish after an advisory abort. Keep the future so a
    // test can explicitly deliver that late completion to the retired channel.
    fn abort(&self) {}
}

impl LspRuntimeSpawner for ControlledRuntime {
    fn spawn(&self, future: LspRuntimeFuture<()>) -> Box<dyn LspRuntimeTask> {
        let task = Arc::new(ControlledTask(Mutex::new(Some(future))));
        self.0.lock().unwrap().push(Arc::clone(&task));
        Box::new(task)
    }
}

impl ControlledRuntime {
    fn complete_next(&self) {
        let tasks = self.0.lock().unwrap();
        let mut future = tasks
            .iter()
            .find_map(|task| task.0.lock().unwrap().take())
            .expect("an admitted diagnostic computation must be waiting");
        assert_eq!(
            future
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop())),
            Poll::Ready(()),
        );
    }

    fn started(&self) -> usize {
        self.0.lock().unwrap().len()
    }
}

#[derive(Default)]
struct DiagnosticAuthority {
    generation: AtomicU64,
    next_generation: Mutex<Option<u64>>,
    next_failure: Mutex<Option<&'static str>>,
    next_code: Mutex<Option<String>>,
}

impl CanonicalDiagnosticSnapshotAuthority for DiagnosticAuthority {
    fn refresh(
        &self,
        request: CanonicalDiagnosticRefreshRequest,
    ) -> LspRuntimeFuture<Result<GenerationDiagnostics, LspRuntimeFailure>> {
        let next_generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let generation = self
            .next_generation
            .lock()
            .unwrap()
            .take()
            .unwrap_or(next_generation);
        let failure = self.next_failure.lock().unwrap().take();
        let code = self
            .next_code
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| "retained-snapshot".to_owned());
        let version = request
            .overlay
            .as_ref()
            .map_or(0, |overlay| overlay.version);
        Box::pin(async move {
            if let Some(failure) = failure {
                return Err(LspRuntimeFailure::new(failure));
            }
            Ok(GenerationDiagnostics {
                generation,
                authority_digest: digest('a'),
                upstream: vec![GatewayDiagnostic {
                    uri: request.document_uri,
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
                    code: Some(code),
                    code_description_uri: None,
                    message: format!("generation {generation}, version {version}"),
                    source: DiagnosticSource::Upstream,
                    related_information: Vec::new(),
                    data: None,
                }],
                tracedecay: Vec::new(),
            })
        })
    }
}

// This wrapper only selects a scheduler interleaving. All operation admission,
// completion ownership, and retirement still belong to the real adapter.
struct ControlledDiagnosticPort {
    adapter: DiagnosticSnapshotAdapter,
    runtime: Arc<ControlledRuntime>,
    complete_after_read: AtomicBool,
    reads: AtomicUsize,
}

impl DiagnosticSnapshotPort for ControlledDiagnosticPort {
    fn document_diagnostics(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        overlay: Option<&OverlaySnapshot>,
    ) -> DiagnosticSnapshotOutcome {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let outcome = self
            .adapter
            .document_diagnostics(root, document_uri, overlay);
        if matches!(outcome, DiagnosticSnapshotOutcome::Refreshing(_))
            && self.complete_after_read.swap(false, Ordering::SeqCst)
        {
            self.runtime.complete_next();
        }
        outcome
    }

    fn request_document_refresh(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        overlay: Option<&OverlaySnapshot>,
        source_generation: Option<u64>,
    ) -> DiagnosticRefreshAdmission {
        self.adapter
            .request_document_refresh(root, document_uri, overlay, source_generation)
    }

    fn retire_document_refresh(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        operation_id: &str,
    ) -> bool {
        self.adapter
            .retire_document_refresh(root, document_uri, operation_id)
    }
}

struct Harness {
    session: DaemonLspProtocolSession<Feedback, Semantics, ControlledDiagnosticPort>,
    runtime: Arc<ControlledRuntime>,
    authority: Arc<DiagnosticAuthority>,
    now_ms: u64,
    next_request_id: u64,
}

impl Harness {
    fn new(push: bool) -> Self {
        let runtime = Arc::new(ControlledRuntime::default());
        let authority = Arc::new(DiagnosticAuthority::default());
        let provider = ControlledDiagnosticPort {
            adapter: DiagnosticSnapshotAdapter::new(runtime.clone(), authority.clone()),
            runtime: runtime.clone(),
            complete_after_read: AtomicBool::new(false),
            reads: AtomicUsize::new(0),
        };
        let capabilities = GatewayCapabilities {
            supports_workspace_folders: true,
            ..GatewayCapabilities::default()
        };
        let upstream = UpstreamCapabilities {
            supports_diagnostics: true,
            ..UpstreamCapabilities::default()
        };
        let workspace = AuthorizedLspWorkspace::new(
            Some(digest('c')),
            vec![AdmittedRoot::authorized("file:///root", digest('b'))],
        )
        .unwrap();
        let session = DaemonLspProtocolSession::from_workspace_ports(
            workspace,
            negotiate_capabilities(&ClientCapabilities::default(), &capabilities, &upstream),
            capabilities,
            upstream,
            Feedback::default(),
            Semantics,
            provider,
        );
        let mut harness = Self {
            session,
            runtime,
            authority,
            now_ms: 0,
            next_request_id: 1,
        };
        let mut capabilities = json!({
            "general": { "positionEncodings": ["utf-16"] },
            "textDocument": { "diagnostic": {} },
            "workspace": { "workspaceFolders": true },
        });
        if push {
            capabilities["textDocument"]["publishDiagnostics"] = json!({ "versionSupport": true });
        }
        let initialized = harness.request(
            "initialize",
            json!({ "rootUri": "file:///root", "capabilities": capabilities }),
        );
        assert!(initialized.get("error").is_none(), "{initialized}");
        harness.notify("initialized", json!({}));
        assert_eq!(
            harness
                .session
                .lifecycle
                .gateway
                .capabilities()
                .supports_publish_diagnostics,
            push,
        );
        harness
    }

    fn send(&mut self, value: Value) -> Vec<Value> {
        self.now_ms += 1;
        self.session
            .handle_payload(&serde_json::to_vec(&value).unwrap(), self.now_ms);
        self.messages()
    }

    fn messages(&mut self) -> Vec<Value> {
        self.session
            .drain_outbound()
            .iter()
            .map(|message| serde_json::from_slice(message).unwrap())
            .collect()
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_request_id;
        self.next_request_id += 1;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .into_iter()
            .find(|message| message["id"] == id)
            .expect("the protocol must answer the request")
    }

    fn notify(&mut self, method: &str, params: Value) -> Vec<Value> {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    fn open(&mut self, version: i64) {
        self.notify("textDocument/didOpen", json!({
            "textDocument": { "uri": DOCUMENT, "languageId": "rust", "version": version, "text": "fn a() {}" },
        }));
        assert_eq!(self.session.overlays().version(DOCUMENT), Some(version));
    }

    fn start_refresh(&mut self) {
        self.now_ms += crate::overlay::OVERLAY_DIAGNOSTIC_DEBOUNCE_MS;
        self.session.flush_due(self.now_ms);
        self.messages();
    }

    fn pull(&mut self, previous_result: Option<&str>) -> Value {
        self.pull_document(DOCUMENT, previous_result)
    }

    fn pull_document(&mut self, uri: &str, previous_result: Option<&str>) -> Value {
        let mut params = json!({ "textDocument": { "uri": uri } });
        if let Some(previous) = previous_result {
            params["previousResultId"] = json!(previous);
        }
        self.request("textDocument/diagnostic", params)
    }
}

fn assert_full(response: &Value, generation: u64, version: i64) -> String {
    assert_eq!(response["result"]["kind"], "full", "{response}");
    assert_eq!(
        response["result"]["items"],
        expected_items(generation, version)
    );
    response["result"]["resultId"].as_str().unwrap().to_owned()
}

fn expected_items(generation: u64, version: i64) -> Value {
    json!([{
        "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } },
        "severity": 2,
        "code": "retained-snapshot",
        "source": "upstream",
        "message": format!("generation {generation}, version {version}"),
    }])
}

#[test]
fn completed_refresh_is_a_repeatable_document_snapshot() {
    let mut harness = Harness::new(true);
    harness.open(1);
    harness.start_refresh();
    assert_eq!(harness.runtime.started(), 1);
    harness.runtime.complete_next();

    let result_id = assert_full(&harness.pull(None), 1, 1);
    let repeated = harness.pull(Some(&result_id));
    assert_eq!(
        repeated["result"],
        json!({ "kind": "unchanged", "resultId": result_id })
    );
    assert_eq!(
        harness.runtime.started(),
        1,
        "reading a fresh snapshot must not restart computation"
    );
}

#[test]
fn background_publication_preserves_the_snapshot_for_pull() {
    let mut harness = Harness::new(true);
    harness.open(1);
    harness.start_refresh();
    harness.runtime.complete_next();
    harness.session.flush_due(harness.now_ms);
    let messages = harness.messages();
    let publication = messages
        .iter()
        .find(|message| message["method"] == "textDocument/publishDiagnostics")
        .expect("completed refresh must publish its diagnostics");
    assert_eq!(publication["params"]["diagnostics"], expected_items(1, 1));

    assert_full(&harness.pull(None), 1, 1);
    assert_eq!(harness.runtime.started(), 1);
}

#[test]
fn pull_only_clients_keep_completions_between_protocol_ticks() {
    let mut harness = Harness::new(false);
    harness.open(1);
    harness.start_refresh();
    harness.runtime.complete_next();
    harness.session.flush_due(harness.now_ms);
    assert!(harness.messages().is_empty(), "push is not negotiated");

    let result_id = assert_full(&harness.pull(None), 1, 1);
    assert_eq!(
        harness.pull(Some(&result_id))["result"]["kind"],
        "unchanged"
    );
    assert_eq!(harness.runtime.started(), 1);
}

#[test]
fn a_pending_pull_becomes_full_after_its_admitted_work_completes() {
    let mut harness = Harness::new(false);
    harness.open(1);
    let pending = harness.pull(None);
    assert!(pending.get("error").is_some(), "{pending}");
    assert!(pending.get("result").is_none());
    assert_eq!(harness.runtime.started(), 1);

    harness.runtime.complete_next();
    assert_full(&harness.pull(None), 1, 1);
    harness.start_refresh();
    assert_eq!(harness.runtime.started(), 1);
}

#[test]
fn completion_between_reads_is_retained_for_later_pulls() {
    let mut harness = Harness::new(false);
    harness.open(1);
    harness.start_refresh();
    harness
        .session
        .diagnostics
        .provider
        .complete_after_read
        .store(true, Ordering::SeqCst);

    let first = harness.pull(None);
    assert!(
        !harness
            .session
            .diagnostics
            .provider
            .complete_after_read
            .load(Ordering::SeqCst)
    );
    let full = if first.get("error").is_some() {
        assert_eq!(first["error"]["code"], -32802);
        harness.pull(None)
    } else {
        first
    };
    let result_id = assert_full(&full, 1, 1);
    assert_eq!(
        harness.pull(Some(&result_id))["result"]["kind"],
        "unchanged"
    );
    assert_eq!(harness.runtime.started(), 1);
}

#[test]
fn content_changes_invalidate_the_retained_document_snapshot() {
    let mut harness = Harness::new(false);
    harness.open(1);
    harness.start_refresh();
    harness.runtime.complete_next();
    let old_result = assert_full(&harness.pull(None), 1, 1);

    harness.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": DOCUMENT, "version": 2 },
            "contentChanges": [{ "text": "fn changed() {}" }],
        }),
    );
    let pending = harness.pull(Some(&old_result));
    assert_eq!(pending["error"]["code"], -32802, "{pending}");
    assert!(pending.get("result").is_none());
    harness.runtime.complete_next();
    let new_result = assert_full(&harness.pull(Some(&old_result)), 2, 2);
    assert_ne!(old_result, new_result);
    assert_eq!(
        harness.pull(Some(&new_result))["result"]["kind"],
        "unchanged"
    );
    assert_eq!(harness.runtime.started(), 2);
}

#[test]
fn save_refreshes_a_snapshot_even_when_the_overlay_version_is_unchanged() {
    let mut harness = Harness::new(false);
    harness.open(1);
    harness.start_refresh();
    harness.runtime.complete_next();
    let old_result = assert_full(&harness.pull(None), 1, 1);

    harness.notify(
        "textDocument/didSave",
        json!({ "textDocument": { "uri": DOCUMENT } }),
    );
    let pending = harness.pull(Some(&old_result));
    assert_eq!(pending["error"]["code"], -32802, "{pending}");
    harness.runtime.complete_next();
    let new_result = assert_full(&harness.pull(Some(&old_result)), 2, 1);
    assert_ne!(old_result, new_result);
    assert_eq!(harness.runtime.started(), 2);
}

#[test]
fn late_completion_cannot_answer_a_reopened_document_lifetime() {
    let mut harness = Harness::new(false);
    harness.open(1);
    harness.start_refresh();
    harness.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": DOCUMENT } }),
    );
    harness.open(1);
    assert_eq!(harness.pull(None)["error"]["code"], -32802);
    assert_eq!(harness.runtime.started(), 2);

    harness.runtime.complete_next();
    let still_pending = harness.pull(None);
    assert_eq!(still_pending["error"]["code"], -32802, "{still_pending}");
    assert!(still_pending.get("result").is_none());
    assert_eq!(
        harness.runtime.started(),
        2,
        "the successor must keep its operation"
    );
    harness.runtime.complete_next();
    assert_full(&harness.pull(None), 2, 1);
    assert_eq!(harness.runtime.started(), 2);
}

#[test]
fn workspace_scope_changes_invalidate_retained_document_evidence() {
    let mut harness = Harness::new(false);
    harness.open(1);
    harness.start_refresh();
    harness.runtime.complete_next();
    let old_result = assert_full(&harness.pull(None), 1, 1);

    harness.notify(
        "workspace/didChangeWorkspaceFolders",
        json!({
            "event": { "added": [{ "uri": "file:///other", "name": "other" }], "removed": [] },
        }),
    );
    let mutation = harness.session.pending_workspace_folder_mutation().unwrap();
    let workspace = AuthorizedLspWorkspace::new(
        Some(digest('d')),
        vec![
            AdmittedRoot::authorized("file:///root", digest('b')),
            AdmittedRoot::authorized("file:///other", digest('e')),
        ],
    )
    .unwrap();
    harness
        .session
        .apply_workspace_folder_mutation(&mutation, workspace)
        .unwrap();
    let pending = harness.pull(Some(&old_result));
    assert_eq!(pending["error"]["code"], -32802, "{pending}");
    harness.runtime.complete_next();
    let new_result = assert_full(&harness.pull(Some(&old_result)), 2, 1);
    assert_ne!(old_result, new_result);
    assert_eq!(harness.runtime.started(), 2);
}

#[test]
fn unopened_documents_have_repeatable_completed_reports() {
    let mut harness = Harness::new(false);
    assert_eq!(harness.pull(None)["error"]["code"], -32802);
    harness.runtime.complete_next();
    let result_id = assert_full(&harness.pull(None), 1, 0);
    assert_eq!(
        harness.pull(Some(&result_id))["result"]["kind"],
        "unchanged"
    );
    assert_eq!(harness.runtime.started(), 1);
}

#[test]
fn unopened_document_snapshots_reuse_the_existing_result_bound() {
    let mut harness = Harness::new(false);
    let mut latest = None;
    for index in 0..=MAX_WORKSPACE_DIAGNOSTIC_RESULTS {
        let uri = format!("file:///root/closed-{index}.rs");
        assert_eq!(harness.pull_document(&uri, None)["error"]["code"], -32802);
        harness.runtime.complete_next();
        let result_id = assert_full(&harness.pull_document(&uri, None), index as u64 + 1, 0);
        assert!(
            harness.session.diagnostics.document_snapshots.len()
                <= MAX_WORKSPACE_DIAGNOSTIC_RESULTS
        );
        assert!(harness.session.diagnostics.published.len() <= MAX_WORKSPACE_DIAGNOSTIC_RESULTS);
        latest = Some((uri, result_id));
    }
    let (uri, result_id) = latest.unwrap();
    assert_eq!(
        harness.pull_document(&uri, Some(&result_id))["result"]["kind"],
        "unchanged"
    );
    assert_eq!(
        harness.runtime.started(),
        MAX_WORKSPACE_DIAGNOSTIC_RESULTS + 1
    );
}

fn assert_incomplete_refresh_replaces_old_snapshot(failure: &'static str, field: &str) {
    let mut harness = Harness::new(false);
    harness.open(1);
    harness.start_refresh();
    harness.runtime.complete_next();
    let old_result = assert_full(&harness.pull(None), 1, 1);

    *harness.authority.next_failure.lock().unwrap() = Some(failure);
    harness.notify(
        "textDocument/didSave",
        json!({ "textDocument": { "uri": DOCUMENT } }),
    );
    harness.runtime.complete_next();
    let failed = harness.pull(Some(&old_result));
    assert_eq!(failed["error"]["code"], -32802, "{failed}");
    assert_eq!(failed["error"]["data"][field], failure);
    assert!(failed.get("result").is_none());
    let repeated = harness.pull(Some(&old_result));
    assert_eq!(repeated["error"]["code"], -32802, "{repeated}");
    assert!(repeated["error"]["data"]["operationId"].as_str().is_some());
    assert!(repeated.get("result").is_none());
    assert_eq!(
        harness.runtime.started(),
        3,
        "the observed failure admits one coalesced retry"
    );
    harness.runtime.complete_next();
    let result_id = assert_full(&harness.pull(Some(&old_result)), 3, 1);
    assert_eq!(
        harness.pull(Some(&result_id))["result"]["kind"],
        "unchanged"
    );
    assert_eq!(harness.runtime.started(), 3);
}

#[test]
fn partial_refreshes_do_not_restore_the_previous_complete_snapshot() {
    assert_incomplete_refresh_replaces_old_snapshot(
        "diagnostic-broker-coverage-incomplete",
        "coverage",
    );
}

#[test]
fn failed_refreshes_do_not_restore_the_previous_complete_snapshot() {
    assert_incomplete_refresh_replaces_old_snapshot("provider-unavailable", "failureClass");
}

#[test]
fn publication_backpressure_does_not_discard_a_completed_snapshot() {
    let mut harness = Harness::new(true);
    harness.open(1);
    harness.start_refresh();
    harness.runtime.complete_next();
    for _ in 0..MAX_QUEUED_OUTBOUND_MESSAGES {
        if !harness.session.enqueue_value(json!({
            "jsonrpc": "2.0", "method": "window/logMessage",
            "params": { "type": 3, "message": "transport occupied" },
        })) {
            break;
        }
    }
    assert!(
        !harness
            .session
            .has_outbound_capacity(crate::MAX_PUBLICATION_BYTES)
    );
    harness.session.poll_diagnostic_refresh(DOCUMENT);
    harness.messages();

    assert_full(&harness.pull(None), 1, 1);
    assert_eq!(harness.runtime.started(), 1);
}

#[test]
fn shutdown_releases_ready_snapshots_and_retires_late_completions() {
    let mut harness = Harness::new(false);
    harness.open(1);
    harness.start_refresh();
    harness.runtime.complete_next();
    assert_full(&harness.pull(None), 1, 1);

    let pending_uri = "file:///root/pending.rs";
    let root = harness.session.root().clone();
    assert_eq!(
        harness.pull_document(pending_uri, None)["error"]["code"],
        -32802
    );
    assert_eq!(harness.session.diagnostics.document_snapshots.len(), 1);
    assert_eq!(harness.session.diagnostics.active_refreshes.len(), 1);
    let shutdown = harness.request("shutdown", Value::Null);
    assert!(shutdown.get("error").is_none(), "{shutdown}");
    assert_eq!(shutdown.get("result"), Some(&Value::Null));
    assert!(harness.session.diagnostics.document_snapshots.is_empty());
    assert!(harness.session.diagnostics.active_refreshes.is_empty());
    assert!(harness.session.diagnostics.published.is_empty());

    harness.runtime.complete_next();
    harness.session.flush_due(harness.now_ms);
    assert_eq!(harness.messages(), Vec::<Value>::new());
    assert!(matches!(
        harness.session.diagnostics.provider.adapter.document_diagnostics(&root, pending_uri, None),
        DiagnosticSnapshotOutcome::Partial { coverage, .. } if coverage == "refresh-required"
    ));
    assert!(harness.session.diagnostics.document_snapshots.is_empty());
    assert_eq!(harness.runtime.started(), 2);
}

#[test]
fn oversized_diagnostic_codes_refuse_then_recover_with_fresh_work() {
    let mut harness = Harness::new(false);
    harness.open(1);
    harness.start_refresh();
    *harness.authority.next_code.lock().unwrap() =
        Some("x".repeat(MAX_WORKSPACE_DIAGNOSTIC_BYTES + 1));
    harness.runtime.complete_next();

    let refused = harness.pull(None);
    assert_eq!(refused["error"]["code"], -32802, "{refused}");
    assert_eq!(
        refused["error"]["data"]["failureClass"],
        "diagnostic-result-byte-capacity",
    );
    assert!(refused.get("result").is_none());
    let pending = harness.pull(None);
    assert_eq!(pending["error"]["code"], -32802, "{pending}");
    assert!(pending["error"]["data"]["operationId"].as_str().is_some());
    assert!(pending.get("result").is_none());
    assert!(harness.session.diagnostics.published.is_empty());
    assert_eq!(harness.runtime.started(), 2);
    harness.runtime.complete_next();
    let result_id = assert_full(&harness.pull(None), 2, 1);
    assert_eq!(
        harness.pull(Some(&result_id))["result"]["kind"],
        "unchanged"
    );
    assert_eq!(harness.runtime.started(), 2);
}

fn assert_completed_refresh_is_not_polled_again(harness: &mut Harness) {
    assert!(harness.session.diagnostics.active_refreshes.is_empty());
    let reads = harness
        .session
        .diagnostics
        .provider
        .reads
        .load(Ordering::SeqCst);
    harness.session.flush_due(harness.now_ms);
    harness.session.flush_due(harness.now_ms + 1);
    assert_eq!(
        harness
            .session
            .diagnostics
            .provider
            .reads
            .load(Ordering::SeqCst),
        reads
    );
    assert_eq!(harness.messages(), Vec::<Value>::new());
}

#[test]
fn an_identical_same_generation_publication_retires_the_completed_refresh() {
    let mut harness = Harness::new(true);
    harness.open(1);
    harness.start_refresh();
    harness.runtime.complete_next();
    let result_id = assert_full(&harness.pull(None), 1, 1);

    *harness.authority.next_generation.lock().unwrap() = Some(1);
    harness.notify(
        "textDocument/didSave",
        json!({ "textDocument": { "uri": DOCUMENT } }),
    );
    harness.runtime.complete_next();
    harness.session.flush_due(harness.now_ms);
    assert!(
        harness.messages().is_empty(),
        "an acknowledged identical report is not republished"
    );
    assert_completed_refresh_is_not_polled_again(&mut harness);
    assert_eq!(
        harness.pull(Some(&result_id))["result"]["kind"],
        "unchanged"
    );
    assert_eq!(harness.runtime.started(), 2);
}

#[test]
fn a_report_too_large_for_push_remains_available_to_pull_without_polling_again() {
    let mut harness = Harness::new(true);
    harness.open(1);
    harness.start_refresh();
    let code = "x".repeat(300 * 1024);
    *harness.authority.next_code.lock().unwrap() = Some(code.clone());
    harness.runtime.complete_next();
    harness.session.flush_due(harness.now_ms);
    assert!(
        harness.messages().is_empty(),
        "the oversized push must be refused"
    );
    assert_completed_refresh_is_not_polled_again(&mut harness);

    let full = harness.pull(None);
    assert_eq!(full["result"]["kind"], "full", "{full}");
    let mut expected = expected_items(1, 1);
    expected[0]["code"] = json!(code);
    assert_eq!(full["result"]["items"], expected);
    let result_id = full["result"]["resultId"].as_str().unwrap();
    assert_eq!(harness.pull(Some(result_id))["result"]["kind"], "unchanged");
    assert_eq!(harness.runtime.started(), 1);
}

#[test]
fn open_document_snapshots_refuse_new_results_when_the_bound_is_owned() {
    let mut harness = Harness::new(false);
    for index in 0..MAX_WORKSPACE_DIAGNOSTIC_RESULTS {
        let uri = format!("file:///root/open-{index}.rs");
        harness.notify("textDocument/didOpen", json!({
            "textDocument": { "uri": uri, "languageId": "rust", "version": 1, "text": "fn a() {}" },
        }));
        assert_eq!(harness.pull_document(&uri, None)["error"]["code"], -32802);
        harness.runtime.complete_next();
        assert_full(&harness.pull_document(&uri, None), index as u64 + 1, 1);
    }

    let refused = harness.pull(None);
    assert_eq!(refused["error"]["code"], -32802, "{refused}");
    assert_eq!(
        refused["error"]["data"]["failureClass"],
        "diagnostic-result-capacity"
    );
    assert!(refused.get("result").is_none());
    assert_eq!(
        harness.session.diagnostics.document_snapshots.len(),
        MAX_WORKSPACE_DIAGNOSTIC_RESULTS
    );
    assert_eq!(harness.runtime.started(), MAX_WORKSPACE_DIAGNOSTIC_RESULTS);
}
